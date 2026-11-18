//! The connection to Mace: the account's supervisor serves a Unix socket
//! (`MACE_PANEL`) and the panel keeps one connection to it, reconnecting when
//! it drops. Everything the panel shows arrives here; everything it does is
//! sent from here.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mace_proto::{Config, ServerPage, State, ToMace, ToPanel};

use crate::{detail, pace, panel};

/// An account's picture: size and RGBA pixels.
pub type Avatar = Arc<(u32, Vec<u8>)>;

#[derive(Default)]
pub struct Data {
    pub connected: bool,
    pub config: Option<Config>,
    pub state: Option<Arc<State>>,
    /// Pages of servers not yet taken by the UI.
    pub pages: Vec<ServerPage>,
    pub avatars: HashMap<String, Avatar>,
    /// Answers to `Run` requests not yet taken by the UI.
    pub done: Vec<(u64, bool, String)>,
    /// Toasts not yet taken by the UI.
    pub toasts: Vec<(String, String)>,
}

pub static DATA: Mutex<Option<Data>> = Mutex::new(None);
static STREAM: Mutex<Option<UnixStream>> = Mutex::new(None);
static STARTED: AtomicBool = AtomicBool::new(false);

pub fn with<T>(f: impl FnOnce(&mut Data) -> T) -> T {
    let mut data = DATA.lock().unwrap();
    f(data.get_or_insert_with(Data::default))
}

/// Connect to Mace (once per process) and keep connected.
pub fn start(path: String, hello: ToMace) {
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let _ = std::thread::Builder::new().name("mace-panel".into()).spawn(move || run(path, hello));
}

fn run(path: String, hello: ToMace) {
    let mut wait = Duration::from_millis(250);
    loop {
        if let Ok(stream) = UnixStream::connect(&path) {
            wait = Duration::from_millis(250);
            let _ = stream.set_write_timeout(Some(Duration::from_millis(250)));
            if let Ok(writer) = stream.try_clone() {
                *STREAM.lock().unwrap() = Some(writer);
                with(|d| d.connected = true);
                send(&hello);
                detail!("connected to Mace");
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { break };
                    match serde_json::from_str::<ToPanel>(&line) {
                        Ok(message) => receive(message),
                        Err(e) => detail!("a message from Mace I don't understand: {e}"),
                    }
                }
                *STREAM.lock().unwrap() = None;
                with(|d| d.connected = false);
                detail!("Mace went away; trying again");
            }
        }
        std::thread::sleep(wait);
        wait = (wait * 2).min(Duration::from_secs(5));
    }
}

fn receive(message: ToPanel) {
    match message {
        ToPanel::Config(config) => {
            panel::INPUT.set_hotkey(config.key);
            pace::configure(config.background_fps, config.anti_afk);
            with(|d| d.config = Some(config));
        }
        ToPanel::State(state) => with(|d| d.state = Some(Arc::new(state))),
        ToPanel::Servers(page) => with(|d| {
            if d.pages.len() < 16 {
                d.pages.push(page);
            }
        }),
        ToPanel::Avatar { user, size, rgba } => {
            if let Some(pixels) = mace_proto::unbase64(&rgba)
                && pixels.len() == (size * size * 4) as usize
            {
                with(|d| d.avatars.insert(user, Arc::new((size, pixels))));
            }
        }
        ToPanel::Done { id, ok, error } => with(|d| {
            if d.done.len() < 64 {
                d.done.push((id, ok, error));
            }
        }),
        ToPanel::Toggle => panel::INPUT.toggle(),
        ToPanel::Toast { title, body } => {
            with(|d| {
                if d.toasts.len() < 16 {
                    d.toasts.push((title, body));
                }
            });
            panel::toast_arrived();
        }
    }
}

/// Send to Mace; false when not connected.
pub fn send(message: &ToMace) -> bool {
    if let Some(file) = FAKE.get() {
        return fake_send(file, message);
    }
    let mut stream = STREAM.lock().unwrap();
    let Some(s) = stream.as_mut() else { return false };
    if s.write_all(message.line().as_bytes()).is_err() {
        *stream = None;
        return false;
    }
    true
}

/// Tests: Mace's messages from a file, and the server pages in it given out on request.
static FAKE: std::sync::OnceLock<(String, Vec<ServerPage>)> = std::sync::OnceLock::new();

pub fn start_fake(file: String) {
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() - 720).unwrap_or(0);
    let mut pages = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str::<ToPanel>(&line.replace("NOW", &now.to_string())) {
            Ok(ToPanel::Servers(page)) => pages.push(page),
            Ok(message) => receive(message),
            Err(e) => crate::note!("fake line not understood: {e}"),
        }
    }
    with(|d| d.connected = true);
    let _ = FAKE.set((format!("{file}.sent"), pages));
}

fn fake_send(file: &(String, Vec<ServerPage>), message: &ToMace) -> bool {
    if let Ok(mut out) = std::fs::OpenOptions::new().create(true).append(true).open(&file.0) {
        let _ = out.write_all(message.line().as_bytes());
    }
    if let ToMace::Servers { place, sort, cursor } = message
        && let Some(page) = file.1.iter().find(|p| p.place == *place && p.sort == *sort && p.cursor == *cursor)
    {
        receive(ToPanel::Servers(page.clone()));
    }
    true
}

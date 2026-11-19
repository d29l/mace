//! Mace's side of the in-game panel.
//!
//! The panel is drawn inside Roblox by a Vulkan layer (the `layer/` crate)
//! that Mace turns on for the games it starts: `env` adds it to Roblox's
//! environment once `preflight` agrees it will load with that Rosemary. The
//! account's supervisor then serves a Unix socket the panel connects to
//! (`Server`): it sends the panel what to show and runs what the panel asks
//! for, as JSON lines (`mace_proto`).

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime};

use mace_proto::{Config, Key, State, Theme, ToMace, ToPanel};
use serde_json::{Map, Value};

use crate::accounts::{self, Account};
use crate::servers::LogFollower;
use crate::{cli, fsutil, launch, paths, processes, recent, servers, settings, supervisor, upgrade};

pub const LAYER_NAME: &str = "VK_LAYER_MACE_panel";
pub const LIBRARY: &str = "libmace_panel.so";

pub fn socket_path(account_id: &str) -> PathBuf {
    paths::runtime().join(format!("panel-{account_id}.sock"))
}

/// The layer's library: `MACE_PANEL_LAYER`, else next to this program (a
/// cargo build), else the installed copy.
pub fn library() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("MACE_PANEL_LAYER").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    if cfg!(test) {
        return None;
    }
    let beside = std::env::current_exe().ok()?.parent()?.join(LIBRARY);
    [beside, paths::app().join("lib").join(LIBRARY)].into_iter().find(|p| p.is_file())
}

/// "2.44" → (2, 44)
fn version(text: &str) -> Option<(u32, u32)> {
    let mut parts = text.trim().split('.');
    Some((parts.next()?.parse().ok()?, parts.next().unwrap_or("0").parse().ok()?))
}

fn host_glibc() -> Option<(u32, u32)> {
    // SAFETY: glibc returns a static string.
    let text = unsafe { std::ffi::CStr::from_ptr(libc::gnu_get_libc_version()) };
    version(&text.to_string_lossy())
}

/// The glibc Rosemary brings (`required_glibc` in its bundle.conf), if it says.
fn rosemary_glibc(runner: &Path) -> Option<(u32, u32)> {
    let bin = runner.parent()?;
    for conf in [bin.join("../Rosemary/share/rosemary/bundle.conf"), bin.join("../share/rosemary/bundle.conf")] {
        if let Ok(text) = std::fs::read_to_string(&conf) {
            return text.lines().find_map(|l| l.strip_prefix("required_glibc=")).and_then(version);
        }
    }
    None
}

/// What an ELF library needs: the libraries it names and the newest glibc symbol version.
fn elf_needs(data: &[u8]) -> Result<(Vec<String>, (u32, u32)), String> {
    let u16_at = |o: usize| data.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |o: usize| data.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let u64_at = |o: usize| data.get(o..o + 8).map(|b| u64::from_le_bytes(b.try_into().unwrap()));
    if data.get(..4) != Some(b"\x7fELF") || data.get(4) != Some(&2) || data.get(5) != Some(&1) || u16_at(18) != Some(62) {
        return Err("not a 64-bit x86 library".into());
    }
    let bad = || "a damaged library".to_string();
    let (shoff, shentsize, shnum) = (u64_at(0x28).ok_or_else(bad)? as usize, u16_at(0x3a).ok_or_else(bad)? as usize,
                                     u16_at(0x3c).ok_or_else(bad)? as usize);
    let section = |i: usize| -> Option<(u32, usize, usize, usize)> {
        let at = shoff + i * shentsize;
        Some((u32_at(at + 4)?, u64_at(at + 24)? as usize, u64_at(at + 32)? as usize, u32_at(at + 40)? as usize))
    };
    let string = |table: usize, at: usize| -> Option<String> {
        let (_, off, size, _) = section(table)?;
        let bytes = data.get(off + at..off + size)?;
        let end = bytes.iter().position(|&b| b == 0)?;
        Some(String::from_utf8_lossy(&bytes[..end]).into_owned())
    };
    let (mut needed, mut newest) = (Vec::new(), (0, 0));
    for i in 0..shnum {
        let Some((kind, off, size, link)) = section(i) else { return Err(bad()) };
        match kind {
            // SHT_DYNAMIC: DT_NEEDED entries
            6 => {
                for entry in (off..off + size).step_by(16) {
                    match u64_at(entry) {
                        Some(1) => needed.push(string(link, u64_at(entry + 8).ok_or_else(bad)? as usize).ok_or_else(bad)?),
                        Some(0) | None => break,
                        _ => {}
                    }
                }
            }
            // SHT_GNU_verneed: the symbol versions each library must provide
            0x6fff_fffe => {
                let mut at = off;
                loop {
                    let (count, aux, next) = (u16_at(at + 2).ok_or_else(bad)?, u32_at(at + 8).ok_or_else(bad)?,
                                              u32_at(at + 12).ok_or_else(bad)?);
                    let mut a = at + aux as usize;
                    for _ in 0..count {
                        let name = string(link, u32_at(a + 8).ok_or_else(bad)? as usize).unwrap_or_default();
                        if let Some(v) = name.strip_prefix("GLIBC_").and_then(version) {
                            newest = newest.max(v);
                        }
                        let step = u32_at(a + 12).ok_or_else(bad)?;
                        if step == 0 {
                            break;
                        }
                        a += step as usize;
                    }
                    if next == 0 {
                        break;
                    }
                    at += next as usize;
                }
            }
            _ => {}
        }
    }
    Ok((needed, newest))
}

/// The layer library to use with this Rosemary, or why the panel can't be had.
/// A layer that can't load would only mean no panel (the Vulkan loader skips
/// it), but saying so beats a panel that silently never appears.
pub fn preflight(runner: &Path) -> Result<PathBuf, String> {
    let library = library().ok_or("the panel's library isn't installed")?;
    let data = std::fs::read(&library).map_err(|e| format!("can't read {}: {e}", library.display()))?;
    let (needed, wants) = elf_needs(&data)?;
    const SYSTEM: [&str; 6] = ["libc.so.6", "libm.so.6", "libgcc_s.so.1", "ld-linux-x86-64.so.2", "libdl.so.2", "libpthread.so.0"];
    if let Some(other) = needed.iter().find(|n| !SYSTEM.contains(&n.as_str())) {
        return Err(format!("the panel library needs {other}"));
    }
    for (whose, has) in [("the system", host_glibc()), ("Rosemary", rosemary_glibc(runner))] {
        if let Some(has) = has
            && wants > has
        {
            return Err(format!("the panel needs glibc {}.{}, {whose} has {}.{}; rebuild Mace",
                               wants.0, wants.1, has.0, has.1));
        }
    }
    Ok(library)
}

/// The layer's manifest, written where the Vulkan loader is pointed (the runtime dir).
fn write_manifest(library: &Path) -> anyhow::Result<PathBuf> {
    let dir = paths::runtime().join("vulkan");
    std::fs::create_dir_all(&dir)?;
    let manifest = serde_json::json!({
        "file_format_version": "1.2.0",
        "layer": {
            "name": LAYER_NAME,
            "type": "GLOBAL",
            "library_path": library,
            "api_version": "1.3.0",
            "implementation_version": "1",
            "description": "Mace's in-game panel",
        }
    });
    fsutil::write_atomic(&dir.join("VkLayer_mace_panel.json"), manifest.to_string().as_bytes())?;
    Ok(dir)
}

/// Add `item` to a `:`-separated list once.
fn add_to_list(env: &mut launch::Env, name: &str, item: &str, first: bool) {
    let mut items: Vec<String> = env.get(name).map(|v| v.split(':').filter(|s| !s.is_empty() && *s != item)
        .map(String::from).collect()).unwrap_or_default();
    if first { items.insert(0, item.to_string()) } else { items.push(item.to_string()) }
    env.insert(name.to_string(), items.join(":"));
}

/// Turn the panel on in Roblox's environment. Errors say why it stays off.
pub fn add_env(env: &mut launch::Env, account: &Account, runner: &Path) -> Result<(), String> {
    let library = preflight(runner)?;
    let dir = write_manifest(&library).map_err(|e| format!("can't write the panel's manifest: {e}"))?;
    add_to_list(env, "VK_ADD_LAYER_PATH", &dir.to_string_lossy(), true);
    add_to_list(env, "VK_INSTANCE_LAYERS", LAYER_NAME, false);
    env.insert("MACE_PANEL".into(), socket_path(&account.id).to_string_lossy().into_owned());
    Ok(())
}

// -- what the panel is told

/// "INSERT" or "SHIFT + F1" → the key by physical position (SDL scancodes) and modifiers.
pub fn parse_key(text: &str) -> Option<Key> {
    let mut mods = 0;
    let mut scancode = None;
    for word in text.split('+').map(|w| w.trim().to_ascii_uppercase()).filter(|w| !w.is_empty()) {
        match word.as_str() {
            "SUPER" | "WIN" | "META" | "LOGO" | "GUI" | "MOD4" => mods |= mace_proto::MOD_SUPER,
            "CTRL" | "CONTROL" => mods |= mace_proto::MOD_CTRL,
            "ALT" => mods |= mace_proto::MOD_ALT,
            "SHIFT" => mods |= mace_proto::MOD_SHIFT,
            key => scancode = Some(scancode_of(key)?),
        }
    }
    Some(Key { scancode: scancode?, mods })
}

fn scancode_of(key: &str) -> Option<u32> {
    let bytes = key.as_bytes();
    if bytes.len() == 1 {
        let c = bytes[0];
        return match c {
            b'A'..=b'Z' => Some(4 + (c - b'A') as u32),
            b'1'..=b'9' => Some(30 + (c - b'1') as u32),
            b'0' => Some(39),
            b'-' => Some(45),
            b'=' => Some(46),
            b'[' => Some(47),
            b']' => Some(48),
            b'\\' => Some(49),
            b';' => Some(51),
            b'\'' => Some(52),
            b'`' => Some(53),
            b',' => Some(54),
            b'.' => Some(55),
            b'/' => Some(56),
            _ => None,
        };
    }
    if let Some(n) = key.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()) {
        return match n {
            1..=12 => Some(57 + n),
            13..=24 => Some(91 + n),
            _ => None,
        };
    }
    Some(match key {
        "RETURN" | "ENTER" => 40,
        "ESCAPE" | "ESC" => 41,
        "BACKSPACE" => 42,
        "TAB" => 43,
        "SPACE" => 44,
        "GRAVE" => 53,
        "INSERT" => 73,
        "HOME" => 74,
        "PAGEUP" | "PRIOR" => 75,
        "DELETE" => 76,
        "END" => 77,
        "PAGEDOWN" | "NEXT" => 78,
        "PAUSE" => 72,
        "SCROLLLOCK" => 71,
        _ => return None,
    })
}

/// The panel's colours: plain greys, or Material roles from ~/.config/mace/theme.json.
pub fn theme() -> Theme {
    let roles = fsutil::read_json(&paths::config().join("theme.json")).unwrap_or(Value::Null);
    let d = Theme::default();
    let pick = |role: &str, fallback: &String| {
        roles.get(role).and_then(Value::as_str).filter(|s| s.starts_with('#') && s.len() >= 7)
            .map(String::from).unwrap_or_else(|| fallback.clone())
    };
    Theme {
        background: pick("surface_container", &d.background),
        surface: pick("surface_container_high", &d.surface),
        surface_hover: pick("surface_container_highest", &d.surface_hover),
        border: pick("outline_variant", &d.border),
        text: pick("on_surface", &d.text),
        subtext: pick("on_surface_variant", &d.subtext),
        accent: pick("primary", &d.accent),
        on_accent: pick("on_primary", &d.on_accent),
        danger: pick("error", &d.danger),
    }
}

/// Where the account whose Roblox window was focused last is written down, so
/// every other account's game knows it runs in the background.
pub fn focused_file() -> PathBuf {
    paths::runtime().join("focused")
}

/// The account whose Roblox window had focus last, if any did.
pub fn last_focused() -> Option<String> {
    std::fs::read_to_string(focused_file()).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn config(me: &str) -> Config {
    let s = settings::load();
    // the game you used last keeps its full frame rate even while you're in another app
    let background_fps = if last_focused().as_deref() == Some(me) { 0 } else { s.background_fps.min(u32::MAX as u64) as u32 };
    Config { me: me.to_string(), key: parse_key(&s.overlay_key).unwrap_or_default(), theme: theme(),
             can_focus: *processes::HYPRLAND, blur: s.panel_blur, background_fps, anti_afk: s.anti_afk }
}

/// Recent games the in-game bar lists.
const GAMES_SHOWN: usize = 8;

/// Every account, as the panel shows them.
pub fn state(followers: &mut HashMap<String, LogFollower>) -> State {
    let everyone = accounts::accounts();
    let snapshot = processes::scan();
    let live = processes::running(&everyone, Some(&snapshot));
    let inputs = if live.is_empty() { Vec::new() } else { processes::sink_inputs() };
    let mut rows = Vec::new();
    for account in &everyone {
        let who = account.identity();
        let running = live.contains_key(&account.id);
        let server = if running {
            let follower = followers.entry(account.id.clone()).or_insert_with(|| LogFollower::new(&account.logs()));
            follower.poll();
            if follower.server.contains_key("ip") { Some(servers::describe(&follower.server)) } else { None }
        } else {
            None
        };
        let text = |m: &Map<String, Value>, k: &str| m.get(k).and_then(Value::as_str).map(String::from);
        rows.push(mace_proto::Account {
            id: account.id.clone(),
            name: account.name_with(&who),
            username: who.username.clone(),
            user: (!who.is_empty()).then(|| who.id.clone()),
            fps: supervisor::read_fps(&account.id).map(|f| f as u32),
            launch: supervisor::read_status(&account.id).get("state").and_then(Value::as_str).map(String::from),
            running,
            muted: if running { processes::muted(account, Some(&snapshot), Some(&inputs)) } else { None },
            server: server.map(|s| mace_proto::Server {
                place: text(&s, "place").unwrap_or_default(),
                job: text(&s, "job").unwrap_or_default(),
                link: text(&s, "link").unwrap_or_default(),
                name: text(&s, "name"),
                location: text(&s, "location"),
                since: s.get("since").and_then(Value::as_f64),
            }),
        });
    }
    followers.retain(|id, _| live.contains_key(id));
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    let games = recent::load().into_iter().take(GAMES_SHOWN).map(|g| mace_proto::Game {
        universe: g.universe, place: g.place, name: g.name, account: g.account, when: g.when,
    }).collect();
    State { now, accounts: rows, games }
}

/// An avatar as square RGBA pixels, at most `size` wide.
pub fn avatar_rgba(path: &Path, size: u32) -> Option<(u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).ok()?));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buffer).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let channels = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Grayscale => 1,
        png::ColorType::Indexed => return None,
    };
    let side = w.min(h);
    let out = (size as usize).min(side).max(1);
    let mut rgba = Vec::with_capacity(out * out * 4);
    for y in 0..out {
        for x in 0..out {
            // box filter over the source pixels this one covers
            let (x0, x1) = (x * side / out, ((x + 1) * side / out).max(x * side / out + 1));
            let (y0, y1) = (y * side / out, ((y + 1) * side / out).max(y * side / out + 1));
            let mut sum = [0u32; 4];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let p = &buffer[(sy * w + sx) * channels..][..channels];
                    let px = match channels {
                        4 => [p[0], p[1], p[2], p[3]],
                        3 => [p[0], p[1], p[2], 255],
                        2 => [p[0], p[0], p[0], p[1]],
                        _ => [p[0], p[0], p[0], 255],
                    };
                    for i in 0..4 {
                        sum[i] += px[i] as u32;
                    }
                }
            }
            let n = ((x1 - x0) * (y1 - y0)) as u32;
            rgba.extend(sum.iter().map(|s| (s / n) as u8));
        }
    }
    Some((out as u32, rgba))
}

// -- the socket

/// What the panel may ask Mace to do.
const ALLOWED: [&str; 6] = ["start", "stop", "mute", "rejoin", "gather", "focus"];

fn run(args: &[String]) -> (bool, String) {
    let Some(name) = args.first() else { return (false, "no command".into()) };
    if !ALLOWED.contains(&name.as_str()) {
        return (false, format!("not allowed from the panel: {name}"));
    }
    let mut args = args[1..].to_vec();
    if name == "start" {
        // starts from the panel don't count as a choice of account for roblox.com's links
        args.push("--no-remember".into());
    }
    let Some(command) = cli::command(name) else { return (false, format!("no command {name}")) };
    match command(&args, &mut std::io::sink()) {
        Ok(()) => (true, String::new()),
        Err(error) => (false, format!("{error:#}")),
    }
}

type Writer = Arc<Mutex<UnixStream>>;

fn send(writer: &Writer, message: &ToPanel) -> bool {
    writer.lock().unwrap().write_all(message.line().as_bytes()).is_ok()
}

struct Shared {
    me: String,
    /// Every connection that said hello (the panels): toggles and toasts go to them.
    panels: Mutex<Vec<Writer>>,
    followers: Mutex<HashMap<String, LogFollower>>,
    /// The server is gone: its threads stop.
    stopped: AtomicBool,
}

impl Shared {
    /// To every panel; false when there was none to send to.
    fn to_panels(&self, message: &ToPanel) -> bool {
        let panels = self.panels.lock().unwrap().clone();
        let mut any = false;
        for panel in panels {
            any |= send(&panel, message);
        }
        any
    }
}

/// The panel socket for one account, served while its supervisor runs.
pub struct Server {
    path: PathBuf,
    shared: Arc<Shared>,
}

impl Server {
    /// Serve the account's panel socket, unless another supervisor already does.
    pub fn start(account: &Account) -> Option<Server> {
        let path = socket_path(&account.id);
        // the supervisor of a client this one replaces may still be finishing: wait for it to let go
        let deadline = std::time::Instant::now() + Duration::from_millis(if cfg!(test) { 300 } else { 15_000 });
        while UnixStream::connect(&path).is_ok() {
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::create_dir_all(paths::runtime());
        let listener = UnixListener::bind(&path).ok()?;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        let shared = Arc::new(Shared { me: account.id.clone(), panels: Mutex::new(Vec::new()),
                                       followers: Mutex::new(HashMap::new()), stopped: AtomicBool::new(false) });
        {
            let shared = shared.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let shared = shared.clone();
                    std::thread::spawn(move || connection(stream, shared));
                }
            });
        }
        {
            let shared = shared.clone();
            std::thread::spawn(move || watch_joins(shared));
        }
        {
            let shared = shared.clone();
            std::thread::spawn(move || tell_about_update(&shared));
        }
        {
            let shared = shared.clone();
            std::thread::spawn(move || crate::discord::run(&shared.me, &shared.stopped));
        }
        Some(Server { path, shared })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::Relaxed);
        let _ = std::fs::remove_file(&self.path);
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    path.metadata().and_then(|m| m.modified()).ok()
}

/// A toast when a newer Mace is out: a minute into the game (after the join's
/// own), then asked again every hour, and told again only for a newer one.
fn tell_about_update(shared: &Shared) {
    let mut told: Option<String> = None;
    let mut wait = Duration::from_secs(60);
    loop {
        let until = std::time::Instant::now() + wait;
        while std::time::Instant::now() < until {
            if shared.stopped.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_secs(5));
        }
        wait = upgrade::EVERY;
        let Some(version) = upgrade::available().filter(|v| told.as_deref() != Some(v.as_str())) else { continue };
        let toast = ToPanel::Toast { title: format!("Mace {version} is out"), body: "Open Mace Settings to update".into() };
        if shared.to_panels(&toast) {
            told = Some(version);
        } else {
            // no panel to show it yet
            wait = Duration::from_secs(60);
        }
    }
}

/// "Joined Flex Your FPS" / "Server in Frankfurt am Main, Hesse, DE".
fn join_toast(server: &Map<String, Value>) -> ToPanel {
    let info = servers::describe(server);
    let title = info.get("name").and_then(Value::as_str).map_or_else(|| "Joined server".into(), |n| format!("Joined {n}"));
    let body = info.get("location").and_then(Value::as_str)
        .map(String::from).unwrap_or_default();
    ToPanel::Toast { title, body }
}

/// Joins worth a toast in this account's game: its own (which game, and where
/// the server is), and other accounts arriving in its server.
fn watch_joins(shared: Arc<Shared>) {
    let mut followers: HashMap<String, LogFollower> = HashMap::new();
    let mut names: HashMap<String, String> = HashMap::new();
    let mut fresh: HashSet<String> = HashSet::new();
    let mut next_accounts = std::time::Instant::now();
    while !shared.stopped.load(Ordering::Relaxed) {
        if std::time::Instant::now() >= next_accounts {
            next_accounts = std::time::Instant::now() + Duration::from_secs(5);
            for account in accounts::accounts() {
                names.insert(account.id.clone(), account.name());
                followers.entry(account.id.clone()).or_insert_with(|| {
                    fresh.insert(account.id.clone()); // what's logged already isn't news
                    LogFollower::new(&account.logs())
                });
            }
        }
        // this account's log first: an alt joining in the same pass is matched against where it is now
        let mut ids: Vec<String> = followers.keys().cloned().collect();
        ids.sort_by_key(|id| *id != shared.me);
        let mut here = None;
        for id in ids {
            let Some(follower) = followers.get_mut(&id) else { continue };
            let joined = follower.poll();
            if id == shared.me {
                here = follower.server.get("job").and_then(Value::as_str).map(String::from);
            }
            if fresh.remove(&id) {
                continue;
            }
            for server in joined {
                let shared = shared.clone();
                if id == shared.me {
                    // the game's name and the server's place may take a lookup
                    std::thread::spawn(move || {
                        shared.to_panels(&join_toast(&server));
                        if let Err(error) = recent::record(&shared.me, &server) {
                            supervisor::note_for(&shared.me, &format!("couldn't note the game played: {error:#}"));
                        }
                    });
                } else if here.is_some() && server.get("job").and_then(Value::as_str) == here.as_deref() {
                    let who = names.get(&id).cloned().unwrap_or_else(|| id.clone());
                    shared.to_panels(&ToPanel::Toast { title: format!("{who} joined your server"), body: String::new() });
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn connection(stream: UnixStream, shared: Arc<Shared>) {
    let Ok(writer) = stream.try_clone() else { return };
    let _ = writer.set_write_timeout(Some(Duration::from_secs(2)));
    let writer: Writer = Arc::new(Mutex::new(writer));
    let open = Arc::new(AtomicBool::new(true));
    let me = shared.me.clone();

    // the state (and new avatars), worked out on one thread; asks while it's busy fold into one
    let wanted = Arc::new((Mutex::new(false), Condvar::new()));
    {
        let (wanted, writer, open, shared) = (wanted.clone(), writer.clone(), open.clone(), shared.clone());
        std::thread::spawn(move || {
            let mut sent_avatars: HashSet<String> = HashSet::new();
            while open.load(Ordering::Relaxed) {
                {
                    let (lock, signal) = &*wanted;
                    let mut pending = lock.lock().unwrap();
                    while !*pending {
                        let (next, timeout) = signal.wait_timeout(pending, Duration::from_secs(1)).unwrap();
                        pending = next;
                        if timeout.timed_out() && !open.load(Ordering::Relaxed) {
                            return;
                        }
                    }
                    *pending = false;
                }
                let state = state(&mut shared.followers.lock().unwrap());
                if !send(&writer, &ToPanel::State(state.clone())) {
                    return;
                }
                for user in state.accounts.iter().filter_map(|a| a.user.clone()) {
                    if sent_avatars.contains(&user) {
                        continue;
                    }
                    if let Some((size, rgba)) = servers::cached_avatar(&user).and_then(|p| avatar_rgba(&p, 96)) {
                        sent_avatars.insert(user.clone());
                        send(&writer, &ToPanel::Avatar { user, size, rgba: mace_proto::base64(&rgba) });
                    }
                }
                for universe in state.games.iter().map(|g| g.universe.clone()) {
                    let key = format!("game:{universe}");
                    if sent_avatars.contains(&key) {
                        continue;
                    }
                    if let Some((size, rgba)) = recent::icon(&universe).and_then(|p| avatar_rgba(&p, 96)) {
                        sent_avatars.insert(key.clone());
                        send(&writer, &ToPanel::Avatar { user: key, size, rgba: mace_proto::base64(&rgba) });
                    }
                }
            }
        });
    }
    let ask_state = {
        let wanted = wanted.clone();
        move || {
            let (lock, signal) = &*wanted;
            *lock.lock().unwrap() = true;
            signal.notify_one();
        }
    };
    // the key and colours, sent again when the settings or theme change (to panels only)
    let send_config = {
        let (writer, open, me) = (writer.clone(), open.clone(), me.clone());
        move || {
            std::thread::spawn(move || {
                let files = [paths::settings_file(), paths::config().join("theme.json"), focused_file()];
                let mut seen: Vec<Option<SystemTime>> = Vec::new();
                while open.load(Ordering::Relaxed) {
                    let now: Vec<_> = files.iter().map(|f| modified(f)).collect();
                    if now != seen {
                        seen = now;
                        if !send(&writer, &ToPanel::Config(config(&me))) {
                            return;
                        }
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
            });
        }
    };
    let mut send_config = Some(send_config);

    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<ToMace>(&line) else { continue };
        match message {
            ToMace::Hello { pid, video, .. } => {
                shared.panels.lock().unwrap().push(writer.clone());
                supervisor::note_for(&me, &format!("panel ready (pid {pid}, {video})"));
                if let Some(start) = send_config.take() {
                    start();
                }
                ask_state();
            }
            ToMace::State => ask_state(),
            ToMace::Servers { place, sort, cursor } => {
                let writer = writer.clone();
                std::thread::spawn(move || {
                    send(&writer, &ToPanel::Servers(servers::public_page(&place, sort, cursor.as_deref())));
                });
            }
            ToMace::Run { id, args } => {
                let (writer, ask_state) = (writer.clone(), ask_state.clone());
                std::thread::spawn(move || {
                    let (ok, error) = run(&args);
                    send(&writer, &ToPanel::Done { id, ok, error });
                    ask_state();
                });
            }
            ToMace::Event { what } if what == "focused" => {
                let _ = fsutil::write_atomic(&focused_file(), me.as_bytes());
            }
            ToMace::Event { .. } => {}
            ToMace::Toggle => {
                shared.to_panels(&ToPanel::Toggle);
            }
            ToMace::Toast { title, body } => {
                let ok = shared.to_panels(&ToPanel::Toast { title, body });
                send(&writer, &ToPanel::Done { id: 0, ok, error: String::new() });
            }
        }
    }
    open.store(false, Ordering::Relaxed);
    shared.panels.lock().unwrap().retain(|w| !Arc::ptr_eq(w, &writer));
}

/// `mace panel`: open or close an account's panel from outside the game (for a
/// desktop shortcut where the game can't see the key).
pub fn toggle(account: &Account) -> anyhow::Result<()> {
    let mut stream = UnixStream::connect(socket_path(&account.id))
        .map_err(|_| anyhow::anyhow!("{} has no in-game panel running", account.name()))?;
    stream.write_all(ToMace::Toggle.line().as_bytes())?;
    Ok(())
}

/// Show a toast in the account's game; false when it has no panel to show it.
pub fn toast(account_id: &str, title: &str, body: &str) -> bool {
    let Ok(mut stream) = UnixStream::connect(socket_path(account_id)) else { return false };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let message = ToMace::Toast { title: title.into(), body: body.into() };
    if stream.write_all(message.line().as_bytes()).is_err() {
        return false;
    }
    BufReader::new(stream).lines().map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<ToPanel>(&l).ok())
        .find_map(|m| match m {
            ToPanel::Done { id: 0, ok, .. } => Some(ok),
            _ => None,
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mace_proto::{MOD_CTRL, MOD_SHIFT, MOD_SUPER};

    #[test]
    fn the_game_used_last_keeps_its_frame_rate() {
        let _home = crate::testenv::TempHome::new();
        settings::save(&settings::Settings { background_fps: 12, ..Default::default() }).unwrap();
        // nobody focused yet: every game may be slowed while in the background
        assert_eq!(config("main").background_fps, 12);
        // anti-AFK only when chosen
        assert!(!config("main").anti_afk);
        settings::save(&settings::Settings { background_fps: 12, anti_afk: true, ..Default::default() }).unwrap();
        assert!(config("main").anti_afk);
        std::fs::create_dir_all(paths::runtime()).unwrap();
        fsutil::write_atomic(&focused_file(), b"alt1").unwrap();
        assert_eq!(config("alt1").background_fps, 0);
        assert_eq!(config("main").background_fps, 12);
        // a panel from before these settings still reads the config
        let old: Config = serde_json::from_str(r#"{"me":"main","key":{"scancode":73,"mods":0},"theme":{"background":"","surface":"","surface_hover":"","border":"","text":"","subtext":"","accent":"","on_accent":"","danger":""},"can_focus":false}"#).unwrap();
        assert_eq!((old.background_fps, old.anti_afk), (0, false));
    }

    #[test]
    fn keys_read_like_the_settings_write_them() {
        assert_eq!(parse_key("SUPER + TAB"), Some(Key { scancode: 43, mods: MOD_SUPER }));
        assert_eq!(parse_key("ctrl+shift+F12"), Some(Key { scancode: 69, mods: MOD_CTRL | MOD_SHIFT }));
        assert_eq!(parse_key("F1"), Some(Key { scancode: 58, mods: 0 }));
        assert_eq!(parse_key("INSERT"), Some(Key { scancode: 73, mods: 0 }));
        assert_eq!(parse_key("Shift + `"), Some(Key { scancode: 53, mods: MOD_SHIFT }));
        assert_eq!(parse_key("SUPER + P").map(|k| k.scancode), Some(19));
        assert_eq!(parse_key("SUPER"), None);
        assert_eq!(parse_key("SUPER + NOPE"), None);
    }

    #[test]
    fn layer_lists_gain_ours_once() {
        let mut env = launch::Env::new();
        env.insert("VK_INSTANCE_LAYERS".into(), "VK_LAYER_MANGOHUD_overlay_x86_64".into());
        add_to_list(&mut env, "VK_INSTANCE_LAYERS", LAYER_NAME, false);
        add_to_list(&mut env, "VK_INSTANCE_LAYERS", LAYER_NAME, false);
        assert_eq!(env["VK_INSTANCE_LAYERS"], format!("VK_LAYER_MANGOHUD_overlay_x86_64:{LAYER_NAME}"));
        add_to_list(&mut env, "VK_ADD_LAYER_PATH", "/run/x", true);
        assert_eq!(env["VK_ADD_LAYER_PATH"], "/run/x");
    }

    #[test]
    fn this_programs_own_needs_are_readable() {
        // any ELF on hand will do: this test binary
        let data = std::fs::read(std::env::current_exe().unwrap()).unwrap();
        let (needed, glibc) = elf_needs(&data).unwrap();
        assert!(needed.iter().any(|n| n == "libc.so.6"));
        assert!(glibc >= (2, 2) && Some(glibc) <= host_glibc());
        assert!(elf_needs(b"not an elf").is_err());
    }

    #[test]
    fn the_panel_gets_answers_over_its_socket() {
        let _home = crate::testenv::TempHome::new();
        let account = Account::new("main", "/nowhere", "Main", true);
        let server = Server::start(&account).expect("serves");
        assert!(Server::start(&account).is_none(), "one supervisor serves an account");
        assert!(!toast("main", "Hi", ""), "no panel yet to show it");
        let stream = UnixStream::connect(socket_path("main")).unwrap();
        let mut writer = stream.try_clone().unwrap();
        let hello = ToMace::Hello { v: mace_proto::VERSION, pid: 1, video: "test".into(), swapchain: String::new() };
        writer.write_all(hello.line().as_bytes()).unwrap();
        writer.write_all(ToMace::Run { id: 4, args: vec!["rm".into(), "-rf".into()] }.line().as_bytes()).unwrap();
        let mut lines = BufReader::new(stream).lines();
        let mut seen = Vec::new();
        for line in lines.by_ref().take(3) {
            seen.push(serde_json::from_str::<ToPanel>(&line.unwrap()).unwrap());
        }
        assert!(seen.iter().any(|m| matches!(m, ToPanel::Config(c) if c.me == "main" && c.key.scancode == 73)));
        assert!(seen.iter().any(|m| matches!(m, ToPanel::State(_))));
        assert!(seen.iter().any(|m| matches!(m, ToPanel::Done { id: 4, ok: false, error } if error.contains("not allowed"))));
        // another process's toast reaches the panel
        assert!(toast("main", "Join link copied", "Anyone with it can join"));
        let toasted = lines.map_while(Result::ok).filter_map(|l| serde_json::from_str::<ToPanel>(&l).ok())
            .find(|m| matches!(m, ToPanel::Toast { .. }));
        assert_eq!(toasted, Some(ToPanel::Toast { title: "Join link copied".into(), body: "Anyone with it can join".into() }));
        drop(server);
        assert!(!socket_path("main").exists());
    }

    #[test]
    fn joins_become_toasts_in_the_right_game() {
        use crate::servers::tests::{JOB, join_lines};
        let home = crate::testenv::TempHome::new();
        let main = accounts::register(Account::new("main", home.make_prefix("m", None), "", false)).unwrap();
        let alt = accounts::register(Account::new("alt1", home.make_prefix("a", None), "Second", false)).unwrap();
        // the server's name and place, as if looked up already: no network in tests
        std::fs::create_dir_all(paths::runtime()).unwrap();
        std::fs::write(paths::runtime().join("servers.json"),
                       serde_json::json!({JOB: {"name": "Obby", "location": "Paris, FR"}}).to_string()).unwrap();
        let _server = Server::start(&main).expect("serves");
        let stream = UnixStream::connect(socket_path("main")).unwrap();
        let _ = stream.set_read_timeout(Some(Duration::from_secs(8)));
        let hello = ToMace::Hello { v: mace_proto::VERSION, pid: 1, video: "test".into(), swapchain: String::new() };
        stream.try_clone().unwrap().write_all(hello.line().as_bytes()).unwrap();
        std::thread::sleep(Duration::from_millis(800));
        let write = |a: &Account| std::fs::write(a.logs().join("0.747.0.7471481_20261115T200000Z_Player_abcde_last.log"),
                                                 join_lines().join("\n") + "\n").unwrap();
        write(&main);
        std::thread::sleep(Duration::from_millis(800));
        write(&alt);
        let toasts: Vec<ToPanel> = BufReader::new(stream).lines().map_while(Result::ok)
            .filter_map(|l| serde_json::from_str::<ToPanel>(&l).ok())
            .filter(|m| matches!(m, ToPanel::Toast { .. })).take(2).collect();
        // both come; on a slow machine one look at the logs can find both, in either order
        assert_eq!(toasts.len(), 2, "{toasts:?}");
        for want in [ToPanel::Toast { title: "Joined Obby".into(), body: "Paris, FR".into() },
                     ToPanel::Toast { title: "Second joined your server".into(), body: String::new() }] {
            assert!(toasts.contains(&want), "{toasts:?}");
        }
    }
}

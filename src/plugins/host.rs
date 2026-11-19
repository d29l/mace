//! `mace plugin-host ACCOUNT`: runs the turned-on plugins for one running
//! account, each in a sandboxed Luau state of its own, talking to the
//! account's supervisor over stdin and stdout (see `protocol`). It notices
//! plugins being turned on or off and their files changing, and loads them
//! again by itself. It ends when its stdin closes (the supervisor is gone).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use mlua::{Function, IntoLuaMulti, Lua, Table, Value as LuaValue, VmState};
use serde_json::Value;

use super::api::{self, CALL_LIMIT, Ctx, ctx};
use super::protocol::{FromHost, ToHost};
use super::{Found, Manifest};
use mace_proto::{PluginButton, State};

/// Loading a plugin (running its script the first time) may take this long.
const LOAD_LIMIT: Duration = Duration::from_secs(1);
const MEMORY_LIMIT: usize = 32 << 20;
/// How often the plugins folder and the settings are looked at.
const SCAN_EVERY: Duration = Duration::from_secs(1);
/// Plugin buttons on the bar, at most.
pub const MOST_BUTTONS: usize = 4;
/// A plugin's failures are toasted at most this often (they're all logged).
const COMPLAIN_EVERY: Duration = Duration::from_secs(60);

struct Loaded {
    id: String,
    lua: Lua,
    stamp: SystemTime,
    /// The sites it was loaded able to reach.
    sites: Vec<String>,
}

/// What reaches the host: the supervisor's messages, and work other threads finished.
pub enum Input {
    Message(ToHost),
    /// The supervisor is gone.
    Ended,
    Http { plugin: String, id: u64, response: Value },
    Picture { plugin: String, key: String, picture: Option<(u32, Vec<u8>)> },
}

/// How often what's playing is looked at, for the `media` event.
const MEDIA_EVERY: Duration = Duration::from_millis(500);

pub struct Host {
    me: String,
    world: Rc<RefCell<Option<State>>>,
    out: Rc<RefCell<Vec<FromHost>>>,
    plugins: Vec<Loaded>,
    /// Plugins that failed to load, and the files' time then: not tried again until they change.
    failed: HashMap<String, SystemTime>,
    scanned: Option<Instant>,
    sent_buttons: Option<Vec<PluginButton>>,
    complained: HashMap<String, Instant>,
    tx: mpsc::Sender<Input>,
    rx: mpsc::Receiver<Input>,
    /// What was playing when last looked (player, track, playing), and when that was.
    media_seen: Option<Value>,
    media_looked: Option<Instant>,
}

/// The newest change to any file in a plugin's folder (a few levels down).
fn stamp(dir: &Path) -> SystemTime {
    fn walk(dir: &Path, depth: usize, newest: &mut SystemTime, seen: &mut usize) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            *seen += 1;
            if *seen > 500 {
                return;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if let Ok(time) = meta.modified() {
                *newest = (*newest).max(time);
            }
            if meta.is_dir() && depth < 3 {
                walk(&entry.path(), depth + 1, newest, seen);
            }
        }
    }
    let mut newest = dir.join("plugin.json").metadata().and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
    walk(dir, 0, &mut newest, &mut 0);
    newest
}

/// The first line of an error, without mlua's wrapping: for a toast.
pub fn brief(error: &mlua::Error) -> String {
    let text = error.to_string();
    let first = text.lines().next().unwrap_or("").trim();
    first.strip_prefix("runtime error: ").unwrap_or(first).chars().take(160).collect()
}

/// Call into a plugin, stopped if it takes too long.
fn guarded<R>(lua: &Lua, limit: Duration, work: impl FnOnce() -> mlua::Result<R>) -> mlua::Result<R> {
    let c = ctx(lua)?;
    let outer = c.deadline.get();
    c.deadline.set(Some(Instant::now() + limit));
    drop(c);
    let result = work();
    ctx(lua)?.deadline.set(outer);
    result
}

fn call(lua: &Lua, f: &Function, args: impl IntoLuaMulti) -> mlua::Result<()> {
    guarded(lua, CALL_LIMIT, || f.call::<()>(args))
}

impl Host {
    pub fn new(me: &str) -> Host {
        let (tx, rx) = mpsc::channel();
        Host { me: me.to_string(), world: Rc::new(RefCell::new(None)), out: Rc::new(RefCell::new(Vec::new())),
               plugins: Vec::new(), failed: HashMap::new(), scanned: None, sent_buttons: None, complained: HashMap::new(),
               tx, rx, media_seen: None, media_looked: None }
    }

    fn log(&self, text: String) {
        self.out.borrow_mut().push(FromHost::Log { text });
    }

    /// Where the supervisor's messages go in (`serve` reads them on a thread of its own).
    pub fn sender(&self) -> mpsc::Sender<Input> {
        self.tx.clone()
    }

    /// Wait for something to come in, at most `wait`; false once the supervisor is gone.
    pub fn wait(&mut self, wait: Duration) -> bool {
        match self.rx.recv_timeout(wait) {
            Ok(input) => {
                if !self.take(input) {
                    return false;
                }
                // whatever else came meanwhile, before drawing
                while let Ok(input) = self.rx.try_recv() {
                    if !self.take(input) {
                        return false;
                    }
                }
                true
            }
            Err(mpsc::RecvTimeoutError::Timeout) => true,
            Err(mpsc::RecvTimeoutError::Disconnected) => false,
        }
    }

    /// Hand over what other threads finished (tests: without waiting).
    pub fn pump(&mut self) {
        while let Ok(input) = self.rx.try_recv() {
            self.take(input);
        }
    }

    fn take(&mut self, input: Input) -> bool {
        match input {
            Input::Message(message) => self.handle(message),
            Input::Ended => return false,
            Input::Http { plugin, id, response } => {
                let Some(i) = self.plugins.iter().position(|p| p.id == plugin) else { return true };
                let lua = self.plugins[i].lua.clone();
                if let Ok(c) = ctx(&lua) {
                    c.in_flight.set(c.in_flight.get().saturating_sub(1));
                    // what it does with the answer likely changes its menu
                    c.dirty.set(true);
                }
                let Ok(callbacks) = lua.named_registry_value::<Table>("mace_http") else { return true };
                let Ok(Some(f)) = callbacks.get::<Option<Function>>(id) else { return true };
                let _ = callbacks.set(id, LuaValue::Nil);
                let arg = api::to_lua(&lua, &response).unwrap_or(LuaValue::Nil);
                if let Err(error) = call(&lua, &f, arg) {
                    self.fail(i, "a request's answer", &error);
                }
            }
            Input::Picture { plugin, key, picture } => {
                let Some(i) = self.plugins.iter().position(|p| p.id == plugin) else { return true };
                match picture {
                    Some((size, rgba)) => {
                        self.out.borrow_mut().push(FromHost::Image { key, size, rgba: mace_proto::base64(&rgba) });
                    }
                    // let a later draw ask again
                    None => {
                        if let Ok(c) = ctx(&self.plugins[i].lua) {
                            c.images.borrow_mut().remove(&key);
                        }
                    }
                }
            }
        }
        true
    }

    /// The `media` event, for plugins that may see what's playing, when it changed.
    pub fn watch_media(&mut self) {
        let listening: Vec<usize> = (0..self.plugins.len()).filter(|&i| {
            let lua = &self.plugins[i].lua;
            ctx(lua).is_ok_and(|c| c.permissions.iter().any(|p| p == "media"))
                && lua.named_registry_value::<Table>("mace_handlers")
                    .and_then(|h| h.get::<Option<Table>>("media")).is_ok_and(|h| h.is_some())
        }).collect();
        if listening.is_empty() || self.media_looked.is_some_and(|t| t.elapsed() < MEDIA_EVERY) {
            return;
        }
        self.media_looked = Some(Instant::now());
        let now = super::media::current(None);
        let seen = now.as_ref().map(|n| serde_json::json!([n["player"], n["title"], n["artist"], n["album"], n["playing"]]));
        if seen == self.media_seen {
            return;
        }
        self.media_seen = seen;
        let data = now.unwrap_or(Value::Null);
        for i in listening {
            self.call_handlers(i, "media", &data);
        }
    }

    fn call_handlers(&mut self, i: usize, name: &str, data: &Value) {
        let lua = self.plugins[i].lua.clone();
        let handlers = lua.named_registry_value::<Table>("mace_handlers").and_then(|h| h.get::<Option<Table>>(name));
        let Ok(Some(handlers)) = handlers else { return };
        let arg = api::to_lua(&lua, data).unwrap_or(LuaValue::Nil);
        for handler in handlers.sequence_values::<Function>().flatten() {
            if let Err(error) = call(&lua, &handler, arg.clone()) {
                self.fail(i, &format!("on \"{name}\""), &error);
            }
        }
    }

    fn complain(&mut self, id: &str, name: &str, what: &str, error: &mlua::Error) {
        self.log(format!("plugin {id}: {what}: {error}"));
        let due = self.complained.get(id).is_none_or(|at| at.elapsed() >= COMPLAIN_EVERY);
        if due {
            self.complained.insert(id.to_string(), Instant::now());
            self.out.borrow_mut().push(FromHost::Toast { title: format!("{name} plugin failed"), body: brief(error) });
        }
    }

    fn load(&mut self, id: &str, dir: &Path, manifest: &Manifest, sites: Vec<String>) -> Result<Loaded, mlua::Error> {
        let lua = Lua::new();
        lua.set_memory_limit(MEMORY_LIMIT)?;
        lua.set_app_data(Ctx {
            id: id.to_string(),
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            dir: dir.to_path_buf(),
            permissions: manifest.permissions.clone(),
            me: self.me.clone(),
            world: self.world.clone(),
            out: self.out.clone(),
            timers: RefCell::new(Vec::new()),
            next_id: Cell::new(0),
            button: RefCell::new(None),
            menu_open: Cell::new(false),
            dirty: Cell::new(false),
            deadline: Cell::new(None),
            images: RefCell::new(Default::default()),
            sites: sites.clone(),
            wanted: manifest.http.clone(),
            done: self.tx.clone(),
            in_flight: Cell::new(0),
            recent: RefCell::new(Default::default()),
            last_menu: RefCell::new(None),
            size: Cell::new((0.0, 0.0)),
        });
        lua.set_interrupt(|lua| {
            if let Some(c) = lua.app_data_ref::<Ctx>()
                && c.deadline.get().is_some_and(|d| Instant::now() > d)
            {
                return Err(mlua::Error::runtime("took too long, so it was stopped"));
            }
            Ok(VmState::Continue)
        });
        api::install(&lua)?;
        lua.sandbox(true)?;
        let source = std::fs::read_to_string(dir.join(&manifest.main))
            .map_err(|e| mlua::Error::runtime(format!("{}: {e}", manifest.main)))?;
        guarded(&lua, LOAD_LIMIT, || lua.load(source).set_name(format!("@{}", manifest.main)).exec())?;
        Ok(Loaded { id: id.to_string(), lua, stamp: stamp(dir), sites })
    }

    /// Load what's turned on and changed, drop what's off or gone.
    pub fn scan(&mut self, force: bool) {
        if !force && self.scanned.is_some_and(|t| t.elapsed() < SCAN_EVERY) {
            return;
        }
        self.scanned = Some(Instant::now());
        let on = crate::settings::load().plugins;
        let wanted: Vec<Found> = super::list().into_iter().filter(|f| super::entry_is_on(on.get(&f.id))).collect();
        let before = self.plugins.len();
        self.plugins.retain(|p| wanted.iter().any(|f| f.id == p.id && f.manifest.is_ok()));
        if self.plugins.len() != before {
            self.log(format!("plugins: {} running", self.plugins.len()));
        }
        for found in wanted {
            let now = stamp(&found.dir);
            let loaded = self.plugins.iter().position(|p| p.id == found.id);
            let Ok(manifest) = &found.manifest else { continue };
            // the sites it may reach: named in plugin.json and agreed to when turned on
            let agreed = super::agreed_sites(on.get(&found.id));
            let sites: Vec<String> = manifest.http.iter().filter(|s| agreed.contains(s)).cloned().collect();
            let same = loaded.is_some_and(|i| self.plugins[i].stamp == now && self.plugins[i].sites == sites);
            if same || (loaded.is_none() && self.failed.get(&found.id) == Some(&now)) {
                continue;
            }
            // a reload keeps the menu open if it was
            let was_open = loaded.is_some_and(|i| ctx(&self.plugins[i].lua).is_ok_and(|c| c.menu_open.get()));
            if let Some(i) = loaded {
                self.plugins.remove(i);
            }
            match self.load(&found.id, &found.dir, manifest, sites) {
                Ok(plugin) => {
                    self.failed.remove(&found.id);
                    self.log(format!("plugin {}: {} {}", found.id, if loaded.is_some() { "reloaded" } else { "loaded" },
                                     manifest.version));
                    if was_open && let Ok(c) = ctx(&plugin.lua) {
                        c.menu_open.set(true);
                        c.dirty.set(true);
                    }
                    self.plugins.push(plugin);
                }
                Err(error) => {
                    self.failed.insert(found.id.clone(), now);
                    self.complain(&found.id.clone(), &manifest.name.clone(), "couldn't load", &error);
                }
            }
        }
        self.plugins.sort_by(|a, b| a.id.cmp(&b.id));
    }

    pub fn handle(&mut self, message: ToHost) {
        match message {
            ToHost::State { state } => *self.world.borrow_mut() = Some(state),
            ToHost::Event { name, data } => {
                // what's playing is only for plugins allowed to see it (it comes from watch_media)
                if name == "media" {
                    return;
                }
                for i in 0..self.plugins.len() {
                    self.call_handlers(i, &name, &data);
                }
            }
            ToHost::Ui { plugin, button, key, what, value } => {
                let Some(i) = self.plugins.iter().position(|p| p.id == plugin) else { return };
                let lua = self.plugins[i].lua.clone();
                let Ok(c) = ctx(&lua) else { return };
                if c.button.borrow().as_ref().is_none_or(|b| b.id != button) {
                    return;
                }
                // the room the menu has: open and size say
                if let Some(room) = value.as_ref().filter(|_| what == "open" || what == "size") {
                    let size = (room["width"].as_f64().unwrap_or(0.0), room["height"].as_f64().unwrap_or(0.0));
                    if size != c.size.get() {
                        c.size.set(size);
                        c.dirty.set(true);
                    }
                }
                match what.as_str() {
                    "open" => {
                        c.menu_open.set(true);
                        c.dirty.set(true);
                        // a panel opening it (maybe a new one) gets it whether or not it changed
                        c.last_menu.borrow_mut().take();
                    }
                    "close" => c.menu_open.set(false),
                    "click" | "change" | "submit" => {
                        drop(c);
                        let found = ["mace_keys", "mace_old_keys"].iter().find_map(|registry| {
                            lua.named_registry_value::<Table>(registry).ok()?.get::<Option<Function>>(key.as_str()).ok()?
                        });
                        let Some(f) = found else { return };
                        let arg = match (what.as_str(), value) {
                            ("change", Some(Value::Bool(on))) => LuaValue::Boolean(on),
                            ("submit", Some(Value::String(text))) => {
                                lua.create_string(text.chars().take(2000).collect::<String>())
                                    .map(LuaValue::String).unwrap_or(LuaValue::Nil)
                            }
                            _ => LuaValue::Nil,
                        };
                        if let Err(error) = call(&lua, &f, arg) {
                            self.fail(i, "a menu control", &error);
                        }
                        // what was used likely changed what the menu shows
                        if let Ok(c) = ctx(&lua) {
                            c.dirty.set(true);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn fail(&mut self, i: usize, what: &str, error: &mlua::Error) {
        let (id, name) = match ctx(&self.plugins[i].lua) {
            Ok(c) => (c.id.clone(), c.name.clone()),
            Err(_) => return,
        };
        self.complain(&id, &name, what, error);
    }

    /// Run the timers that are due.
    pub fn run_timers(&mut self) {
        let now = Instant::now();
        for i in 0..self.plugins.len() {
            let lua = self.plugins[i].lua.clone();
            let due: Vec<(u64, bool)> = {
                let Ok(c) = ctx(&lua) else { continue };
                let mut timers = c.timers.borrow_mut();
                let mut due = Vec::new();
                for t in timers.iter_mut().filter(|t| t.due <= now) {
                    due.push((t.id, t.every.is_some()));
                    if let Some(every) = t.every {
                        t.due = now + every;
                    }
                }
                timers.retain(|t| t.every.is_some() || t.due > now);
                due
            };
            for (id, repeats) in due {
                let Ok(callbacks) = lua.named_registry_value::<Table>("mace_timers") else { continue };
                let Ok(Some(f)) = callbacks.get::<Option<Function>>(id) else { continue };
                if !repeats {
                    let _ = callbacks.set(id, LuaValue::Nil);
                }
                if let Err(error) = call(&lua, &f, ()) {
                    self.fail(i, "a timer", &error);
                }
            }
        }
    }

    /// When the host next has something to do on its own.
    pub fn next_wake(&self) -> Duration {
        let now = Instant::now();
        let mut wait = SCAN_EVERY.min(MEDIA_EVERY);
        for p in &self.plugins {
            if let Ok(c) = ctx(&p.lua) {
                for t in c.timers.borrow().iter() {
                    wait = wait.min(t.due.saturating_duration_since(now));
                }
            }
        }
        wait.max(Duration::from_millis(5))
    }

    /// Draw the open menus that changed, and tell the panel about the buttons when they did.
    pub fn settle(&mut self) {
        for i in 0..self.plugins.len() {
            let lua = self.plugins[i].lua.clone();
            let Ok(c) = ctx(&lua) else { continue };
            if !(c.menu_open.get() && c.dirty.replace(false)) {
                continue;
            }
            let (id, name, button) = (c.id.clone(), c.name.clone(), c.button.borrow().as_ref().map(|b| b.id.clone()));
            drop(c);
            let Some(button) = button else { continue };
            match self.render(&lua) {
                Ok(menu) => {
                    // often unchanged (a timer that redraws often): then not sent again
                    let c = ctx(&lua);
                    let fresh = c.as_ref().is_ok_and(|c| c.last_menu.borrow().as_ref() != Some(&menu));
                    if fresh && let Ok(c) = c {
                        *c.last_menu.borrow_mut() = Some(menu.clone());
                        drop(c);
                        self.out.borrow_mut().push(FromHost::Menu { plugin: id, button, menu });
                    }
                }
                Err(error) => {
                    let menu = mace_proto::Menu { title: name.clone(), subtitle: None, children: vec![
                        mace_proto::Node::text(format!("This menu failed: {}", brief(&error)), "subtle"),
                    ] };
                    self.out.borrow_mut().push(FromHost::Menu { plugin: id.clone(), button, menu });
                    self.complain(&id, &name, "its menu", &error);
                }
            }
        }
        let buttons: Vec<PluginButton> = self.plugins.iter()
            .filter_map(|p| {
                let c = ctx(&p.lua).ok()?;
                let b = c.button.borrow().clone()?;
                Some(PluginButton { plugin: c.id.clone(), id: b.id, icon: b.icon, tip: b.tip, badge: b.badge })
            })
            .take(MOST_BUTTONS)
            .collect();
        // nothing to say until there's a button (most people have no plugins)
        let before = self.sent_buttons.as_deref().unwrap_or_default();
        if before != buttons.as_slice() {
            self.sent_buttons = Some(buttons.clone());
            self.out.borrow_mut().push(FromHost::Buttons { buttons });
        }
    }

    fn render(&self, lua: &Lua) -> mlua::Result<mace_proto::Menu> {
        let Some(menu) = lua.named_registry_value::<Option<Function>>("mace_menu")? else {
            return Err(mlua::Error::runtime("its button has no menu"));
        };
        // keys from the menu before stay good a little longer: a click may be on the one still showing
        let current: Table = lua.named_registry_value("mace_keys")?;
        lua.set_named_registry_value("mace_old_keys", current)?;
        let keys = lua.create_table()?;
        lua.set_named_registry_value("mace_keys", &keys)?;
        // the room it has, for menus that change with it
        let (width, height) = ctx(lua)?.size.get();
        let room = lua.create_table()?;
        room.set("width", width)?;
        room.set("height", height)?;
        let made: LuaValue = guarded(lua, CALL_LIMIT, || menu.call(room))?;
        let c = ctx(lua)?;
        let mut keys = super::ui::Keys { table: &keys, ctx: &c, count: 0 };
        guarded(lua, CALL_LIMIT, || super::ui::to_menu(lua, made, &mut keys))
    }

    pub fn take_output(&self) -> Vec<FromHost> {
        std::mem::take(&mut *self.out.borrow_mut())
    }

    /// The ids of the plugins running.
    pub fn running(&self) -> Vec<String> {
        self.plugins.iter().map(|p| p.id.clone()).collect()
    }
}

/// Serve the supervisor on `input` and `output` until `input` ends.
pub fn serve(me: &str, input: impl BufRead + Send + 'static, mut output: impl Write) {
    let mut host = Host::new(me);
    let tx = host.sender();
    std::thread::spawn(move || {
        for line in input.lines() {
            let Ok(line) = line else { break };
            if let Ok(message) = serde_json::from_str::<ToHost>(&line)
                && tx.send(Input::Message(message)).is_err()
            {
                return;
            }
        }
        let _ = tx.send(Input::Ended);
    });
    loop {
        host.scan(false);
        host.run_timers();
        host.watch_media();
        host.settle();
        for message in host.take_output() {
            if output.write_all(message.line().as_bytes()).and_then(|_| output.flush()).is_err() {
                return;
            }
        }
        if !host.wait(host.next_wake()) {
            return;
        }
    }
}

/// `mace plugin-host ACCOUNT`.
pub fn run(me: &str) -> i32 {
    serve(me, std::io::BufReader::new(std::io::stdin()), std::io::stdout());
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::tests::write_plugin;
    use crate::testenv::TempHome;
    use mace_proto::Node;
    use serde_json::json;

    fn manifest(id: &str, permissions: &[&str]) -> Value {
        json!({ "id": id, "name": id.to_uppercase(), "version": "1.0.0", "api": 1, "permissions": permissions })
    }

    fn turned_on(id: &str, permissions: &[&str], script: &str) -> std::path::PathBuf {
        let dir = write_plugin(id, manifest(id, permissions), script);
        super::super::set_on(id, true).unwrap();
        dir
    }

    fn host() -> Host {
        let mut host = Host::new("main");
        *host.world.borrow_mut() = Some(State { now: 0.0, accounts: vec![mace_proto::Account {
            id: "main".into(), name: "Main".into(), running: true,
            server: Some(mace_proto::Server { place: "1818".into(), job: "j1".into(), name: Some("Obby".into()),
                                              location: Some("\u{1F1E9}\u{1F1EA} Frankfurt, DE".into()), ..Default::default() }),
            ..Default::default()
        }], games: Vec::new() });
        host.scan(true);
        host.settle();
        host
    }

    fn ui(host: &mut Host, plugin: &str, key: &str, what: &str, value: Option<Value>) -> Vec<FromHost> {
        host.handle(ToHost::Ui { plugin: plugin.into(), button: "b".into(), key: key.into(), what: what.into(), value });
        host.settle();
        host.take_output()
    }

    fn menu_of(out: &[FromHost]) -> mace_proto::Menu {
        out.iter().rev().find_map(|m| match m { FromHost::Menu { menu, .. } => Some(menu.clone()), _ => None })
            .expect("a menu was drawn")
    }

    fn toasts(out: &[FromHost]) -> Vec<(String, String)> {
        out.iter().filter_map(|m| match m { FromHost::Toast { title, body } => Some((title.clone(), body.clone())), _ => None })
            .collect()
    }

    fn key_of(menu: &mace_proto::Menu, label: &str) -> String {
        menu.children.iter().find_map(|n| match n {
            Node::Button { label: l, key, .. } if l == label => key.clone(),
            _ => None,
        }).expect("the button")
    }

    const COUNTER: &str = r#"
        local ui = mace.ui
        local count = mace.storage.get("count") or 0
        mace.bar.button({ id = "b", icon = "timer", tip = "Count", menu = function()
            local game = mace.game()
            return ui.menu({ title = "Count", subtitle = game and game.location }, {
                ui.text(`{count}`),
                ui.button("More", { style = "primary", onClick = function()
                    count += 1
                    mace.storage.set("count", count)
                end }),
            })
        end })
    "#;

    #[test]
    fn a_button_its_menu_and_a_click() {
        let _home = TempHome::new();
        turned_on("counter", &[], COUNTER);
        let mut host = host();
        let out = host.take_output();
        assert!(out.iter().any(|m| matches!(m, FromHost::Buttons { buttons }
            if buttons.len() == 1 && buttons[0].icon == "timer" && buttons[0].tip == "Count")), "{out:?}");
        let menu = menu_of(&ui(&mut host, "counter", "", "open", None));
        assert_eq!((menu.title.as_str(), menu.subtitle.as_deref()), ("Count", Some("Frankfurt, DE")));
        assert_eq!(menu.children[0], Node::text("0", ""));
        let more = key_of(&menu, "More");
        let menu = menu_of(&ui(&mut host, "counter", &more, "click", None));
        assert_eq!(menu.children[0], Node::text("1", ""));
        // stored: a host started later (another account, or after a restart) sees it
        let mut again = Host::new("alt1");
        again.scan(true);
        let menu = menu_of(&ui(&mut again, "counter", "", "open", None));
        assert_eq!(menu.children[0], Node::text("1", ""));
        // closed: no more drawing
        assert!(ui(&mut host, "counter", "", "close", None).iter().all(|m| !matches!(m, FromHost::Menu { .. })));
    }

    #[test]
    fn what_needs_a_permission_asks_for_it() {
        let _home = TempHome::new();
        let script = r#"
            mace.bar.button({ id = "b", icon = "refresh", menu = function()
                return mace.ui.menu({}, { mace.ui.button("Go", { onClick = function() mace.rejoin() end }) })
            end })
        "#;
        turned_on("polite", &["join"], script);
        turned_on("rude", &[], script);
        let mut host = host();
        host.take_output();
        let go = key_of(&menu_of(&ui(&mut host, "polite", "", "open", None)), "Go");
        let out = ui(&mut host, "polite", &go, "click", None);
        assert!(out.iter().any(|m| matches!(m, FromHost::Act { args } if args == &["rejoin", "main"])), "{out:?}");
        let go = key_of(&menu_of(&ui(&mut host, "rude", "", "open", None)), "Go");
        let out = ui(&mut host, "rude", &go, "click", None);
        assert!(!out.iter().any(|m| matches!(m, FromHost::Act { .. })));
        let (title, body) = &toasts(&out)[0];
        assert_eq!(title, "RUDE plugin failed");
        assert!(body.contains("\"join\" permission"), "{body}");
    }

    #[test]
    fn a_plugin_that_never_stops_is_stopped() {
        let _home = TempHome::new();
        turned_on("spin", &[], r#"
            local spins = 0
            mace.bar.button({ id = "b", menu = function()
                return mace.ui.menu({}, {
                    mace.ui.text(`{spins}`),
                    mace.ui.button("Spin", { onClick = function() spins += 1; while true do end end }),
                    mace.ui.button("Fine", { onClick = function() spins += 10 end }),
                })
            end })
        "#);
        let mut host = host();
        let menu = menu_of(&ui(&mut host, "spin", "", "open", None));
        let started = Instant::now();
        let out = ui(&mut host, "spin", &key_of(&menu, "Spin"), "click", None);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(toasts(&out)[0].1.contains("took too long"), "{out:?}");
        // and it still works afterwards
        let menu = menu_of(&out);
        let out = ui(&mut host, "spin", &key_of(&menu, "Fine"), "click", None);
        assert_eq!(menu_of(&out).children[0], Node::text("11", ""));
    }

    #[test]
    fn a_broken_plugin_leaves_the_others_and_comes_back_when_fixed() {
        let _home = TempHome::new();
        let dir = turned_on("broken", &[], "mace.bar.button({ id = \"b\", icon = ");
        turned_on("fine", &[], "mace.bar.button({ id = \"b\", icon = \"star\" })");
        write_plugin("off", manifest("off", &[]), "mace.bar.button({ id = \"b\" })");
        let mut host = host();
        assert_eq!(host.running(), ["fine"]);
        let out = host.take_output();
        assert_eq!(toasts(&out)[0].0, "BROKEN plugin failed");
        assert!(out.iter().any(|m| matches!(m, FromHost::Buttons { buttons } if buttons.len() == 1 && buttons[0].plugin == "fine")));
        // not tried again until its files change
        host.scan(true);
        assert!(toasts(&host.take_output()).is_empty());
        std::fs::write(dir.join("init.luau"), "mace.bar.button({ id = \"b\", icon = \"bolt\" })").unwrap();
        let later = std::fs::File::options().write(true).open(dir.join("init.luau")).unwrap();
        later.set_modified(SystemTime::now() + Duration::from_secs(2)).unwrap();
        host.scan(true);
        host.settle();
        assert_eq!(host.running(), ["broken", "fine"]);
        assert!(host.take_output().iter().any(|m| matches!(m, FromHost::Buttons { buttons } if buttons.len() == 2)));
        // turned off: gone
        super::super::set_on("fine", false).unwrap();
        host.scan(true);
        assert_eq!(host.running(), ["broken"]);
    }

    #[test]
    fn events_and_timers_reach_the_plugin() {
        let _home = TempHome::new();
        turned_on("listen", &[], r#"
            mace.on("joined", function(server) mace.toast("joined", server.name) end)
            mace.on("left", function() mace.toast("left") end)
            mace.after(0, function() mace.toast("soon") end)
            local ticks = 0
            local t
            t = mace.every(1, function() ticks += 1; if ticks == 2 then t:cancel() end end)
            mace.bar.button({ id = "b", menu = function() return mace.ui.menu({}, { mace.ui.text(`{ticks}`) }) end })
        "#);
        let mut host = host();
        host.handle(ToHost::Event { name: "joined".into(), data: json!({ "name": "Obby", "place": "1" }) });
        host.handle(ToHost::Event { name: "left".into(), data: Value::Null });
        host.run_timers();
        let said: Vec<_> = toasts(&host.take_output()).into_iter().map(|(t, b)| format!("{t}:{b}")).collect();
        assert_eq!(said, ["joined:Obby", "left:", "soon:"]);
        assert!(host.next_wake() <= Duration::from_secs(1));
    }

    #[test]
    fn menus_are_checked_and_pictures_sent_once() {
        let _home = TempHome::new();
        let dir = turned_on("pics", &[], r#"
            mace.bar.button({ id = "b", icon = "pic.png", menu = function()
                return mace.ui.menu({ title = "Pics" }, {
                    mace.ui.row({ image = "pic.png", title = "A row", onClick = function() end,
                                  trailing = mace.ui.button("X", { icon = "close" }) }),
                    mace.ui.toggle("On", { value = true, onChange = function(on) mace.toast(tostring(on)) end }),
                    mace.ui.input({ placeholder = "Say", onSubmit = function(t) mace.toast(t) end }),
                    mace.ui.stack({ "plain", mace.ui.space("small") }, { direction = "row" }),
                })
            end })
        "#);
        // a 2x2 red PNG
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 2, 2);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.write_header().unwrap().write_image_data(&[255, 0, 0, 255].repeat(4)).unwrap();
        }
        std::fs::write(dir.join("pic.png"), png).unwrap();
        let mut host = host();
        let out = host.take_output();
        assert!(toasts(&out).is_empty(), "{out:?}");
        let mut out = out;
        out.extend(ui(&mut host, "pics", "", "open", None));
        out.extend(ui(&mut host, "pics", "", "open", None));
        let images: Vec<_> = out.iter().filter(|m| matches!(m, FromHost::Image { .. })).collect();
        assert_eq!(images.len(), 1);
        let menu = menu_of(&out);
        let Node::Row { image: Some(image), key: Some(_), trailing: Some(trailing), .. } = &menu.children[0] else { panic!() };
        assert_eq!(image, "plugin:pics/pic.png");
        assert!(matches!(&**trailing, Node::Button { icon: Some(i), .. } if i == "close"));
        let Node::Toggle { key: Some(toggle), .. } = &menu.children[1] else { panic!() };
        let Node::Input { key: Some(input), .. } = &menu.children[2] else { panic!() };
        let out = ui(&mut host, "pics", toggle, "change", Some(json!(false)));
        assert_eq!(toasts(&out)[0].0, "false");
        let out = ui(&mut host, "pics", input, "submit", Some(json!("hello")));
        assert_eq!(toasts(&out)[0].0, "hello");
        assert!(matches!(&menu.children[3], Node::Stack { direction, children, .. } if direction == "row" && children.len() == 2));
    }

    #[test]
    fn a_bad_menu_says_so() {
        let _home = TempHome::new();
        turned_on("oops", &[], r#"
            mace.bar.button({ id = "b", menu = function() return mace.ui.menu({}, { { type = "chart" } }) end })
        "#);
        let mut host = host();
        let out = ui(&mut host, "oops", "", "open", None);
        let menu = menu_of(&out);
        assert!(matches!(&menu.children[0], Node::Text { text, .. } if text.contains("no block \"chart\"")), "{menu:?}");
    }

    /// Run `script` as a click on a button "Go" of a plugin with these sites; the toasts it made.
    fn click_go(host: &mut Host, id: &str) -> Vec<(String, String)> {
        let go = key_of(&menu_of(&ui(host, id, "", "open", None)), "Go");
        let mut out = ui(host, id, &go, "click", None);
        // answers come from other threads
        for _ in 0..100 {
            host.pump();
            host.settle();
            out.extend(host.take_output());
            if !toasts(&out).is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        toasts(&out)
    }

    fn web(id: &str, sites: &[&str], script: &str) {
        let body = format!(r#"
            mace.bar.button({{ id = "b", menu = function()
                return mace.ui.menu({{}}, {{ mace.ui.button("Go", {{ onClick = function() {script} end }}) }})
            end }})
        "#);
        write_plugin(id, json!({ "id": id, "name": id, "api": 1, "http": sites }), &body);
        super::super::set_on(id, true).unwrap();
    }

    #[test]
    fn the_web_only_the_sites_agreed_to() {
        let _home = TempHome::new();
        let answer = r#"function(r) mace.toast(tostring(r.status), r.body) end"#;
        web("fine", &["mace.test"], &format!("mace.http.get(\"https://mace.test/x?q=1\", {answer})"));
        web("other", &["mace.test"], &format!("mace.http.get(\"https://elsewhere.test/\", {answer})"));
        web("plain", &["mace.test"], &format!("mace.http.get(\"http://mace.test/\", {answer})"));
        web("post", &["*.mace.test"], &format!(
            "mace.http.request({{ url = \"https://api.mace.test/p\", method = \"post\", body = \"x\" }}, {answer})"));
        let mut host = host();
        assert_eq!(click_go(&mut host, "fine"), [("200".to_string(), "GET https://mace.test/x?q=1".to_string())]);
        assert!(click_go(&mut host, "other")[0].1.contains("isn't one of the plugin's sites"));
        assert!(click_go(&mut host, "plain")[0].1.contains("only https"));
        assert_eq!(click_go(&mut host, "post"), [("200".to_string(), "POST https://api.mace.test/p".to_string())]);
        // a site added after it was turned on isn't agreed to
        let dir = super::super::dir().join("fine");
        std::fs::write(dir.join("plugin.json"), json!({ "id": "fine", "name": "fine", "api": 1,
                                                        "http": ["mace.test", "more.test"] }).to_string()).unwrap();
        std::fs::write(dir.join("init.luau"), r#"
            mace.bar.button({ id = "b", menu = function()
                return mace.ui.menu({}, { mace.ui.button("Go", { onClick = function()
                    mace.http.get("https://more.test/", function() end)
                end }) })
            end })
        "#).unwrap();
        std::fs::File::options().write(true).open(dir.join("init.luau")).unwrap()
            .set_modified(SystemTime::now() + Duration::from_secs(2)).unwrap();
        host.scan(true);
        assert!(click_go(&mut host, "fine")[0].1.contains("turn it off and on again"));
        // turned on again: agreed
        super::super::set_on("fine", false).unwrap();
        super::super::set_on("fine", true).unwrap();
        assert_eq!(crate::plugins::agreed_sites(crate::settings::load().plugins.get("fine")), ["mace.test", "more.test"]);
    }

    #[test]
    fn json_progress_pictures_and_quiet_redraws() {
        let _home = TempHome::new();
        turned_on("bits", &[], r#"
            local t = mace.json.decode('{"a": [1, 2], "b": null, "c": "x"}')
            assert(t.a[2] == 2 and t.b == nil and t.c == "x")
            assert(mace.json.decode(mace.json.encode({ n = 3 })).n == 3)
            local ticks = 0
            mace.every(0.1, function() ticks += 1 end)
            mace.bar.button({ id = "b", menu = function()
                return mace.ui.menu({}, {
                    mace.ui.progress(30, 200, { rate = 1, times = true }),
                    mace.ui.text("same every time"),
                })
            end })
        "#);
        let mut host = host();
        assert!(toasts(&host.take_output()).is_empty());
        let menu = menu_of(&ui(&mut host, "bits", "", "open", None));
        assert_eq!(menu.children[0], Node::Progress { value: 30.0, max: 200.0, rate: 1.0, times: true, key: None });
        // redrawn, but the same: nothing sent
        host.handle(ToHost::Ui { plugin: "bits".into(), button: "b".into(), key: "k".into(), what: "click".into(), value: None });
        if let Ok(c) = ctx(&host.plugins[0].lua) {
            c.dirty.set(true);
        }
        host.settle();
        assert!(!host.take_output().iter().any(|m| matches!(m, FromHost::Menu { .. })));
        // timers every tenth of a second
        std::thread::sleep(Duration::from_millis(150));
        host.run_timers();
        assert!(host.next_wake() <= Duration::from_millis(100));
    }
}

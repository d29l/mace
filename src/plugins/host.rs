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
}

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
        Host { me: me.to_string(), world: Rc::new(RefCell::new(None)), out: Rc::new(RefCell::new(Vec::new())),
               plugins: Vec::new(), failed: HashMap::new(), scanned: None, sent_buttons: None, complained: HashMap::new() }
    }

    fn log(&self, text: String) {
        self.out.borrow_mut().push(FromHost::Log { text });
    }

    fn complain(&mut self, id: &str, name: &str, what: &str, error: &mlua::Error) {
        self.log(format!("plugin {id}: {what}: {error}"));
        let due = self.complained.get(id).is_none_or(|at| at.elapsed() >= COMPLAIN_EVERY);
        if due {
            self.complained.insert(id.to_string(), Instant::now());
            self.out.borrow_mut().push(FromHost::Toast { title: format!("{name} plugin failed"), body: brief(error) });
        }
    }

    fn load(&mut self, id: &str, dir: &Path, manifest: &Manifest) -> Result<Loaded, mlua::Error> {
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
        Ok(Loaded { id: id.to_string(), lua, stamp: stamp(dir) })
    }

    /// Load what's turned on and changed, drop what's off or gone.
    pub fn scan(&mut self, force: bool) {
        if !force && self.scanned.is_some_and(|t| t.elapsed() < SCAN_EVERY) {
            return;
        }
        self.scanned = Some(Instant::now());
        let on = crate::settings::load().plugins;
        let wanted: Vec<Found> = super::list().into_iter().filter(|f| on.get(&f.id).and_then(Value::as_bool) == Some(true))
            .collect();
        let before = self.plugins.len();
        self.plugins.retain(|p| wanted.iter().any(|f| f.id == p.id && f.manifest.is_ok()));
        if self.plugins.len() != before {
            self.log(format!("plugins: {} running", self.plugins.len()));
        }
        for found in wanted {
            let now = stamp(&found.dir);
            let loaded = self.plugins.iter().position(|p| p.id == found.id);
            if loaded.is_some_and(|i| self.plugins[i].stamp == now) || self.failed.get(&found.id) == Some(&now) {
                continue;
            }
            let Ok(manifest) = &found.manifest else { continue };
            // a reload keeps the menu open if it was
            let was_open = loaded.is_some_and(|i| ctx(&self.plugins[i].lua).is_ok_and(|c| c.menu_open.get()));
            if let Some(i) = loaded {
                self.plugins.remove(i);
            }
            match self.load(&found.id, &found.dir, manifest) {
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
                for i in 0..self.plugins.len() {
                    let lua = self.plugins[i].lua.clone();
                    let handlers = lua.named_registry_value::<Table>("mace_handlers")
                        .and_then(|h| h.get::<Option<Table>>(name.as_str()));
                    let Ok(Some(handlers)) = handlers else { continue };
                    let arg = api::to_lua(&lua, &data).unwrap_or(LuaValue::Nil);
                    for handler in handlers.sequence_values::<Function>().flatten() {
                        if let Err(error) = call(&lua, &handler, arg.clone()) {
                            self.fail(i, &format!("on \"{name}\""), &error);
                        }
                    }
                }
            }
            ToHost::Ui { plugin, button, key, what, value } => {
                let Some(i) = self.plugins.iter().position(|p| p.id == plugin) else { return };
                let lua = self.plugins[i].lua.clone();
                let Ok(c) = ctx(&lua) else { return };
                if c.button.borrow().as_ref().is_none_or(|b| b.id != button) {
                    return;
                }
                match what.as_str() {
                    "open" => {
                        c.menu_open.set(true);
                        c.dirty.set(true);
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
        let mut wait = SCAN_EVERY;
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
                Ok(menu) => self.out.borrow_mut().push(FromHost::Menu { plugin: id, button, menu }),
                Err(error) => {
                    let menu = mace_proto::Menu { title: name.clone(), subtitle: None, children: vec![
                        mace_proto::Node::Text { text: format!("This menu failed: {}", brief(&error)), style: "subtle".into() },
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
        let made: LuaValue = guarded(lua, CALL_LIMIT, || menu.call(()))?;
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
    let (tx, rx) = mpsc::channel::<ToHost>();
    std::thread::spawn(move || {
        for line in input.lines() {
            let Ok(line) = line else { break };
            if let Ok(message) = serde_json::from_str::<ToHost>(&line)
                && tx.send(message).is_err()
            {
                return;
            }
        }
    });
    let mut host = Host::new(me);
    loop {
        host.scan(false);
        host.run_timers();
        host.settle();
        for message in host.take_output() {
            if output.write_all(message.line().as_bytes()).and_then(|_| output.flush()).is_err() {
                return;
            }
        }
        match rx.recv_timeout(host.next_wake()) {
            Ok(message) => {
                host.handle(message);
                // whatever else came meanwhile, before drawing
                while let Ok(message) = rx.try_recv() {
                    host.handle(message);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
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
        assert_eq!(menu.children[0], Node::Text { text: "0".into(), style: String::new() });
        let more = key_of(&menu, "More");
        let menu = menu_of(&ui(&mut host, "counter", &more, "click", None));
        assert_eq!(menu.children[0], Node::Text { text: "1".into(), style: String::new() });
        // stored: a host started later (another account, or after a restart) sees it
        let mut again = Host::new("alt1");
        again.scan(true);
        let menu = menu_of(&ui(&mut again, "counter", "", "open", None));
        assert_eq!(menu.children[0], Node::Text { text: "1".into(), style: String::new() });
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
        assert_eq!(menu_of(&out).children[0], Node::Text { text: "11".into(), style: String::new() });
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
}

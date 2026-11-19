//! The `mace` table a plugin's Luau sees, and what each plugin's state holds.
//! docs/plugins.md describes it for plugin authors; keep the two in step.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::os::fd::AsRawFd;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use mlua::{AppDataRef, Function, Lua, LuaSerdeExt, Table, Value as LuaValue};
use serde_json::{Map, Value, json};

use super::protocol::FromHost;
use crate::{accounts, fsutil, servers};
use mace_proto::{Sort, State};

/// How long a call into a plugin may take before it's stopped.
pub const CALL_LIMIT: Duration = Duration::from_millis(200);
/// A plugin's storage, at most.
const STORAGE_LIMIT: usize = 1 << 20;
/// Timers that repeat run at most this often.
const SHORTEST_EVERY: Duration = Duration::from_millis(100);
pub const EVENTS: [&str; 6] = ["joined", "left", "opened", "closed", "focused", "media"];

pub struct Timer {
    pub id: u64,
    pub due: Instant,
    pub every: Option<Duration>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ButtonDef {
    pub id: String,
    pub icon: String,
    pub tip: String,
    pub badge: Option<u32>,
}

/// One plugin's side of the host, kept in its Luau state's app data. Only
/// ever borrowed shared (`app_data_ref`), so calls back into Luau can't trip
/// over a borrow; what changes is in cells.
pub struct Ctx {
    pub id: String,
    pub name: String,
    pub version: String,
    pub dir: PathBuf,
    pub permissions: Vec<String>,
    /// The account this host is for.
    pub me: String,
    pub world: Rc<RefCell<Option<State>>>,
    pub out: Rc<RefCell<Vec<FromHost>>>,
    pub timers: RefCell<Vec<Timer>>,
    pub next_id: Cell<u64>,
    pub button: RefCell<Option<ButtonDef>>,
    pub menu_open: Cell<bool>,
    /// `mace.refresh()` was called: draw the open menu again.
    pub dirty: Cell<bool>,
    /// When the call in progress has to be done by; None: no call, or waiting on the network.
    pub deadline: Cell<Option<Instant>>,
    /// Pictures already sent to the panel (or, from the web, on their way).
    pub images: RefCell<HashSet<String>>,
    /// The sites it may reach: in its plugin.json, and agreed to when it was turned on.
    pub sites: Vec<String>,
    /// The sites its plugin.json names (more than `sites` when it added some since).
    pub wanted: Vec<String>,
    /// Where requests and pictures that finished are sent, for the host to hand over.
    pub done: std::sync::mpsc::Sender<super::host::Input>,
    pub in_flight: Cell<usize>,
    /// When its last requests were made, for the per-minute limit.
    pub recent: RefCell<std::collections::VecDeque<Instant>>,
    /// The menu last sent, so an unchanged one isn't sent again.
    pub last_menu: RefCell<Option<mace_proto::Menu>>,
    /// The room its menu has (points), as the panel last said; 0 until it does.
    pub size: Cell<(f64, f64)>,
}

/// Requests a plugin may have going at once, and make in a minute.
const MOST_IN_FLIGHT: usize = 6;
const MOST_A_MINUTE: usize = 120;

impl Ctx {
    /// A site the plugin may reach, or why not.
    pub fn may_reach(&self, url: &str) -> mlua::Result<String> {
        let host = super::net::host_of(url).map_err(mlua::Error::runtime)?;
        if super::site_allowed(&self.sites, &host) {
            return Ok(host);
        }
        Err(mlua::Error::runtime(if super::site_allowed(&self.wanted, &host) {
            format!("{host} was added to plugin.json since the plugin was turned on: turn it off and on again in Mace Settings")
        } else {
            format!("{host} isn't one of the plugin's sites: add it to plugin.json's \"http\"")
        }))
    }

    /// Count a request against the limits, or say it's one too many.
    fn may_request(&self) -> mlua::Result<()> {
        let mut recent = self.recent.borrow_mut();
        while recent.front().is_some_and(|t| t.elapsed() > Duration::from_secs(60)) {
            recent.pop_front();
        }
        if self.in_flight.get() >= MOST_IN_FLIGHT {
            return Err(mlua::Error::runtime(format!("at most {MOST_IN_FLIGHT} requests at once")));
        }
        if recent.len() >= MOST_A_MINUTE {
            return Err(mlua::Error::runtime(format!("at most {MOST_A_MINUTE} requests a minute")));
        }
        recent.push_back(Instant::now());
        self.in_flight.set(self.in_flight.get() + 1);
        Ok(())
    }

    /// Fetch a picture from one of its sites, once; the key it'll be known by.
    pub fn web_picture(&self, url: &str) -> mlua::Result<String> {
        self.may_reach(url)?;
        use sha2::Digest;
        let hash = sha2::Sha256::digest(url.as_bytes());
        let key = format!("web:{}", hash.iter().take(12).map(|b| format!("{b:02x}")).collect::<String>());
        if self.images.borrow_mut().insert(key.clone()) {
            let (done, url, key, plugin) = (self.done.clone(), url.to_string(), key.clone(), self.id.clone());
            std::thread::spawn(move || {
                let picture = super::net::picture(&url);
                let _ = done.send(super::host::Input::Picture { plugin, key, picture });
            });
        }
        Ok(key)
    }
}

impl Ctx {
    pub fn next(&self) -> u64 {
        let id = self.next_id.get() + 1;
        self.next_id.set(id);
        id
    }

    pub fn send(&self, message: FromHost) {
        self.out.borrow_mut().push(message);
    }

    fn need(&self, permission: &str) -> mlua::Result<()> {
        if self.permissions.iter().any(|p| p == permission) {
            return Ok(());
        }
        Err(mlua::Error::runtime(format!(
            "this needs the \"{permission}\" permission: add it to plugin.json's \"permissions\"")))
    }

    fn act(&self, args: Vec<String>) {
        self.send(FromHost::Act { args });
    }

    /// The network takes as long as it takes: that time doesn't count against the call.
    fn waiting<T>(&self, work: impl FnOnce() -> T) -> T {
        let deadline = self.deadline.take();
        let result = work();
        if deadline.is_some() {
            self.deadline.set(Some(Instant::now() + CALL_LIMIT));
        }
        result
    }
}

pub fn ctx(lua: &Lua) -> mlua::Result<AppDataRef<'_, Ctx>> {
    lua.app_data_ref::<Ctx>().ok_or_else(|| mlua::Error::runtime("no plugin state"))
}

/// JSON to Luau, null as nil.
pub fn to_lua(lua: &Lua, value: &Value) -> mlua::Result<LuaValue> {
    let options = mlua::serde::SerializeOptions::new().serialize_none_to_null(false).serialize_unit_to_null(false)
        .set_array_metatable(false);
    lua.to_value_with(value, options)
}

/// Roblox gives places with a flag emoji in front; the panel's font has no flags.
pub fn plain_location(text: &str) -> String {
    let kept: String = text.chars().filter(|c| !('\u{1F1E6}'..='\u{1F1FF}').contains(c)).collect();
    kept.trim().to_string()
}

pub fn server_json(s: &mace_proto::Server) -> Value {
    json!({
        "place": s.place, "job": s.job, "universe": s.universe, "name": s.name,
        "location": s.location.as_deref().map(plain_location), "ping": s.ping, "since": s.since, "link": s.link,
    })
}

fn account_json(a: &mace_proto::Account) -> Value {
    json!({
        "id": a.id, "name": a.name, "username": a.username, "user": a.user, "running": a.running,
        "server": a.server.as_ref().map(server_json),
    })
}

fn current(ctx: &Ctx) -> Option<mace_proto::Server> {
    let world = ctx.world.borrow();
    world.as_ref()?.accounts.iter().find(|a| a.id == ctx.me)?.server.clone()
}

/// A file of the plugin's own, by a relative name with no way out of its folder.
pub fn own_file(dir: &Path, name: &str) -> mlua::Result<PathBuf> {
    let path = Path::new(name);
    if name.is_empty() || path.components().any(|c| !matches!(c, Component::Normal(_) | Component::CurDir)) {
        return Err(mlua::Error::runtime(format!("\"{name}\" isn't a file in the plugin's folder")));
    }
    Ok(dir.join(path))
}

fn digits(text: &str) -> bool {
    !text.is_empty() && text.len() <= 20 && text.bytes().all(|b| b.is_ascii_digit())
}

fn job_id(text: &str) -> bool {
    !text.is_empty() && text.len() <= 64 && text.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

fn sort(options: &Option<Table>) -> mlua::Result<Sort> {
    match options.as_ref().map(|o| o.get::<Option<String>>("sort")).transpose()?.flatten().as_deref() {
        None | Some("most") => Ok(Sort::Most),
        Some("fewest") => Ok(Sort::Fewest),
        Some(other) => Err(mlua::Error::runtime(format!("sort is \"most\" or \"fewest\", not \"{other}\""))),
    }
}

// --- storage: one JSON object per plugin, shared by every account's host

fn storage_read(id: &str) -> Map<String, Value> {
    match fsutil::read_json(&super::data_file(id)) {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

fn storage_change(id: &str, edit: impl FnOnce(&mut Map<String, Value>)) -> mlua::Result<()> {
    let file = super::data_file(id);
    let work = || -> anyhow::Result<()> {
        let dir = file.parent().expect("in plugin-data");
        std::fs::create_dir_all(dir)?;
        let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true)
            .open(dir.join(format!(".{id}.lock")))?;
        // SAFETY: flock on a descriptor this file owns; let go when it closes.
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
        let mut map = storage_read(id);
        edit(&mut map);
        let text = serde_json::to_vec(&map)?;
        anyhow::ensure!(text.len() <= STORAGE_LIMIT, "a plugin's storage holds at most 1 MB");
        fsutil::write_atomic(&file, &text)
    };
    work().map_err(|e| mlua::Error::runtime(format!("{e:#}")))
}

/// The `mace` global, its parts read-only. Called before the sandbox closes the globals.
pub fn install(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    let mace = lua.create_table()?;
    let (id, name, version, me) = {
        let c = ctx(lua)?;
        (c.id.clone(), c.name.clone(), c.version.clone(), c.me.clone())
    };
    mace.set("api", super::API)?;
    let plugin = lua.create_table()?;
    plugin.set("id", id.as_str())?;
    plugin.set("name", name)?;
    plugin.set("version", version)?;
    mace.set("plugin", plugin)?;
    let account = accounts::accounts().into_iter().find(|a| a.id == me);
    let who = account.as_ref().map(|a| a.identity());
    let me_json = json!({
        "id": me,
        "name": account.as_ref().zip(who.as_ref()).map(|(a, w)| a.name_with(w)).unwrap_or_else(|| me.clone()),
        "username": who.as_ref().map(|w| w.username.clone()).unwrap_or_default(),
        "user": who.as_ref().filter(|w| !w.is_empty()).map(|w| w.id.clone()),
    });
    mace.set("me", to_lua(lua, &me_json)?)?;

    for registry in ["mace_handlers", "mace_timers", "mace_keys", "mace_old_keys", "mace_modules", "mace_http"] {
        lua.set_named_registry_value(registry, lua.create_table()?)?;
    }

    mace.set("on", lua.create_function(|lua, (name, handler): (String, Function)| {
        if !EVENTS.contains(&name.as_str()) {
            return Err(mlua::Error::runtime(format!("no event \"{name}\" (there are: {})", EVENTS.join(", "))));
        }
        let handlers: Table = lua.named_registry_value("mace_handlers")?;
        let list: Table = match handlers.get::<Option<Table>>(name.as_str())? {
            Some(list) => list,
            None => {
                let list = lua.create_table()?;
                handlers.set(name.as_str(), &list)?;
                list
            }
        };
        list.push(handler)
    })?)?;

    let timer = |repeat: bool| {
        move |lua: &Lua, (seconds, callback): (f64, Function)| -> mlua::Result<Table> {
            if !seconds.is_finite() || seconds < 0.0 {
                return Err(mlua::Error::runtime("seconds must be 0 or more"));
            }
            let c = ctx(lua)?;
            let wait = Duration::from_secs_f64(seconds.min(86400.0 * 365.0));
            let every = repeat.then(|| wait.max(SHORTEST_EVERY));
            let id = c.next();
            c.timers.borrow_mut().push(Timer { id, due: Instant::now() + every.unwrap_or(wait), every });
            lua.named_registry_value::<Table>("mace_timers")?.set(id, callback)?;
            let handle = lua.create_table()?;
            handle.set("cancel", lua.create_function(move |lua, _: mlua::MultiValue| {
                ctx(lua)?.timers.borrow_mut().retain(|t| t.id != id);
                lua.named_registry_value::<Table>("mace_timers")?.set(id, LuaValue::Nil)
            })?)?;
            Ok(handle)
        }
    };
    mace.set("after", lua.create_function(timer(false))?)?;
    mace.set("every", lua.create_function(timer(true))?)?;

    mace.set("toast", lua.create_function(|lua, (title, body): (String, Option<String>)| {
        let clip = |s: String| s.chars().take(200).collect::<String>();
        ctx(lua)?.send(FromHost::Toast { title: clip(title), body: clip(body.unwrap_or_default()) });
        Ok(())
    })?)?;

    mace.set("game", lua.create_function(|lua, ()| {
        let c = ctx(lua)?;
        match current(&c) {
            Some(server) => to_lua(lua, &server_json(&server)),
            None => Ok(LuaValue::Nil),
        }
    })?)?;

    mace.set("accounts", lua.create_function(|lua, ()| {
        let c = ctx(lua)?;
        c.need("accounts")?;
        let list: Vec<Value> = c.world.borrow().as_ref().map(|w| w.accounts.iter().map(account_json).collect())
            .unwrap_or_default();
        to_lua(lua, &Value::Array(list))
    })?)?;

    mace.set("join", lua.create_function(|lua, (place, job): (LuaValue, Option<String>)| {
        let c = ctx(lua)?;
        c.need("join")?;
        let place = match place {
            LuaValue::Integer(n) => n.to_string(),
            LuaValue::Number(n) if n.fract() == 0.0 => format!("{n:.0}"),
            LuaValue::String(s) => s.to_str()?.to_string(),
            _ => String::new(),
        };
        if !digits(&place) || job.as_deref().is_some_and(|j| !job_id(j)) {
            return Err(mlua::Error::runtime("join(place, job?): a place id, and maybe a server's job id"));
        }
        let target = match job {
            Some(job) => format!("{place} {job}"),
            None => place,
        };
        c.act(vec!["start".into(), c.me.clone(), target]);
        Ok(())
    })?)?;

    mace.set("rejoin", lua.create_function(|lua, ()| {
        let c = ctx(lua)?;
        c.need("join")?;
        c.act(vec!["rejoin".into(), c.me.clone()]);
        Ok(())
    })?)?;

    let page_of = |lua: &Lua, page: &mace_proto::ServerPage| -> mlua::Result<LuaValue> {
        let servers: Vec<Value> = page.list.iter().map(|s| json!({
            "id": s.id, "playing": s.playing, "max": s.max, "ping": s.ping,
            "location": s.location.as_deref().map(plain_location),
        })).collect();
        to_lua(lua, &json!({ "servers": servers, "next": page.next,
                             "error": (!page.error.is_empty()).then_some(&page.error) }))
    };
    mace.set("servers", lua.create_function(move |lua, options: Option<Table>| {
        let c = ctx(lua)?;
        let place = match options.as_ref().map(|o| o.get::<Option<String>>("place")).transpose()?.flatten() {
            Some(place) => place,
            None => current(&c).map(|s| s.place).ok_or_else(|| mlua::Error::runtime("not in a game, and no place given"))?,
        };
        let cursor = options.as_ref().map(|o| o.get::<Option<String>>("cursor")).transpose()?.flatten();
        let sort = sort(&options)?;
        let page = c.waiting(|| servers::public_page(&place, sort, cursor.as_deref()));
        page_of(lua, &page)
    })?)?;

    mace.set("hop", lua.create_function(|lua, options: Option<Table>| {
        let c = ctx(lua)?;
        c.need("join")?;
        let Some(here) = current(&c) else { return Ok(false) };
        let sort = sort(&options)?;
        let page = c.waiting(|| servers::public_page(&here.place, sort, None));
        let Some(next) = page.list.iter().find(|s| s.id != here.job && s.playing < s.max && s.playing > 0) else {
            return Ok(false);
        };
        c.act(vec!["start".into(), c.me.clone(), format!("{} {}", here.place, next.id)]);
        Ok(true)
    })?)?;

    mace.set("copy", lua.create_function(|lua, text: String| {
        let c = ctx(lua)?;
        c.need("clipboard")?;
        c.send(FromHost::Copy { text: text.chars().take(10_000).collect() });
        Ok(())
    })?)?;

    mace.set("start", lua.create_function(|lua, (account, link): (String, Option<String>)| {
        let c = ctx(lua)?;
        c.need("accounts")?;
        let mut args = vec!["start".to_string(), account];
        args.extend(link);
        c.act(args);
        Ok(())
    })?)?;
    mace.set("stop", lua.create_function(|lua, account: String| {
        let c = ctx(lua)?;
        c.need("accounts")?;
        c.act(vec!["stop".into(), account]);
        Ok(())
    })?)?;
    mace.set("gather", lua.create_function(|lua, ()| {
        let c = ctx(lua)?;
        c.need("accounts")?;
        c.act(vec!["gather".into(), c.me.clone()]);
        Ok(())
    })?)?;

    let storage = lua.create_table()?;
    storage.set("get", lua.create_function(|lua, key: String| {
        let id = ctx(lua)?.id.clone();
        to_lua(lua, storage_read(&id).get(&key).unwrap_or(&Value::Null))
    })?)?;
    storage.set("set", lua.create_function(|lua, (key, value): (String, LuaValue)| {
        let id = ctx(lua)?.id.clone();
        let value: Value = if value.is_nil() { Value::Null } else { lua.from_value(value)? };
        storage_change(&id, |map| {
            if value.is_null() {
                map.remove(&key);
            } else {
                map.insert(key, value);
            }
        })
    })?)?;
    storage.set("keys", lua.create_function(|lua, ()| {
        let id = ctx(lua)?.id.clone();
        let mut keys: Vec<String> = storage_read(&id).keys().cloned().collect();
        keys.sort();
        Ok(keys)
    })?)?;
    storage.set_readonly(true);
    mace.set("storage", storage)?;

    let bar = lua.create_table()?;
    bar.set("button", lua.create_function(|lua, def: Table| {
        let c = ctx(lua)?;
        if c.button.borrow().is_some() {
            return Err(mlua::Error::runtime("a plugin has one bar button"));
        }
        let icon: String = def.get::<Option<String>>("icon")?.unwrap_or_else(|| "extension".into());
        let made = ButtonDef {
            id: def.get::<Option<String>>("id")?.unwrap_or_else(|| "main".into()),
            icon: super::ui::icon(lua, &c, &icon)?,
            tip: def.get::<Option<String>>("tip")?.unwrap_or_else(|| c.name.clone()),
            badge: def.get::<Option<u32>>("badge")?,
        };
        *c.button.borrow_mut() = Some(made);
        if let Some(menu) = def.get::<Option<Function>>("menu")? {
            lua.set_named_registry_value("mace_menu", menu)?;
        }
        let handle = lua.create_table()?;
        handle.set("set", lua.create_function(|lua, (_this, changes): (LuaValue, Table)| {
            let c = ctx(lua)?;
            let icon = changes.get::<Option<String>>("icon")?.map(|i| super::ui::icon(lua, &c, &i)).transpose()?;
            let tip = changes.get::<Option<String>>("tip")?;
            let badge = changes.get::<Option<u32>>("badge")?;
            if let Some(button) = c.button.borrow_mut().as_mut() {
                if let Some(icon) = icon {
                    button.icon = icon;
                }
                if let Some(tip) = tip {
                    button.tip = tip;
                }
                if let Some(badge) = badge {
                    button.badge = (badge > 0).then_some(badge);
                }
            }
            Ok(())
        })?)?;
        Ok(handle)
    })?)?;
    bar.set("close", lua.create_function(|lua, ()| {
        ctx(lua)?.send(FromHost::Close);
        Ok(())
    })?)?;
    bar.set_readonly(true);
    mace.set("bar", bar)?;

    // --- the web: only the plugin's sites, and the answer comes later, to a callback
    let http = lua.create_table()?;
    let start = |lua: &Lua, request: super::net::Request, callback: Function| -> mlua::Result<()> {
        let c = ctx(lua)?;
        c.may_reach(&request.url)?;
        c.may_request()?;
        let id = c.next();
        lua.named_registry_value::<Table>("mace_http")?.set(id, callback)?;
        let (done, plugin) = (c.done.clone(), c.id.clone());
        std::thread::spawn(move || {
            let response = super::net::fetch(&request);
            let _ = done.send(super::host::Input::Http { plugin, id, response });
        });
        Ok(())
    };
    http.set("get", lua.create_function(move |lua, (url, callback): (String, Function)| {
        start(lua, super::net::Request { url, method: "GET".into(), headers: Vec::new(), body: None }, callback)
    })?)?;
    http.set("request", lua.create_function(move |lua, (options, callback): (Table, Function)| {
        let url: String = options.get("url")?;
        let method = options.get::<Option<String>>("method")?.unwrap_or_else(|| "GET".into()).to_ascii_uppercase();
        if !["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"].contains(&method.as_str()) {
            return Err(mlua::Error::runtime(format!("no method {method}")));
        }
        let mut headers = Vec::new();
        if let Some(table) = options.get::<Option<Table>>("headers")? {
            for pair in table.pairs::<String, String>() {
                headers.push(pair?);
            }
        }
        let body = options.get::<Option<String>>("body")?;
        start(lua, super::net::Request { url, method, headers, body }, callback)
    })?)?;
    http.set_readonly(true);
    mace.set("http", http)?;

    let json_table = lua.create_table()?;
    json_table.set("encode", lua.create_function(|lua, value: LuaValue| {
        let value: Value = lua.from_value(value)?;
        serde_json::to_string(&value).map_err(|e| mlua::Error::runtime(e.to_string()))
    })?)?;
    json_table.set("decode", lua.create_function(|lua, text: String| {
        let value: Value = serde_json::from_str(&text).map_err(|e| mlua::Error::runtime(format!("not JSON: {e}")))?;
        to_lua(lua, &value)
    })?)?;
    json_table.set_readonly(true);
    mace.set("json", json_table)?;

    // --- what's playing (MPRIS)
    let media = lua.create_table()?;
    media.set("current", lua.create_function(|lua, player: Option<String>| {
        let c = ctx(lua)?;
        c.need("media")?;
        match c.waiting(|| super::media::current(player.as_deref())) {
            Some(now) => to_lua(lua, &now),
            None => Ok(LuaValue::Nil),
        }
    })?)?;
    media.set("players", lua.create_function(|lua, ()| {
        let c = ctx(lua)?;
        c.need("media")?;
        Ok(c.waiting(super::media::players))
    })?)?;
    for (name, action) in [("playPause", "PlayPause"), ("play", "Play"), ("pause", "Pause"), ("next", "Next"),
                           ("previous", "Previous")] {
        media.set(name, lua.create_function(move |lua, player: Option<String>| {
            let c = ctx(lua)?;
            c.need("media")?;
            Ok(c.waiting(|| super::media::control(player.as_deref(), action, None)))
        })?)?;
    }
    media.set("seek", lua.create_function(|lua, (seconds, player): (f64, Option<String>)| {
        let c = ctx(lua)?;
        c.need("media")?;
        Ok(c.waiting(|| super::media::control(player.as_deref(), "seek", Some(seconds))))
    })?)?;
    media.set_readonly(true);
    mace.set("media", media)?;

    mace.set("refresh", lua.create_function(|lua, ()| {
        ctx(lua)?.dirty.set(true);
        Ok(())
    })?)?;

    let ui: Table = lua.load(super::ui::PRELUDE).set_name("=mace.ui").eval()?;
    ui.set_readonly(true);
    mace.set("ui", ui)?;
    mace.set_readonly(true);
    globals.set("mace", &mace)?;

    globals.set("print", lua.create_function(|lua, args: mlua::MultiValue| {
        let text = args.iter().map(|v| v.to_string().unwrap_or_else(|_| "?".into())).collect::<Vec<_>>().join(" ");
        let c = ctx(lua)?;
        c.send(FromHost::Log { text: format!("plugin {}: {}", c.id, text.chars().take(500).collect::<String>()) });
        Ok(())
    })?)?;

    // "@mace", or a module of the plugin's own ("util" or "./util" is util.luau next to the script)
    globals.set("require", lua.create_function(|lua, name: String| -> mlua::Result<LuaValue> {
        if name == "@mace" {
            return lua.globals().get("mace");
        }
        let modules: Table = lua.named_registry_value("mace_modules")?;
        if let Some(done) = modules.get::<Option<LuaValue>>(name.as_str())? {
            return Ok(done);
        }
        let file = name.strip_prefix("./").unwrap_or(&name);
        let file = if file.ends_with(".luau") || file.ends_with(".lua") { file.to_string() } else { format!("{file}.luau") };
        let path = own_file(&ctx(lua)?.dir, &file)?;
        let source = std::fs::read_to_string(&path)
            .map_err(|_| mlua::Error::runtime(format!("require: no {file} in the plugin's folder")))?;
        let value: LuaValue = lua.load(source).set_name(format!("@{file}")).eval()?;
        let value = if value.is_nil() { LuaValue::Boolean(true) } else { value };
        modules.set(name.as_str(), &value)?;
        Ok(value)
    })?)?;
    Ok(())
}

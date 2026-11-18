//! Mace's settings: one global set, and per-account overrides on top.
//!
//! Stored as JSON in ~/.config/mace/settings.json. Anything missing or of the
//! wrong kind falls back to its default, so an old or hand-edited file always
//! loads.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{fsutil, paths};

pub const PRESENTATION_MODES: [&str; 5] = ["", "immediate", "mailbox", "fifo", "fifo_relaxed"];
pub const DECORATIONS: [&str; 2] = ["server", "client"];
pub const MENUS: [&str; 3] = ["none", "strip", "titlebar"];

/// Set by Mace for every account; an extra variable may not move them.
const RESERVED_ENV: [&str; 2] = ["ROSEMARY_PREFIX", "ROSEMARY_PREFIX_HOME"];

static ENV_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").unwrap());
static FLAG_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_]+$").unwrap());
static CORES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d+(-\d+)?(,\d+(-\d+)?)*$").unwrap());
static WHOLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^-?\d+$").unwrap());

/// What one account may set for itself, over the shared settings.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Overrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presentation_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_rate: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps_cap: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_cores: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gamemode: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mangohud: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fast_flags: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_env: Option<Map<String, Value>>,
}

impl Overrides {
    pub fn is_empty(&self) -> bool {
        *self == Overrides::default()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// "auto" is the newest Rosemary runner Butter installed; or a path to one.
    pub rosemary: String,
    /// Vulkan presentation for Rosemary's Metal: immediate, mailbox, fifo, or "" for Rosemary's default.
    pub presentation_mode: String,
    /// Hz Roblox is told the screen runs at; Roblox caps its FPS there. 0 = the real one.
    pub refresh_rate: u64,
    /// Roblox's own frame-rate setting (FramerateCap); 0 leaves what the account has.
    pub fps_cap: u64,
    /// Who draws the title bar: server (your desktop) or client.
    pub decorations: String,
    /// Roblox's Mac menu bar: none, strip, titlebar.
    pub menu: String,
    /// Ctrl+C/V/X/A/Z work as Cmd in text fields.
    pub control_edit_keys: bool,
    pub confine_pointer: bool,
    pub gamemode: bool,
    pub mangohud: bool,
    /// taskset list such as "1-5,7-11"; "" = every core.
    pub cpu_cores: String,
    /// A command around Roblox, e.g. "gamescope -f --"; %command% marks the spot.
    pub wrapper: String,
    pub fast_flags: Map<String, Value>,
    pub extra_env: Map<String, Value>,
    /// A window with progress while an account starts.
    pub loading_screen: bool,
    /// The in-game panel, drawn inside Roblox.
    pub panel: bool,
    /// Blur the game behind the open panel; otherwise it's only dimmed.
    pub panel_blur: bool,
    /// Frames per second for Roblox windows you aren't using (all but the last one focused); 0 = no limit.
    pub background_fps: u64,
    /// Nudge idle games now and then so Roblox doesn't kick them after 20 minutes.
    /// Off unless turned on, since it sends input to the game. Saved under a new
    /// name: 0.1.0 had it on by default and saved that as `anti_afk`.
    #[serde(rename = "anti_afk_opt_in")]
    pub anti_afk: bool,
    /// Ask GitHub for a newer Mace when it starts and every hour (only the copy install.sh puts in ~/.local/bin updates itself).
    pub check_updates: bool,
    /// The in-game panel's key, e.g. "INSERT" or "SHIFT + F1" (the game has to receive it).
    pub overlay_key: String,
    /// Account id -> its own values.
    pub accounts: Map<String, Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            rosemary: "auto".into(),
            presentation_mode: "immediate".into(),
            refresh_rate: 9999,
            fps_cap: 0,
            decorations: "server".into(),
            menu: "none".into(),
            control_edit_keys: true,
            confine_pointer: false,
            gamemode: true,
            mangohud: false,
            cpu_cores: String::new(),
            wrapper: String::new(),
            fast_flags: Map::new(),
            extra_env: Map::new(),
            loading_screen: true,
            panel: true,
            panel_blur: false,
            background_fps: 10,
            anti_afk: false,
            check_updates: true,
            overlay_key: "INSERT".into(),
            accounts: Map::new(),
        }
    }
}

/// The settings one account runs with.
#[derive(Clone, Debug)]
pub struct Effective {
    pub rosemary: String,
    pub presentation_mode: String,
    pub refresh_rate: u64,
    pub fps_cap: u64,
    pub decorations: String,
    pub menu: String,
    pub control_edit_keys: bool,
    pub confine_pointer: bool,
    pub gamemode: bool,
    pub mangohud: bool,
    pub cpu_cores: String,
    pub wrapper: String,
    pub fast_flags: Map<String, Value>,
    pub extra_env: Map<String, Value>,
}

fn as_u64(value: Option<&Value>, default: u64) -> u64 {
    match value {
        Some(Value::Number(n)) => n.as_u64().or_else(|| n.as_f64().map(|f| f.max(0.0) as u64)).unwrap_or(default),
        Some(Value::String(s)) => s.trim().parse::<u64>().unwrap_or(default),
        Some(Value::Bool(b)) => *b as u64,
        _ => default,
    }
}

fn truthy(value: Option<&Value>, default: bool) -> bool {
    match value {
        None | Some(Value::Null) => default,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Variables as Rosemary will see them: names checked, values trimmed (a space
/// typed after `=` would otherwise become part of the value).
pub fn clean_env(env: Option<&Value>) -> Map<String, Value> {
    let mut out = Map::new();
    if let Some(Value::Object(map)) = env {
        for (name, value) in map {
            let name = name.trim();
            if !ENV_NAME.is_match(name) || RESERVED_ENV.contains(&name) {
                continue;
            }
            let value = text(Some(value));
            let value = value.trim();
            if value.contains('\n') || value.contains('\0') {
                continue;
            }
            out.insert(name.to_string(), Value::String(value.to_string()));
        }
    }
    out
}

pub fn clean_flags(flags: Option<&Value>) -> Map<String, Value> {
    let mut out = Map::new();
    if let Some(Value::Object(map)) = flags {
        for (name, value) in map {
            let name = name.trim();
            if name.is_empty() || !FLAG_NAME.is_match(name) {
                continue;
            }
            let value = match value {
                Value::String(s) => parse_flag_value(s),
                other => other.clone(),
            };
            out.insert(name.to_string(), value);
        }
    }
    out
}

/// How a value typed into the FastFlags editor is stored: true/false and whole
/// numbers as JSON values, anything else as a string.
pub fn parse_flag_value(text: &str) -> Value {
    let text = text.trim();
    match text.to_lowercase().as_str() {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        _ => {}
    }
    if WHOLE.is_match(text) {
        if let Ok(n) = text.parse::<i64>() {
            return Value::from(n);
        }
    }
    Value::String(text.to_string())
}

pub fn valid_cores(text: &str) -> bool {
    text.is_empty() || CORES.is_match(&text.replace(' ', ""))
}

fn choice(value: Option<&Value>, allowed: &[&str], default: &str) -> String {
    match value {
        Some(Value::String(s)) if allowed.contains(&s.as_str()) => s.clone(),
        _ => default.to_string(),
    }
}

impl Settings {
    /// Settings from any JSON value, every field checked.
    pub fn from_value(value: &Value) -> Settings {
        let d = Settings::default();
        let get = |key: &str| value.get(key);
        let mut cores = text(get("cpu_cores")).replace(' ', "");
        if !valid_cores(&cores) {
            cores.clear();
        }
        let rosemary = text(get("rosemary"));
        let mut accounts = Map::new();
        if let Some(Value::Object(map)) = get("accounts") {
            for (id, overrides) in map {
                if overrides.is_object() {
                    accounts.insert(id.clone(), serde_json::to_value(clean_overrides(overrides)).unwrap());
                }
            }
        }
        Settings {
            rosemary: if rosemary.is_empty() { d.rosemary } else { rosemary },
            presentation_mode: choice(get("presentation_mode"), &PRESENTATION_MODES, &d.presentation_mode),
            refresh_rate: as_u64(get("refresh_rate"), d.refresh_rate),
            fps_cap: as_u64(get("fps_cap"), 0),
            decorations: choice(get("decorations"), &DECORATIONS, &d.decorations),
            menu: choice(get("menu"), &MENUS, &d.menu),
            control_edit_keys: truthy(get("control_edit_keys"), false),
            confine_pointer: truthy(get("confine_pointer"), false),
            gamemode: truthy(get("gamemode"), false),
            mangohud: truthy(get("mangohud"), false),
            cpu_cores: cores,
            wrapper: text(get("wrapper")).trim().to_string(),
            fast_flags: clean_flags(get("fast_flags")),
            extra_env: clean_env(get("extra_env")),
            loading_screen: truthy(get("loading_screen"), d.loading_screen),
            panel: truthy(get("panel"), d.panel),
            panel_blur: truthy(get("panel_blur"), d.panel_blur),
            background_fps: as_u64(get("background_fps"), d.background_fps).min(1000),
            anti_afk: truthy(get("anti_afk_opt_in"), d.anti_afk),
            check_updates: truthy(get("check_updates"), d.check_updates),
            overlay_key: text(get("overlay_key")).trim().to_string(),
            accounts,
        }
    }

    /// Defaults with the stored values on top; keys the file lacks keep their defaults.
    pub fn from_stored(stored: &Value) -> Settings {
        let mut merged = serde_json::to_value(Settings::default()).unwrap();
        if let (Value::Object(base), Value::Object(stored)) = (&mut merged, stored) {
            for (key, value) in stored {
                if base.contains_key(key) {
                    base.insert(key.clone(), value.clone());
                }
            }
        }
        Settings::from_value(&merged)
    }

    pub fn overrides(&self, account_id: &str) -> Overrides {
        self.accounts.get(account_id).map(clean_overrides).unwrap_or_default()
    }

    pub fn set_overrides(&mut self, account_id: &str, overrides: &Overrides) {
        if overrides.is_empty() {
            self.accounts.remove(account_id);
        } else {
            self.accounts.insert(account_id.to_string(), serde_json::to_value(overrides).unwrap());
        }
    }

    /// The settings one account runs with: the global ones, its overrides on top.
    pub fn for_account(&self, account_id: &str) -> Effective {
        let o = self.overrides(account_id);
        let mut fast_flags = self.fast_flags.clone();
        fast_flags.extend(o.fast_flags.unwrap_or_default());
        let mut extra_env = self.extra_env.clone();
        extra_env.extend(o.extra_env.unwrap_or_default());
        Effective {
            rosemary: self.rosemary.clone(),
            presentation_mode: o.presentation_mode.unwrap_or_else(|| self.presentation_mode.clone()),
            refresh_rate: o.refresh_rate.unwrap_or(self.refresh_rate),
            fps_cap: o.fps_cap.unwrap_or(self.fps_cap),
            decorations: self.decorations.clone(),
            menu: self.menu.clone(),
            control_edit_keys: self.control_edit_keys,
            confine_pointer: self.confine_pointer,
            gamemode: o.gamemode.unwrap_or(self.gamemode),
            mangohud: o.mangohud.unwrap_or(self.mangohud),
            cpu_cores: o.cpu_cores.unwrap_or_else(|| self.cpu_cores.clone()),
            wrapper: self.wrapper.clone(),
            fast_flags,
            extra_env,
        }
    }
}

fn clean_overrides(value: &Value) -> Overrides {
    let mut o = Overrides::default();
    let get = |key: &str| value.get(key).filter(|v| !v.is_null());
    if let Some(Value::String(mode)) = get("presentation_mode") {
        if PRESENTATION_MODES.contains(&mode.as_str()) {
            o.presentation_mode = Some(mode.clone());
        }
    }
    if let Some(v) = get("refresh_rate") {
        o.refresh_rate = Some(as_u64(Some(v), 0));
    }
    if let Some(v) = get("fps_cap") {
        o.fps_cap = Some(as_u64(Some(v), 0));
    }
    if let Some(v) = get("cpu_cores") {
        let cores = text(Some(v)).replace(' ', "");
        if valid_cores(&cores) {
            o.cpu_cores = Some(cores);
        }
    }
    if let Some(v) = get("gamemode") {
        o.gamemode = Some(truthy(Some(v), false));
    }
    if let Some(v) = get("mangohud") {
        o.mangohud = Some(truthy(Some(v), false));
    }
    if let Some(v) = get("fast_flags") {
        o.fast_flags = Some(clean_flags(Some(v)));
    }
    if let Some(v) = get("extra_env") {
        o.extra_env = Some(clean_env(Some(v)));
    }
    o
}

pub fn load_from(path: &Path) -> Settings {
    let stored = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| Value::Object(Map::new()));
    Settings::from_stored(&stored)
}

pub fn load() -> Settings {
    load_from(&paths::settings_file())
}

pub fn save_to(settings: &Settings, path: &Path) -> anyhow::Result<()> {
    let clean = Settings::from_value(&serde_json::to_value(settings)?);
    let mut text = serde_json::to_string_pretty(&clean)?;
    text.push('\n');
    fsutil::write_atomic(path, text.as_bytes())
}

pub fn save(settings: &Settings) -> anyhow::Result<()> {
    save_to(settings, &paths::settings_file())
}

pub fn exists() -> bool {
    paths::settings_file().exists()
}

/// Mace settings matching a Nutmeg config.json (its settings file, read as data).
pub fn from_nutmeg(config: &Value) -> Settings {
    let get = |key: &str| config.get(key);
    let mut s = serde_json::to_value(Settings::default()).unwrap();
    let set = |s: &mut Value, key: &str, value: Value| {
        s.as_object_mut().unwrap().insert(key.to_string(), value);
    };
    if let Some(Value::String(path)) = get("rosemary_path") {
        if !path.is_empty() {
            set(&mut s, "rosemary", Value::String(path.clone()));
        }
    }
    set(&mut s, "presentation_mode", Value::String(text(get("metal_presentation_mode"))));
    set(&mut s, "refresh_rate", Value::from(as_u64(get("screen_refresh_rate"), 0)));
    for key in ["decorations", "menu"] {
        if let Some(Value::String(v)) = get(key) {
            if !v.is_empty() {
                set(&mut s, key, Value::String(v.clone()));
            }
        }
    }
    let defaults = Settings::default();
    set(&mut s, "control_edit_keys", Value::Bool(truthy(get("control_edit_keys"), defaults.control_edit_keys)));
    set(&mut s, "confine_pointer", Value::Bool(truthy(get("confine_pointer"), false)));
    set(&mut s, "gamemode", Value::Bool(truthy(get("use_gamemode"), false)));
    set(&mut s, "mangohud", Value::Bool(truthy(get("use_mangohud"), false)));
    set(&mut s, "wrapper", Value::String(text(get("wrapper_command"))));
    set(&mut s, "fast_flags", get("fast_flags").cloned().unwrap_or(Value::Object(Map::new())));
    set(&mut s, "extra_env", get("extra_env").cloned().unwrap_or(Value::Object(Map::new())));
    Settings::from_value(&s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn anti_afk_is_off_until_chosen() {
        assert!(!Settings::default().anti_afk);
        // 0.1.0 saved its on-by-default value under the old name: not a choice, so it's dropped
        assert!(!Settings::from_stored(&json!({ "anti_afk": true })).anti_afk);
        let chosen = Settings::from_stored(&json!({ "anti_afk_opt_in": true }));
        assert!(chosen.anti_afk);
        assert_eq!(serde_json::to_value(&chosen).unwrap()["anti_afk_opt_in"], json!(true));
    }

    #[test]
    fn defaults_when_missing() {
        let s = Settings::from_stored(&json!({}));
        assert_eq!(s.rosemary, "auto");
        assert_eq!(s.refresh_rate, 9999);
        assert!(s.accounts.is_empty());
    }

    #[test]
    fn env_values_are_trimmed_and_checked() {
        let env = clean_env(Some(&json!({" MANGOHUD ": " 1", "SDL_AUDIO_DRIVER": "pulseaudio ", "BAD NAME": "x",
                                         "ROSEMARY_PREFIX": "/elsewhere", "MULTI": "a\nb"})));
        assert_eq!(Value::Object(env), json!({"MANGOHUD": "1", "SDL_AUDIO_DRIVER": "pulseaudio"}));
    }

    #[test]
    fn bad_values_fall_back() {
        let s = Settings::from_stored(&json!({"presentation_mode": "sideways", "cpu_cores": "1-x",
                                              "refresh_rate": "fast", "menu": "?"}));
        assert_eq!(s.presentation_mode, "immediate");
        assert_eq!(s.cpu_cores, "");
        assert_eq!(s.refresh_rate, 9999);
        assert_eq!(s.menu, "none");
    }

    #[test]
    fn account_overrides_merge() {
        let s = Settings::from_stored(&json!({
            "fast_flags": {"A": 1, "B": 2}, "extra_env": {"X": "1"},
            "accounts": {"alt1": {"fps_cap": 30, "fast_flags": {"B": 3}, "extra_env": {"Y": "2"}}}
        }));
        let alt = s.for_account("alt1");
        assert_eq!(alt.fps_cap, 30);
        assert_eq!(Value::Object(alt.fast_flags), json!({"A": 1, "B": 3}));
        assert_eq!(Value::Object(alt.extra_env), json!({"X": "1", "Y": "2"}));
        assert_eq!(s.for_account("main").fps_cap, 0);
    }

    #[test]
    fn flag_values() {
        assert_eq!(parse_flag_value("True"), json!(true));
        assert_eq!(parse_flag_value("9999"), json!(9999));
        assert_eq!(parse_flag_value(" Vulkan "), json!("Vulkan"));
    }

    #[test]
    fn from_nutmeg_maps_its_keys() {
        let s = from_nutmeg(&json!({"metal_presentation_mode": "mailbox", "screen_refresh_rate": "144",
                                    "use_gamemode": true, "use_mangohud": false, "decorations": "client",
                                    "extra_env": {"A": "1 "}, "fast_flags": {"F": 1}}));
        assert_eq!((s.presentation_mode.as_str(), s.refresh_rate, s.decorations.as_str()), ("mailbox", 144, "client"));
        assert!(s.gamemode);
        assert_eq!(Value::Object(s.extra_env), json!({"A": "1"}));
    }

    #[test]
    fn round_trip_through_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut s = Settings::default();
        s.fps_cap = 500;
        s.set_overrides("alt1", &Overrides { fps_cap: Some(60), cpu_cores: Some("6,7".into()), ..Default::default() });
        save_to(&s, &path).unwrap();
        let again = load_from(&path);
        assert_eq!(again.fps_cap, 500);
        assert_eq!(again.accounts.get("alt1").unwrap(), &json!({"fps_cap": 60, "cpu_cores": "6,7"}));
    }
}

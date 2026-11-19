//! Plugins: Luau scripts that add buttons and menus to the in-game bar and act
//! on what happens in the game. Each one is a folder in
//! `~/.local/share/mace/plugins/<id>/` with a `plugin.json` and its script; its
//! data lives apart, in `plugin-data/<id>.json`, so reinstalling keeps it.
//!
//! They run in `mace plugin-host`, a process each running account's
//! supervisor starts (never inside Roblox), each plugin in a sandboxed Luau
//! state of its own. docs/plugins.md is the reference for plugin authors.

pub mod api;
pub mod host;
pub mod protocol;
pub mod ui;

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::{fsutil, paths, settings};

/// The plugin API this Mace offers; a plugin names the one it was written for.
pub const API: u32 = 1;

/// What a plugin may be allowed beyond the basics (toasts, its own storage,
/// timers, its bar button, and this account's game).
pub const PERMISSIONS: [(&str, &str); 3] = [
    ("join", "Send this account to other servers"),
    ("accounts", "See your other accounts, and start, stop or gather them"),
    ("clipboard", "Copy text"),
];

pub fn dir() -> PathBuf {
    paths::data().join("plugins")
}

pub fn data_file(id: &str) -> PathBuf {
    paths::data().join("plugin-data").join(format!("{id}.json"))
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    pub api: u32,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_main")]
    pub main: String,
    #[serde(default)]
    pub permissions: Vec<String>,
}

fn default_main() -> String {
    "init.luau".into()
}

/// A plugin folder, and its manifest or why it can't be used.
#[derive(Debug, Clone)]
pub struct Found {
    pub id: String,
    pub dir: PathBuf,
    pub manifest: Result<Manifest, String>,
}

impl Found {
    pub fn name(&self) -> &str {
        self.manifest.as_ref().map_or(&self.id, |m| &m.name)
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 48 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Read and check a plugin folder's manifest.
pub fn read_manifest(dir: &Path) -> Result<Manifest, String> {
    let id = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let text = std::fs::read_to_string(dir.join("plugin.json")).map_err(|_| "No plugin.json".to_string())?;
    let manifest: Manifest = serde_json::from_str(&text).map_err(|e| format!("plugin.json: {e}"))?;
    if manifest.id != id {
        return Err(format!("plugin.json says id \"{}\", but the folder is \"{id}\"", manifest.id));
    }
    if !valid_id(&manifest.id) {
        return Err("The id may only have a-z, 0-9 and -".into());
    }
    if manifest.api != API {
        return Err(format!("Made for plugin API {}; this Mace has API {API}", manifest.api));
    }
    if let Some(unknown) = manifest.permissions.iter().find(|p| !PERMISSIONS.iter().any(|(k, _)| k == p)) {
        return Err(format!("Unknown permission \"{unknown}\""));
    }
    let main = Path::new(&manifest.main);
    if main.is_absolute() || main.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err("main must be a file in the plugin's folder".into());
    }
    if !dir.join(main).is_file() {
        return Err(format!("No {}", manifest.main));
    }
    Ok(manifest)
}

/// Every plugin folder, by id.
pub fn list() -> Vec<Found> {
    let mut found: Vec<Found> = std::fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| {
            let dir = e.path();
            Found { id: e.file_name().to_string_lossy().into_owned(), manifest: read_manifest(&dir), dir }
        })
        .collect();
    found.sort_by(|a, b| a.id.cmp(&b.id));
    found
}

pub fn is_on(id: &str) -> bool {
    settings::load().plugins.get(id).and_then(Value::as_bool) == Some(true)
}

pub fn set_on(id: &str, on: bool) -> anyhow::Result<()> {
    let mut s = settings::load();
    if on {
        s.plugins.insert(id.to_string(), Value::Bool(true));
    } else {
        s.plugins.remove(id);
    }
    settings::save(&s)
}

/// The plain words for a plugin's permissions, for asking before turning it on.
pub fn describe_permissions(manifest: &Manifest) -> Vec<&'static str> {
    manifest.permissions.iter().filter_map(|p| PERMISSIONS.iter().find(|(k, _)| k == p).map(|(_, words)| *words)).collect()
}

/// `mace plugins new ID`: a plugin that works, to start from.
pub fn create(id: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(valid_id(id), "a plugin id may only have a-z, 0-9 and -");
    let dir = dir().join(id);
    anyhow::ensure!(!dir.exists(), "{} exists already", dir.display());
    std::fs::create_dir_all(&dir)?;
    let name = id.split('-').map(|w| {
        let mut chars = w.chars();
        chars.next().map(|c| c.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default()
    }).collect::<Vec<_>>().join(" ");
    let manifest = serde_json::json!({
        "id": id, "name": name, "version": "0.1.0", "api": API, "author": "",
        "description": "", "main": "init.luau", "permissions": [],
    });
    fsutil::write_atomic(&dir.join("plugin.json"), (serde_json::to_string_pretty(&manifest)? + "\n").as_bytes())?;
    fsutil::write_atomic(&dir.join("init.luau"), TEMPLATE.replace("NAME", &name).as_bytes())?;
    Ok(dir)
}

const TEMPLATE: &str = r#"-- NAME: a Mace plugin. The reference: docs/plugins.md in Mace's repository.
-- Save this file and the plugin reloads in your game.
local ui = mace.ui

local clicks = mace.storage.get("clicks") or 0

mace.bar.button({
    id = "main",
    icon = "star",
    tip = "NAME",
    menu = function()
        local game = mace.game()
        return ui.menu({ title = "NAME", subtitle = if game then game.name else "Not in a game" }, {
            ui.text(`Clicked {clicks} times`, { style = "subtle" }),
            ui.button("Click me", { style = "primary", onClick = function()
                clicks += 1
                mace.storage.set("clicks", clicks)
            end }),
        })
    end,
})

mace.on("joined", function(server)
    mace.toast("Hello from NAME", server.name or "a new server")
end)
"#;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::testenv::TempHome;

    pub fn write_plugin(id: &str, manifest: Value, script: &str) -> PathBuf {
        let dir = dir().join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plugin.json"), manifest.to_string()).unwrap();
        std::fs::write(dir.join("init.luau"), script).unwrap();
        dir
    }

    #[test]
    fn manifests_are_checked() {
        let _home = TempHome::new();
        let ok = |id: &str| serde_json::json!({"id": id, "name": "N", "api": 1});
        write_plugin("good", ok("good"), "");
        write_plugin("moved", ok("other"), "");
        write_plugin("newer", serde_json::json!({"id": "newer", "name": "N", "api": 2}), "");
        write_plugin("greedy", serde_json::json!({"id": "greedy", "name": "N", "api": 1, "permissions": ["root"]}), "");
        write_plugin("Caps", ok("Caps"), "");
        let found = list();
        let reason = |id: &str| found.iter().find(|f| f.id == id).unwrap().manifest.clone().err().unwrap_or_default();
        assert_eq!(reason("good"), "");
        assert!(reason("moved").contains("folder is \"moved\""));
        assert!(reason("newer").contains("API 2"));
        assert!(reason("greedy").contains("root"));
        assert!(reason("Caps").contains("a-z"));
    }

    #[test]
    fn a_new_plugin_is_off_until_turned_on() {
        let _home = TempHome::new();
        let dir = create("hello-world").unwrap();
        let manifest = read_manifest(&dir).unwrap();
        assert_eq!(manifest.name, "Hello World");
        assert!(!is_on("hello-world"));
        set_on("hello-world", true).unwrap();
        assert!(is_on("hello-world"));
        set_on("hello-world", false).unwrap();
        assert!(!is_on("hello-world"));
        assert!(create("hello-world").is_err());
        assert!(create("../up").is_err());
    }
}

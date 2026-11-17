//! Menu entries, the icon, and roblox.com's Play button (roblox-player: and
//! roblox: links), which Mace takes over and gives back on `integrate --undo`.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Map, Value, json};

use crate::{APP_ID, fsutil, paths};

pub const SCHEMES: [&str; 2] = ["x-scheme-handler/roblox-player", "x-scheme-handler/roblox"];
// Browsers that open links through the desktop portal (OpenURI) don't ask
// xdg-mime: the portal remembers the app each link type was opened with
// ("desktop-used-apps") and, after a few uses, picks it without asking. Mace
// sets that record too, and puts the old one back on --undo.
const PORTAL_TABLE: &str = "desktop-used-apps";

fn portal_id() -> String {
    format!("{APP_ID}.open")
}

fn applications() -> PathBuf {
    paths::data_home().join("applications")
}

fn icon_target() -> PathBuf {
    paths::data_home().join("icons/hicolor/scalable/apps").join(format!("{APP_ID}.svg"))
}

/// Quote a path for a desktop entry's Exec line.
fn exec_quote(path: &Path) -> String {
    let text = path.to_string_lossy();
    if text.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c)) {
        text.into_owned()
    } else {
        format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\"").replace('$', "\\$").replace('`', "\\`"))
    }
}

fn entries(command: &Path) -> [(String, String); 2] {
    let run = exec_quote(command);
    [
        (format!("{APP_ID}.desktop"), format!(
"[Desktop Entry]
Type=Application
Name=Mace
GenericName=Roblox launcher
Comment=Play Roblox on several accounts at once
Exec={run}
Icon={APP_ID}
Terminal=false
Categories=Game;
Keywords=Roblox;macOS;Rosemary;accounts;
StartupWMClass={APP_ID}
")),
        (format!("{APP_ID}.open.desktop"), format!(
"[Desktop Entry]
Type=Application
Name=Mace (Roblox links)
Comment=Opens roblox.com's Play button in Mace
Exec={run} open %u
Icon={APP_ID}
Terminal=false
NoDisplay=true
MimeType={};
", SCHEMES.join(";"))),
    ]
}

fn state_file() -> PathBuf {
    paths::data().join("integration.json")
}

fn read_state() -> Map<String, Value> {
    match fsutil::read_json(&state_file()) {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

fn run_out(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn default_handler(scheme: &str) -> String {
    run_out("xdg-mime", &["query", "default", scheme]).unwrap_or_default()
}

fn portal_get(scheme: &str) -> Option<Vec<String>> {
    fsutil::which("flatpak")?;
    let out = run_out("flatpak", &["permissions", PORTAL_TABLE, scheme])?;
    out.lines().find_map(|line| {
        let fields: Vec<&str> = line.split('\t').collect();
        (fields.len() >= 4 && fields[0] == PORTAL_TABLE && fields[1] == scheme && fields[2].is_empty())
            .then(|| fields[3].split(',').filter(|v| !v.is_empty()).map(String::from).collect())
    })
}

fn portal_set(scheme: &str, values: &[String]) -> bool {
    if fsutil::which("flatpak").is_some() {
        let mut args = vec!["permission-set", PORTAL_TABLE, scheme, ""];
        args.extend(values.iter().map(String::as_str));
        return Command::new("flatpak").args(&args).output().is_ok_and(|o| o.status.success());
    }
    if fsutil::which("gdbus").is_some() {
        let array = format!("[{}]", values.iter().map(|v| format!("'{v}'")).collect::<Vec<_>>().join(", "));
        return Command::new("gdbus")
            .args(["call", "--session", "--dest", "org.freedesktop.impl.portal.PermissionStore",
                   "--object-path", "/org/freedesktop/impl/portal/PermissionStore",
                   "--method", "org.freedesktop.impl.portal.PermissionStore.SetPermission",
                   PORTAL_TABLE, "true", scheme, "", &array])
            .output()
            .is_ok_and(|o| o.status.success());
    }
    false
}

/// Install the entries and make Mace open Roblox links. Returns what was done.
pub fn install(command: &Path) -> anyhow::Result<Vec<String>> {
    let mut done = Vec::new();
    std::fs::create_dir_all(applications())?;
    for (name, text) in entries(command) {
        let path = applications().join(&name);
        std::fs::write(&path, text)?;
        done.push(format!("menu entry {}", path.display()));
    }
    if paths::logo().exists() {
        std::fs::create_dir_all(icon_target().parent().unwrap())?;
        std::fs::copy(paths::logo(), icon_target())?;
        done.push(format!("icon {}", icon_target().display()));
    }
    let mut state = read_state();
    let handler = format!("{APP_ID}.open.desktop");
    for scheme in SCHEMES {
        let current = default_handler(scheme);
        let previous = state.entry("previous").or_insert_with(|| json!({}));
        if !current.is_empty() && current != handler && previous.get(scheme).is_none() {
            previous[scheme] = json!(current);
        }
        let earlier = portal_get(scheme);
        let portal = state.entry("portal").or_insert_with(|| json!({}));
        if let Some(earlier) = earlier {
            if earlier.first().is_some_and(|first| *first != portal_id()) && portal.get(scheme).is_none() {
                portal[scheme] = json!(earlier);
            }
        }
    }
    fsutil::write_json(&state_file(), &Value::Object(state))?;
    if fsutil::which("xdg-mime").is_some() {
        let mut args = vec!["default", handler.as_str()];
        args.extend(SCHEMES);
        let _ = Command::new("xdg-mime").args(&args).output();
        done.push("roblox.com's Play button now opens Mace".into());
    }
    let mine = [portal_id(), "3".into(), "3".into()];
    if SCHEMES.iter().all(|scheme| portal_set(scheme, &mine)) {
        done.push("browsers that go through the desktop portal open Mace too".into());
    }
    if fsutil::which("update-desktop-database").is_some() {
        let _ = Command::new("update-desktop-database").arg(applications()).output();
    }
    if fsutil::which("gtk-update-icon-cache").is_some() {
        let _ = Command::new("gtk-update-icon-cache").args(["-q", "-t"]).arg(paths::data_home().join("icons/hicolor")).output();
    }
    Ok(done)
}

/// Give Roblox links back to whatever had them before, and remove the entries.
pub fn uninstall() -> Vec<String> {
    let mut done = Vec::new();
    let state = read_state();
    if let Some(Value::Object(previous)) = state.get("previous") {
        for (scheme, handler) in previous {
            if let (Some(handler), Some(_)) = (handler.as_str(), fsutil::which("xdg-mime")) {
                let _ = Command::new("xdg-mime").args(["default", handler, scheme]).output();
                done.push(format!("{} links open {handler} again", scheme.rsplit('/').next().unwrap_or(scheme)));
            }
        }
    }
    for scheme in SCHEMES {
        match state.get("portal").and_then(|p| p.get(scheme)).and_then(Value::as_array) {
            Some(values) => {
                let values: Vec<String> = values.iter().filter_map(|v| v.as_str().map(String::from)).collect();
                portal_set(scheme, &values);
            }
            None => {
                if portal_get(scheme).and_then(|v| v.into_iter().next()) == Some(portal_id()) {
                    portal_set(scheme, &[]);
                }
            }
        }
    }
    for (name, _) in entries(Path::new("mace")) {
        let path = applications().join(name);
        if std::fs::remove_file(&path).is_ok() {
            done.push(format!("removed {}", path.display()));
        }
    }
    let _ = std::fs::remove_file(icon_target());
    let _ = std::fs::remove_file(state_file());
    if fsutil::which("update-desktop-database").is_some() {
        let _ = Command::new("update-desktop-database").arg(applications()).output();
    }
    done
}

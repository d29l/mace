//! Menu entries, the icon, and roblox.com's Play button (roblox-player: and
//! roblox: links), which Mace takes over and gives back on `integrate --undo`.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Map, Value, json};

use crate::{APP_ID, SETTINGS_ID, fsutil, paths};

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

/// Mace, which starts Roblox (its windows are Roblox's, so they show under it);
/// Mace Settings, the window; and the handler for roblox.com's links.
fn entries(command: &Path) -> [(String, String); 3] {
    let run = exec_quote(command);
    [
        (format!("{APP_ID}.desktop"), format!(
"[Desktop Entry]
Type=Application
Name=Mace
GenericName=Roblox launcher
Comment=Play Roblox
Exec={run} play
Icon={APP_ID}
Terminal=false
Categories=Game;
Keywords=Roblox;macOS;Rosemary;play;
StartupWMClass=com.roblox.RobloxPlayer
")),
        (format!("{SETTINGS_ID}.desktop"), format!(
"[Desktop Entry]
Type=Application
Name=Mace Settings
Comment=Accounts, mods and settings for Mace
Exec={run} settings
Icon={APP_ID}
Terminal=false
Categories=Game;
Keywords=Roblox;accounts;mods;settings;
StartupWMClass={SETTINGS_ID}
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
    state.insert(ENTRIES_KEY.into(), json!(crate::VERSION));
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
    update_caches();
    Ok(done)
}

fn update_caches() {
    if fsutil::which("update-desktop-database").is_some() {
        let _ = Command::new("update-desktop-database").arg(applications()).output();
    }
    if fsutil::which("gtk-update-icon-cache").is_some() {
        let _ = Command::new("gtk-update-icon-cache").args(["-q", "-t"]).arg(paths::data_home().join("icons/hicolor")).output();
    }
}

/// The version of Mace that last wrote the menu entries, in the state file.
const ENTRIES_KEY: &str = "entries";

/// The program the installed menu entries start: the first word of Mace's
/// Exec line, as `exec_quote` wrote it. None when there are no entries.
fn installed_command() -> Option<PathBuf> {
    let text = std::fs::read_to_string(applications().join(format!("{APP_ID}.desktop"))).ok()?;
    let exec = text.lines().find_map(|l| l.strip_prefix("Exec="))?.trim();
    let program = match exec.strip_prefix('"') {
        Some(quoted) => {
            let mut out = String::new();
            let mut chars = quoted.chars();
            while let Some(ch) = chars.next() {
                match ch {
                    '\\' => out.push(chars.next()?),
                    '"' => break,
                    ch => out.push(ch),
                }
            }
            out
        }
        None => exec.split_whitespace().next()?.to_string(),
    };
    Some(PathBuf::from(program))
}

/// An update is done by the old version, which leaves the menu entries as it
/// wrote them: the first time a newer one runs, it writes its own (once per
/// version, and only where they're installed). Only the program the entries
/// start does it, not a copy being checked before an update or a test build.
pub fn refresh_after_update() {
    let mut state = read_state();
    if state.get(ENTRIES_KEY).and_then(Value::as_str) == Some(crate::VERSION) {
        return;
    }
    let Some(command) = installed_command() else { return };
    let same = |a: &Path, b: &Path| matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b);
    if !same(&command, &crate::launch::mace_exe()) {
        return;
    }
    if refresh(&command).is_ok() {
        state.insert(ENTRIES_KEY.into(), json!(crate::VERSION));
        let _ = fsutil::write_json(&state_file(), &Value::Object(state));
    }
}

/// After an update: the menu entries and icon as they are now, where they're
/// installed. Roblox links stay with whatever has them.
pub fn refresh(command: &Path) -> anyhow::Result<()> {
    if !applications().join(format!("{APP_ID}.desktop")).is_file() {
        return Ok(());
    }
    for (name, text) in entries(command) {
        fsutil::write_atomic(&applications().join(&name), text.as_bytes())?;
    }
    if paths::logo().exists() {
        std::fs::create_dir_all(icon_target().parent().unwrap())?;
        std::fs::copy(paths::logo(), icon_target())?;
    }
    update_caches();
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;

    #[test]
    fn a_newer_version_rewrites_the_menu_once() {
        let home = TempHome::new();
        let read = |id: &str| std::fs::read_to_string(applications().join(format!("{id}.desktop"))).unwrap();
        // never integrated: nothing appears
        refresh_after_update();
        assert!(!applications().exists());
        // as 0.1.1 left it: one entry, which opened the window, and no version noted
        let write_old = |program: &Path| {
            std::fs::create_dir_all(applications()).unwrap();
            std::fs::write(applications().join(format!("{APP_ID}.desktop")),
                           format!("[Desktop Entry]\nName=Mace\nExec={}\n", exec_quote(program))).unwrap();
        };
        // the menu starts another copy (a test build, or one being checked before an update): left alone
        write_old(Path::new("/opt/other/mace"));
        refresh_after_update();
        assert!(!read(APP_ID).contains(" play") && !applications().join(format!("{SETTINGS_ID}.desktop")).exists());
        // it starts this program (here through a path that needs quoting): rewritten
        let program = home.path().join("my mace/mace");
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(std::env::current_exe().unwrap(), &program).unwrap();
        write_old(&program);
        refresh_after_update();
        let play = read(APP_ID);
        assert!(play.contains(&format!("Exec={} play\n", exec_quote(&program))), "{play}");
        assert!(applications().join(format!("{SETTINGS_ID}.desktop")).is_file());
        // once per version: later edits stay
        std::fs::write(applications().join(format!("{SETTINGS_ID}.desktop")), "mine").unwrap();
        refresh_after_update();
        assert_eq!(std::fs::read_to_string(applications().join(format!("{SETTINGS_ID}.desktop"))).unwrap(), "mine");
    }

    #[test]
    fn an_update_refreshes_only_what_is_installed() {
        let _home = TempHome::new();
        // never integrated (install.sh --no-links): nothing appears
        refresh(Path::new("/opt/mace")).unwrap();
        assert!(!applications().exists() && !icon_target().exists());
        // integrated: the entries and icon are written again; links aren't touched
        std::fs::create_dir_all(applications()).unwrap();
        std::fs::write(applications().join(format!("{APP_ID}.desktop")), "old").unwrap();
        refresh(Path::new("/opt/mace")).unwrap();
        let read = |id: &str| std::fs::read_to_string(applications().join(format!("{id}.desktop"))).unwrap();
        // Mace starts Roblox; an older install's window gets its own entry, Mace Settings
        let play = read(APP_ID);
        assert!(play.contains("Name=Mace\n") && play.contains("Exec=/opt/mace play\n"), "{play}");
        let window = read(SETTINGS_ID);
        assert!(window.contains("Name=Mace Settings\n") && window.contains("Exec=/opt/mace settings\n"), "{window}");
        assert!(read(&format!("{APP_ID}.open")).contains("Exec=/opt/mace open %u\n"));
        assert!(icon_target().is_file());
        assert!(!state_file().exists());
    }
}

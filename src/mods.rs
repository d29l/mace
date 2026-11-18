//! Mods: files that replace Roblox's own, for every account. The mods folder
//! mirrors `Roblox.app/Contents/Resources` (`content/…`, `ExtraContent/…`,
//! `PlatformContent/…`), the layout of Bloxstrap's Modifications folder, so
//! its mod packs work as they are. Before each start the account's copy of
//! Roblox gets them, and files whose mod is gone are put back from the build.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, bail};
use serde_json::{Map, Value, json};

use crate::accounts::Account;
use crate::{builds, fsutil, paths};

/// Roblox's mouse pointer (the "far" one is shown over the 3D world).
pub const CURSORS: [&str; 2] =
    ["content/textures/Cursors/KeyboardMouse/ArrowCursor.png", "content/textures/Cursors/KeyboardMouse/ArrowFarCursor.png"];
/// The cursor shift lock shows, unless the game sets its own.
pub const SHIFT_LOCK: &str = "content/textures/MouseLockedCursor.png";
/// The sound a character makes when it dies.
pub const DEATH_SOUND: &str = "content/sounds/ouch.ogg";
/// A font for all of Roblox's text: every font family is pointed at it.
pub const FONT: &str = "content/fonts/CustomFont.ttf";
const FONT_ASSET: &str = "rbxasset://fonts/CustomFont.ttf";
const FAMILIES: &str = "content/fonts/families";
const RESOURCES: &str = "Contents/Resources";
/// Old cursors Bloxstrap keeps (Roblox's own art), fetched when picked rather than shipped with Mace.
const CURSOR_PRESETS: &str = "https://raw.githubusercontent.com/bloxstraplabs/bloxstrap/main/Bloxstrap/Resources/Mods/Cursor";

pub fn folder() -> PathBuf {
    paths::data().join("mods")
}

/// A path inside the mods folder (and inside Roblox's Resources): plain names only.
fn safe(rel: &str) -> bool {
    let path = Path::new(rel);
    !rel.is_empty() && path.components().all(|c| matches!(c, Component::Normal(_)))
}

/// Every mod file, relative to the folder, sorted. Hidden files are Mace's own
/// notes, and `ClientSettings` is left out (Mace writes FastFlags itself).
pub fn files() -> Vec<String> {
    let root = folder();
    let mut out = Vec::new();
    walk(&root, &root, 0, &mut out);
    out.sort();
    out
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || (depth == 0 && name.eq_ignore_ascii_case("ClientSettings")) {
            continue;
        }
        let path = entry.path();
        // followed through symlinks, which is how people often add packs
        let Ok(meta) = fs::metadata(&path) else { continue };
        if meta.is_dir() && depth < 16 {
            walk(root, &path, depth + 1, out);
        } else if meta.is_file()
            && let Some(rel) = path.strip_prefix(root).ok().and_then(Path::to_str)
            && safe(rel)
        {
            out.push(rel.to_string());
        }
    }
}

/// The mods that aren't one of the named ones (cursors, death sound, font).
pub fn other_files() -> Vec<String> {
    files().into_iter().filter(|f| !CURSORS.contains(&f.as_str()) && ![SHIFT_LOCK, DEATH_SOUND, FONT].contains(&f.as_str()))
        .collect()
}

pub fn has(rel: &str) -> bool {
    folder().join(rel).is_file()
}

/// Put `contents` in the mods folder at `rel`.
pub fn put(rel: &str, contents: &[u8]) -> anyhow::Result<()> {
    if !safe(rel) {
        bail!("not a path inside the mods folder: {rel}");
    }
    fsutil::write_atomic(&folder().join(rel), contents)
}

/// Take `rel` out of the mods folder, and the folders it leaves empty.
pub fn remove(rel: &str) -> anyhow::Result<()> {
    if !safe(rel) {
        bail!("not a path inside the mods folder: {rel}");
    }
    let root = folder();
    let path = root.join(rel);
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let mut dir = path.parent();
    while let Some(d) = dir.filter(|d| *d != root && d.starts_with(&root)) {
        if fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
    Ok(())
}

/// A short note kept with the mods (e.g. which cursor), in a hidden file.
pub fn note(name: &str) -> Option<String> {
    fs::read_to_string(folder().join(format!(".{name}"))).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub fn set_note(name: &str, value: Option<&str>) {
    let path = folder().join(format!(".{name}"));
    match value {
        Some(value) => {
            let _ = fsutil::write_atomic(&path, value.as_bytes());
        }
        None => {
            let _ = fs::remove_file(path);
        }
    }
}

/// One of Bloxstrap's old cursors ("2006", "2013"): both images, downloaded.
pub fn fetch_cursor_preset(year: &str) -> anyhow::Result<[Vec<u8>; 2]> {
    if !matches!(year, "2006" | "2013") {
        bail!("no cursor from {year}");
    }
    let agent = builds::agent(Duration::from_secs(15));
    let get = |name: &str| -> anyhow::Result<Vec<u8>> {
        let url = format!("{CURSOR_PRESETS}/From{year}/{name}");
        let bytes = agent.get(&url).call()?.body_mut().with_config().limit(1 << 20).read_to_vec()?;
        if !bytes.starts_with(b"\x89PNG") {
            bail!("{url} isn't a picture");
        }
        Ok(bytes)
    };
    Ok([get("ArrowCursor.png")?, get("ArrowFarCursor.png")?])
}

enum Data {
    File(PathBuf),
    Bytes(Vec<u8>),
}

/// What one Resources file should hold, and a signature to tell when that changed.
struct Wanted {
    sig: String,
    len: u64,
    data: Data,
}

fn signature(meta: &fs::Metadata) -> String {
    let modified = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).unwrap_or_default();
    format!("{} {}", meta.len(), modified.as_nanos())
}

/// A font family's file with every face pointed at the custom font.
fn family_with_font(path: &Path) -> Option<Vec<u8>> {
    let mut family: Value = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
    for face in family.get_mut("faces")?.as_array_mut()? {
        face.as_object_mut()?.insert("assetId".into(), json!(FONT_ASSET));
    }
    serde_json::to_vec_pretty(&family).ok()
}

/// Every Resources file the mods decide, given the build's own Resources.
fn wanted(original: Option<&Path>) -> BTreeMap<String, Wanted> {
    let root = folder();
    let mut out = BTreeMap::new();
    for rel in files() {
        let path = root.join(&rel);
        if let Ok(meta) = fs::metadata(&path) {
            out.insert(rel, Wanted { sig: signature(&meta), len: meta.len(), data: Data::File(path) });
        }
    }
    // a custom font: every family points at it, unless the mods give that family's file themselves
    let font = out.get(FONT).map(|w| w.sig.clone());
    if let (Some(font), Some(original)) = (font, original)
        && let Ok(entries) = fs::read_dir(original.join(FAMILIES))
    {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = format!("{FAMILIES}/{name}");
            if !name.ends_with(".json") || out.contains_key(&rel) {
                continue;
            }
            if let Some(bytes) = family_with_font(&entry.path()) {
                out.insert(rel, Wanted { sig: format!("font {font}"), len: bytes.len() as u64, data: Data::Bytes(bytes) });
            }
        }
    }
    out
}

/// Write `data` at `target` (a new file, renamed into place), staying inside `root`.
fn write(root: &Path, target: &Path, data: &Data) -> anyhow::Result<()> {
    let parent = target.parent().context("no folder")?;
    fs::create_dir_all(parent)?;
    let (real_root, real_parent) = (root.canonicalize()?, parent.canonicalize()?);
    if !real_parent.starts_with(&real_root) {
        bail!("{} leads out of Roblox's files", target.display());
    }
    match data {
        Data::Bytes(bytes) => fsutil::write_atomic(target, bytes),
        Data::File(source) => {
            let temp = parent.join(format!(".{}.mace", target.file_name().unwrap_or_default().to_string_lossy()));
            fs::copy(source, &temp).with_context(|| format!("copying {}", source.display()))?;
            fs::rename(&temp, target)?;
            Ok(())
        }
    }
}

/// Put Roblox's own file back at `rel`, or remove it when Roblox has none there.
fn restore(root: &Path, original: Option<&Path>, rel: &str) -> anyhow::Result<()> {
    let target = root.join(rel);
    match original.map(|o| o.join(rel)).filter(|o| o.is_file()) {
        Some(source) => write(root, &target, &Data::File(source)),
        None => match fs::remove_file(&target) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        },
    }
}

fn record_path(account: &Account) -> PathBuf {
    account.folder().join("mods.json")
}

/// Give the account's copy of Roblox the mods (it must not be running).
/// Returns how many files were written or put back.
pub fn apply(account: &Account) -> anyhow::Result<usize> {
    let root = account.bundle().join(RESOURCES);
    if !root.is_dir() {
        return Ok(0);
    }
    let build = builds::installed(account);
    let original = build.as_ref().map(|b| b.store().join(RESOURCES)).filter(|p| p.is_dir());
    let guid = build.map(|b| b.guid).unwrap_or_default();
    let stored = fsutil::read_json(&record_path(account)).unwrap_or(Value::Null);
    // a different build means a fresh copy: none of the mods are in it yet
    let mut done: Map<String, Value> = match stored.get("files") {
        Some(Value::Object(files)) if stored.get("build").and_then(Value::as_str) == Some(guid.as_str()) => files.clone(),
        _ => Map::new(),
    };
    let wanted = wanted(original.as_deref());
    let mut changed = 0;
    let gone: Vec<String> = done.keys().filter(|rel| !wanted.contains_key(*rel)).cloned().collect();
    for rel in gone {
        if safe(&rel) {
            restore(&root, original.as_deref(), &rel)?;
            changed += 1;
        }
        done.remove(&rel);
    }
    for (rel, want) in &wanted {
        let target = root.join(rel);
        let current = done.get(rel).and_then(Value::as_str) == Some(want.sig.as_str())
            && fs::metadata(&target).is_ok_and(|m| m.len() == want.len);
        if !current {
            write(&root, &target, &want.data)?;
            done.insert(rel.clone(), json!(want.sig));
            changed += 1;
        }
    }
    if done.is_empty() {
        let _ = fs::remove_file(record_path(account));
    } else {
        fsutil::write_json(&record_path(account), &json!({ "build": guid, "files": done }))?;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;

    /// A build in the store with a few Resources files, and an account holding a copy of it.
    fn setup(home: &TempHome, guid: &str) -> Account {
        let account = Account::new("main", home.path().join("prefix"), "", true);
        let store = paths::builds().join(guid).join("Roblox.app").join(RESOURCES);
        for (rel, text) in [(DEATH_SOUND, "oof"), (CURSORS[0], "arrow"), ("content/fonts/families/Arimo.json",
            r#"{"name":"Arimo","faces":[{"name":"Regular","assetId":"rbxasset://fonts/Arimo-Regular.ttf"},{"name":"Bold","assetId":"rbxassetid://1"}]}"#)] {
            fs::create_dir_all(store.join(rel).parent().unwrap()).unwrap();
            fs::write(store.join(rel), text).unwrap();
        }
        fs::create_dir_all(account.folder()).unwrap();
        let _ = fs::remove_dir_all(account.bundle());
        let status = std::process::Command::new("cp").arg("-a").arg(store.parent().unwrap().parent().unwrap())
            .arg(account.bundle()).status().unwrap();
        assert!(status.success());
        fs::write(account.folder().join("build.json"), format!(r#"{{"guid":"{guid}","version":"1","channel":"LIVE"}}"#)).unwrap();
        account
    }

    fn read(account: &Account, rel: &str) -> String {
        fs::read_to_string(account.bundle().join(RESOURCES).join(rel)).unwrap_or_else(|_| "<none>".into())
    }

    #[test]
    fn mods_go_in_and_come_back_out() {
        let home = TempHome::new();
        let account = setup(&home, "version-a");
        put(DEATH_SOUND, b"classic").unwrap();
        put("content/textures/new.png", b"added").unwrap();
        assert_eq!(apply(&account).unwrap(), 2);
        assert_eq!(read(&account, DEATH_SOUND), "classic");
        assert_eq!(read(&account, "content/textures/new.png"), "added");
        // nothing changed: nothing written
        assert_eq!(apply(&account).unwrap(), 0);
        // changed mod
        std::thread::sleep(Duration::from_millis(5));
        put(DEATH_SOUND, b"other!").unwrap();
        assert_eq!(apply(&account).unwrap(), 1);
        assert_eq!(read(&account, DEATH_SOUND), "other!");
        // removed mods: Roblox's own file back, an added file gone
        remove(DEATH_SOUND).unwrap();
        remove("content/textures/new.png").unwrap();
        assert_eq!(apply(&account).unwrap(), 2);
        assert_eq!(read(&account, DEATH_SOUND), "oof");
        assert_eq!(read(&account, "content/textures/new.png"), "<none>");
        assert!(!record_path(&account).exists());
        // and the emptied folders went with them
        assert!(!folder().join("content").exists());
    }

    #[test]
    fn a_copy_roblox_overwrote_gets_the_mod_again() {
        let home = TempHome::new();
        let account = setup(&home, "version-a");
        put(CURSORS[0], b"mine").unwrap();
        apply(&account).unwrap();
        fs::write(account.bundle().join(RESOURCES).join(CURSORS[0]), "arrow").unwrap();
        assert_eq!(apply(&account).unwrap(), 1);
        assert_eq!(read(&account, CURSORS[0]), "mine");
    }

    #[test]
    fn a_new_build_starts_over() {
        let home = TempHome::new();
        let account = setup(&home, "version-a");
        put(DEATH_SOUND, b"classic").unwrap();
        apply(&account).unwrap();
        let account = setup(&home, "version-b");
        assert_eq!(read(&account, DEATH_SOUND), "oof");
        assert_eq!(apply(&account).unwrap(), 1);
        assert_eq!(read(&account, DEATH_SOUND), "classic");
    }

    #[test]
    fn a_custom_font_takes_over_every_family() {
        let home = TempHome::new();
        let account = setup(&home, "version-a");
        put(FONT, b"ttf").unwrap();
        apply(&account).unwrap();
        let family: Value = serde_json::from_str(&read(&account, "content/fonts/families/Arimo.json")).unwrap();
        let faces = family["faces"].as_array().unwrap();
        assert_eq!(faces.len(), 2);
        assert!(faces.iter().all(|f| f["assetId"] == FONT_ASSET));
        assert_eq!(family["name"], "Arimo");
        remove(FONT).unwrap();
        apply(&account).unwrap();
        assert!(read(&account, "content/fonts/families/Arimo.json").contains("Arimo-Regular.ttf"));
        assert_eq!(read(&account, FONT), "<none>");
    }

    /// Reaches GitHub, so only on request: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn cursor_presets_download() {
        for year in ["2006", "2013"] {
            let [arrow, far] = fetch_cursor_preset(year).unwrap();
            assert!(arrow.len() > 100 && far.len() > 100);
        }
        assert!(fetch_cursor_preset("1999").is_err());
    }

    #[test]
    fn named_mods_have_their_own_rows() {
        let _home = TempHome::new();
        for rel in [CURSORS[0], CURSORS[1], SHIFT_LOCK, DEATH_SOUND, FONT, "content/textures/other.png"] {
            put(rel, b"x").unwrap();
        }
        assert_eq!(other_files(), vec!["content/textures/other.png".to_string()]);
    }

    #[test]
    fn nothing_leads_outside() {
        let _home = TempHome::new();
        assert!(put("../escape", b"x").is_err());
        assert!(put("/etc/passwd", b"x").is_err());
        assert!(remove("content/../../x").is_err());
        assert!(safe("content/sounds/ouch.ogg") && !safe("") && !safe("./a"));
        // hidden notes and ClientSettings aren't mods
        put("ClientSettings/ClientAppSettings.json", b"{}").unwrap();
        set_note("cursor", Some("2006"));
        put("content/a.png", b"a").unwrap();
        assert_eq!(files(), vec!["content/a.png".to_string()]);
        assert_eq!(note("cursor").as_deref(), Some("2006"));
    }
}

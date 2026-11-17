//! The Roblox build each account runs.
//!
//! Roblox puts accounts on channels (production, or test channels such as
//! zbisect012) and each channel is on its own version. The client checks its
//! channel at every start and, when it is running any other version, forces its
//! own updater, which under Rosemary crashes or leaves the wrong build behind.
//! So Mace gives every account the build its channel wants before it starts:
//!
//! - the channel comes from the link being opened, else the channel the client
//!   saved from the last link, else its last update check (appStorage.json);
//! - the version comes from Roblox's client-version API for that channel;
//! - each build is downloaded once into builds/<guid>/Roblox.app;
//! - each account runs its own reflinked copy (accounts/<id>/Roblox.app), so
//!   accounts on one build share its disk space, and FastFlags written into one
//!   copy stay in that copy.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail};
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::accounts::Account;
use crate::{fsutil, paths};

const VERSION_API: &str = "https://clientsettingscdn.roblox.com/v2/client-version/MacPlayer";
const VERSION_API_FALLBACK: &str = "https://clientsettings.roblox.com/v2/client-version/MacPlayer";
const HOSTS: [&str; 3] = ["setup.rbxcdn.com", "setup-aws.rbxcdn.com", "setup-ak.rbxcdn.com"];
/// channel/common serves every channel's builds, production's included.
const FOLDERS: [&str; 2] = ["channel/common/mac", "mac"];
pub const PRODUCTION: &str = "production";
pub const USER_AGENT: &str = "Mace";
const EXECUTABLE: &str = "Contents/MacOS/RobloxPlayer";
pub const FLAGS_FILE: &str = "Contents/MacOS/ClientSettings/ClientAppSettings.json";
pub const CHANNEL_PREFS: &str = "Library/Preferences/com.roblox.RobloxPlayerChannel.plist";
const X86_64: u32 = 0x0100_0007;

static GUID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^version-[0-9a-f]{16}$").unwrap());
static URL_CHANNEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\+channel:([^+]*)").unwrap());

#[derive(Clone, Debug, PartialEq)]
pub struct Build {
    pub guid: String,
    pub version: String,
    pub channel: String,
}

impl Build {
    pub fn new(guid: &str, version: &str, channel: &str) -> Build {
        Build { guid: guid.into(), version: version.into(), channel: normal_channel(channel) }
    }

    pub fn store(&self) -> PathBuf {
        paths::builds().join(&self.guid).join("Roblox.app")
    }

    fn as_json(&self) -> Value {
        json!({"guid": self.guid, "version": self.version, "channel": self.channel})
    }
}

pub fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(timeout))
        .timeout_recv_response(Some(timeout))
        .user_agent(USER_AGENT)
        .build()
        .into()
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// ---------------------------------------------------------------- which build

pub fn normal_channel(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() || name.eq_ignore_ascii_case(PRODUCTION) || name.eq_ignore_ascii_case("live") {
        PRODUCTION.into()
    } else {
        name.into()
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&text[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The channel roblox.com put in a roblox-player: link ("+channel:zbisect012+",
/// empty for production); None when the link doesn't say.
pub fn url_channel(url: Option<&str>) -> Option<String> {
    URL_CHANNEL.captures(url?).map(|m| normal_channel(&percent_decode(&m[1])))
}

/// The channel the account is on, newest word first: the link being opened,
/// then what the client saved from the last link (its channel preference), then
/// its last update check.
pub fn channel(account: &Account, url: Option<&str>) -> String {
    if let Some(from_url) = url_channel(url) {
        return from_url;
    }
    if let Ok(prefs) = plist::Value::from_file(account.home().join(CHANNEL_PREFS)) {
        if let Some(value) = prefs.as_dictionary().and_then(|d| d.get("www.roblox.com")) {
            if let Some(name) = value.as_string() {
                return normal_channel(name);
            }
        }
    }
    normal_channel(account.read_storage().get("UpdateControllerCacheChannel").and_then(Value::as_str).unwrap_or(""))
}

/// The build the account's client was told to run at its last start.
pub fn last_check(account: &Account) -> Option<Build> {
    let storage = account.read_storage();
    let payload: Value = serde_json::from_str(storage.get("UpdateControllerCacheJsonPayload")?.as_str()?).ok()?;
    let guid = payload.get("clientVersionUpload")?.as_str()?;
    if !GUID.is_match(guid) {
        return None;
    }
    let channel = storage.get("UpdateControllerCacheChannel").and_then(Value::as_str).unwrap_or("");
    Some(Build::new(guid, payload.get("version").and_then(Value::as_str).unwrap_or(""), channel))
}

fn channels_cache() -> PathBuf {
    paths::cache().join("channels.json")
}

fn remember(build: &Build) {
    let mut data = match fsutil::read_json(&channels_cache()) {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    };
    let mut entry = build.as_json();
    entry["checked"] = json!(now());
    data.insert(build.channel.clone(), entry);
    let _ = fsutil::write_json(&channels_cache(), &Value::Object(data));
}

enum Answer {
    Build(Build),
    Closed,
    Failed,
}

fn ask(url: &str, channel_name: &str, timeout: Duration) -> Answer {
    match agent(timeout).get(url).call() {
        Ok(mut response) => {
            let Ok(data) = response.body_mut().read_json::<Value>() else { return Answer::Failed };
            let guid = data.get("clientVersionUpload").and_then(Value::as_str).unwrap_or("");
            let version = data.get("version").and_then(Value::as_str).unwrap_or("");
            if GUID.is_match(guid) && !version.is_empty() {
                Answer::Build(Build::new(guid, version, channel_name))
            } else {
                Answer::Failed
            }
        }
        Err(ureq::Error::StatusCode(401 | 403 | 404)) => Answer::Closed,
        Err(_) => Answer::Failed,
    }
}

/// The build a channel is on now, from Roblox; the last answer when offline.
pub fn resolve(channel_name: &str) -> Option<Build> {
    resolve_with(channel_name, Duration::from_secs(8))
}

pub fn resolve_with(channel_name: &str, timeout: Duration) -> Option<Build> {
    let channel_name = normal_channel(channel_name);
    let suffix = if channel_name == PRODUCTION { String::new() } else { format!("/channel/{channel_name}") };
    for base in [VERSION_API, VERSION_API_FALLBACK] {
        match ask(&format!("{base}{suffix}"), &channel_name, timeout) {
            Answer::Build(build) => {
                remember(&build);
                return Some(build);
            }
            // a test channel Roblox has closed: its accounts are back on production
            Answer::Closed if channel_name != PRODUCTION => return resolve_with(PRODUCTION, timeout),
            Answer::Closed => return None,
            Answer::Failed => continue,
        }
    }
    let cached = fsutil::read_json(&channels_cache())?;
    let entry = cached.get(&channel_name)?;
    let guid = entry.get("guid")?.as_str()?;
    GUID.is_match(guid)
        .then(|| Build::new(guid, entry.get("version").and_then(Value::as_str).unwrap_or(""), &channel_name))
}

/// The build to run `account` on: its channel's current one, or, offline, what
/// its client last asked for, or what it already has.
pub fn wanted(account: &Account, url: Option<&str>) -> Option<Build> {
    resolve(&channel(account, url)).or_else(|| last_check(account)).or_else(|| installed(account))
}

// ---------------------------------------------------------------- the store

pub fn bundle_version(bundle: &Path) -> Option<String> {
    let info = plist::Value::from_file(bundle.join("Contents/Info.plist")).ok()?;
    let version = info.as_dictionary()?.get("CFBundleShortVersionString")?.as_string()?;
    (!version.is_empty()).then(|| version.to_string())
}

/// Whether a Mach-O file (thin or fat) has x86-64 code.
pub fn has_x86_64(program: &Path) -> bool {
    let Ok(mut file) = File::open(program) else { return false };
    let mut head = [0u8; 8];
    if file.read_exact(&mut head).is_err() {
        return false;
    }
    if u32::from_le_bytes(head[..4].try_into().unwrap()) == 0xFEED_FACF {
        return u32::from_le_bytes(head[4..].try_into().unwrap()) == X86_64;
    }
    let magic = u32::from_be_bytes(head[..4].try_into().unwrap());
    let count = u32::from_be_bytes(head[4..].try_into().unwrap()) as usize;
    if !(magic == 0xCAFE_BABE || magic == 0xCAFE_BABF) || count == 0 || count > 16 {
        return false;
    }
    let size = if magic == 0xCAFE_BABE { 20 } else { 32 };
    let mut table = vec![0u8; count * size];
    if file.read_exact(&mut table).is_err() {
        return false;
    }
    table.chunks(size).any(|entry| u32::from_be_bytes(entry[..4].try_into().unwrap()) == X86_64)
}

pub fn verify(bundle: &Path, version: Option<&str>) -> anyhow::Result<()> {
    let name = bundle.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let program = bundle.join(EXECUTABLE);
    if !fsutil::is_executable(&program) {
        bail!("{name} has no runnable RobloxPlayer");
    }
    if !has_x86_64(&program) {
        bail!("{name}'s RobloxPlayer has no x86-64 code");
    }
    if let Some(version) = version.filter(|v| !v.is_empty()) {
        let have = bundle_version(bundle);
        if have.as_deref() != Some(version) {
            bail!("{name} is Roblox {}, expected {version}", have.unwrap_or_default());
        }
    }
    Ok(())
}

/// An exclusive lock for as long as the returned file is open.
fn lock(name: &str) -> anyhow::Result<File> {
    fs::create_dir_all(paths::builds())?;
    let file = File::create(paths::builds().join(format!(".{name}.lock")))?;
    // SAFETY: flock on a descriptor we own.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(file)
}

/// Fetch the build's zip to `target`, resuming a partial download.
fn download(build: &Build, target: &Path, progress: &mut dyn FnMut(u64, u64)) -> anyhow::Result<()> {
    let part = target.with_extension("zip.part");
    let agent = agent(Duration::from_secs(30));
    let mut errors = Vec::new();
    for folder in FOLDERS {
        for host in HOSTS {
            let url = format!("https://{host}/{folder}/{}-RobloxPlayer.zip", build.guid);
            let mut have = part.metadata().map_or(0, |m| m.len());
            let mut request = agent.get(&url);
            if have > 0 {
                request = request.header("Range", &format!("bytes={have}-"));
            }
            let result = (|| -> anyhow::Result<()> {
                let mut response = request.call()?;
                if have > 0 && response.status().as_u16() != 206 {
                    have = 0; // the server sent it whole
                }
                let length: u64 = response
                    .headers()
                    .get("content-length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let total = have + length;
                let mut out = fs::OpenOptions::new().create(true).write(true).append(have > 0).truncate(have == 0)
                    .open(&part)?;
                let mut reader = response.body_mut().as_reader();
                let mut buffer = vec![0u8; 1 << 20];
                let mut done = have;
                loop {
                    let n = reader.read(&mut buffer)?;
                    if n == 0 {
                        break;
                    }
                    out.write_all(&buffer[..n])?;
                    done += n as u64;
                    progress(done, total);
                }
                out.flush()?;
                let size = part.metadata()?.len();
                if total > 0 && size != total {
                    bail!("got {size} of {total} bytes");
                }
                Ok(())
            })();
            match result {
                Ok(()) => {
                    fs::rename(&part, target)?;
                    return Ok(());
                }
                Err(error) => {
                    if matches!(error.downcast_ref::<ureq::Error>(), Some(ureq::Error::StatusCode(416))) {
                        let _ = fs::remove_file(&part); // the part is already whole or broken
                    }
                    errors.push(format!("{url}: {error}"));
                }
            }
        }
    }
    let tail = errors[errors.len().saturating_sub(3)..].join("\n");
    bail!("could not download Roblox {}:\n{tail}", build.version)
}

/// Whether `path` (made of plain names) stays under `root`.
fn inside(root: &Path, path: &Path) -> bool {
    let mut depth: i32 = 0;
    for part in path.strip_prefix(root).unwrap_or(path).components() {
        match part {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => return false,
        }
    }
    path.starts_with(root)
}

/// Unpack a Roblox zip keeping the bundle's symlinks and executable bits,
/// refusing anything that would land outside `root`. Reading each member to its
/// end checks its CRC.
pub fn extract(archive_path: &Path, root: &Path) -> anyhow::Result<()> {
    let mut archive = zip::ZipArchive::new(File::open(archive_path)?)?;
    for index in 0..archive.len() {
        let mut member = archive.by_index(index)?;
        let name = member.name()?.to_string();
        let relative = Path::new(&name);
        if name.starts_with('/') || name.contains('\\')
            || relative.components().any(|c| matches!(c, Component::ParentDir | Component::RootDir | Component::Prefix(_)))
        {
            bail!("the Roblox archive has a bad path: {name:?}");
        }
        let destination = root.join(relative);
        let mode = member.unix_mode().unwrap_or(0);
        let kind = mode & libc::S_IFMT;
        if member.is_dir() || kind == libc::S_IFDIR {
            fs::create_dir_all(&destination)?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        if kind == libc::S_IFLNK {
            let mut target = String::new();
            member.read_to_string(&mut target)?;
            let resolved = destination.parent().unwrap_or(root).join(&target);
            if target.starts_with('/') || !inside(root, &resolved) {
                bail!("the Roblox archive has a link out of the bundle: {name:?}");
            }
            std::os::unix::fs::symlink(&target, &destination)?;
            continue;
        }
        if kind != 0 && kind != libc::S_IFREG {
            bail!("the Roblox archive has a special file: {name:?}");
        }
        let permissions = if mode & 0o111 != 0 { 0o755 } else { 0o644 };
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(permissions)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&destination)?;
        io::copy(&mut member, &mut out)?;
    }
    Ok(())
}

/// The build in the store, downloading and unpacking it if needed.
pub fn ensure(build: &Build, progress: &mut dyn FnMut(u64, u64)) -> anyhow::Result<PathBuf> {
    if build.store().exists() {
        return Ok(build.store());
    }
    let _lock = lock(&build.guid)?;
    if build.store().exists() {
        return Ok(build.store()); // another Mace fetched it meanwhile
    }
    fs::create_dir_all(paths::downloads())?;
    let zip_path = paths::downloads().join(format!("{}-RobloxPlayer.zip", build.guid));
    if !zip_path.exists() {
        download(build, &zip_path, progress)?;
    }
    let staging = tempfile::Builder::new().prefix(&format!(".{}-", build.guid)).tempdir_in(paths::builds())?;
    if let Err(error) = extract(&zip_path, staging.path()) {
        let _ = fs::remove_file(&zip_path); // damaged: fetch it again next time
        return Err(anyhow!("the Roblox {} download is damaged: {error}", build.version));
    }
    let unpacked = staging.path().join("RobloxPlayer.app");
    verify(&unpacked, Some(&build.version))?;
    fs::rename(&unpacked, staging.path().join("Roblox.app"))?;
    let mut record = build.as_json();
    record["fetched"] = json!(now());
    fs::write(staging.path().join("build.json"), format!("{record}\n"))?;
    let staging = staging.keep();
    fs::rename(&staging, build.store().parent().unwrap())?;
    let _ = fs::remove_file(&zip_path);
    Ok(build.store())
}

// ---------------------------------------------------------------- an account's copy

/// The build the account's copy holds, as Mace recorded it.
pub fn installed(account: &Account) -> Option<Build> {
    let data = fsutil::read_json(&account.folder().join("build.json"))?;
    Some(Build::new(
        data.get("guid")?.as_str()?,
        data.get("version").and_then(Value::as_str).unwrap_or(""),
        data.get("channel").and_then(Value::as_str).unwrap_or(PRODUCTION),
    ))
}

pub fn up_to_date(account: &Account, build: &Build) -> bool {
    installed(account).is_some_and(|have| have.guid == build.guid)
        && account.bundle().exists()
        && bundle_version(&account.bundle()).as_deref() == Some(build.version.as_str())
}

/// Give `account` its own copy of `build` (fetched first if needed). The caller
/// makes sure the account isn't running.
pub fn provision(account: &Account, build: &Build, progress: &mut dyn FnMut(u64, u64)) -> anyhow::Result<PathBuf> {
    if up_to_date(account, build) {
        return Ok(account.bundle());
    }
    let source = ensure(build, progress)?;
    let folder = account.folder();
    fs::create_dir_all(&folder)?;
    let fresh = folder.join(".Roblox.app.new");
    let old = folder.join(".Roblox.app.old");
    for leftover in [&fresh, &old] {
        let _ = fs::remove_dir_all(leftover);
    }
    // a reflink on btrfs/XFS: no space used until one copy changes
    let status = Command::new("cp").args(["-a", "--reflink=auto"]).arg(&source).arg(&fresh).status()?;
    if !status.success() {
        bail!("copying Roblox {} for {} failed", build.version, account.id);
    }
    if account.bundle().symlink_metadata().is_ok() {
        fs::rename(account.bundle(), &old)?;
    }
    fs::rename(&fresh, account.bundle())?;
    let _ = fs::remove_dir_all(&old);
    fsutil::write_atomic(&folder.join("build.json"), format!("{}\n", build.as_json()).as_bytes())?;
    Ok(account.bundle())
}

/// Roblox reads FastFlags from ClientSettings/ClientAppSettings.json in its
/// bundle. Written only when it changes, and removed when there are none.
pub fn write_fast_flags(bundle: &Path, flags: &Map<String, Value>) -> anyhow::Result<()> {
    let path = bundle.join(FLAGS_FILE);
    if flags.is_empty() {
        let _ = fs::remove_file(&path);
        return Ok(());
    }
    let sorted: std::collections::BTreeMap<_, _> = flags.iter().collect();
    let text = format!("{}\n", serde_json::to_string_pretty(&sorted)?);
    if fs::read_to_string(&path).is_ok_and(|have| have == text) {
        return Ok(());
    }
    fsutil::write_atomic(&path, text.as_bytes())
}

/// Delete store builds no account uses, keeping the newest unused one (a quick
/// way back if an account moves channel again). Returns the removed guids.
pub fn gc(accounts: &[Account]) -> Vec<String> {
    let used: Vec<String> = accounts.iter().filter_map(installed).map(|b| b.guid).collect();
    let mut unused = Vec::new();
    let Ok(entries) = fs::read_dir(paths::builds()) else { return Vec::new() };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if name.starts_with(".version-") && path.is_dir() {
            let _ = fs::remove_dir_all(&path); // an unpack that was interrupted
            continue;
        }
        if !name.starts_with("version-") || used.contains(&name) || !path.is_dir() {
            continue;
        }
        let fetched = fsutil::read_json(&path.join("build.json"))
            .and_then(|d| d.get("fetched").and_then(Value::as_u64))
            .unwrap_or(0);
        unused.push((fetched, name, path));
    }
    unused.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    unused
        .into_iter()
        .skip(1)
        .map(|(_, name, path)| {
            let _ = fs::remove_dir_all(path);
            name
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::Account;
    use crate::testenv::TempHome;
    use std::io::Cursor;

    const MACHO_X86_64: [u8; 32] = {
        let mut b = [0u8; 32];
        let magic = 0xFEED_FACFu32.to_le_bytes();
        let cpu = X86_64.to_le_bytes();
        b[0] = magic[0]; b[1] = magic[1]; b[2] = magic[2]; b[3] = magic[3];
        b[4] = cpu[0]; b[5] = cpu[1]; b[6] = cpu[2]; b[7] = cpu[3];
        b
    };

    /// (name, data, mode): S_IFLNK in the mode makes a symlink.
    pub fn zip_of(entries: &[(&str, Vec<u8>, u32)]) -> Vec<u8> {
        let mut buffer = Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buffer);
            for (name, data, mode) in entries {
                if mode & libc::S_IFMT == libc::S_IFLNK {
                    writer.add_symlink(*name, String::from_utf8(data.clone()).unwrap(),
                                       zip::write::SimpleFileOptions::default()).unwrap();
                } else {
                    let options = zip::write::SimpleFileOptions::default().unix_permissions(*mode & 0o777);
                    writer.start_file(*name, options).unwrap();
                    writer.write_all(data).unwrap();
                }
            }
            writer.finish().unwrap();
        }
        buffer.into_inner()
    }

    pub fn player(version: &str) -> Vec<(&'static str, Vec<u8>, u32)> {
        let mut plist = Vec::new();
        let mut dict = plist::Dictionary::new();
        dict.insert("CFBundleShortVersionString".into(), version.into());
        plist::Value::Dictionary(dict).to_writer_xml(&mut plist).unwrap();
        vec![
            ("RobloxPlayer.app/Contents/Info.plist", plist, libc::S_IFREG | 0o644),
            ("RobloxPlayer.app/Contents/MacOS/RobloxPlayer", MACHO_X86_64.to_vec(), libc::S_IFREG | 0o755),
        ]
    }

    fn unpack(home: &TempHome, entries: &[(&str, Vec<u8>, u32)]) -> anyhow::Result<PathBuf> {
        let archive = home.path().join("a.zip");
        fs::write(&archive, zip_of(entries)).unwrap();
        let out = home.path().join("out");
        let _ = fs::remove_dir_all(&out);
        fs::create_dir_all(&out).unwrap();
        extract(&archive, &out)?;
        Ok(out)
    }

    #[test]
    fn keeps_exec_bits_and_links() {
        let home = TempHome::new();
        let mut entries = player("0.1.0.1");
        entries.push(("RobloxPlayer.app/Contents/Frameworks/A.framework/Versions/A/A", b"lib".to_vec(), libc::S_IFREG | 0o644));
        entries.push(("RobloxPlayer.app/Contents/Frameworks/A.framework/A", b"Versions/A/A".to_vec(), libc::S_IFLNK | 0o777));
        let out = unpack(&home, &entries).unwrap();
        assert!(fsutil::is_executable(&out.join("RobloxPlayer.app/Contents/MacOS/RobloxPlayer")));
        let link = out.join("RobloxPlayer.app/Contents/Frameworks/A.framework/A");
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(fs::read(&link).unwrap(), b"lib");
        verify(&out.join("RobloxPlayer.app"), Some("0.1.0.1")).unwrap();
    }

    #[test]
    fn refuses_traversal() {
        let home = TempHome::new();
        assert!(unpack(&home, &[("../evil", b"x".to_vec(), libc::S_IFREG | 0o644)]).is_err());
        assert!(unpack(&home, &[("a/link", b"../../../../etc".to_vec(), libc::S_IFLNK | 0o777)]).is_err());
        assert!(unpack(&home, &[("a/abs", b"/etc/passwd".to_vec(), libc::S_IFLNK | 0o777)]).is_err());
    }

    #[test]
    fn verify_wrong_version() {
        let home = TempHome::new();
        let out = unpack(&home, &player("0.2.0.2")).unwrap();
        assert!(verify(&out.join("RobloxPlayer.app"), Some("0.1.0.1")).is_err());
    }

    #[test]
    fn fat_binary() {
        let home = TempHome::new();
        let path = home.path().join("fat");
        let mut fat = Vec::new();
        fat.extend(0xCAFE_BABEu32.to_be_bytes());
        fat.extend(2u32.to_be_bytes());
        fat.extend(0x0100_000Cu32.to_be_bytes());
        fat.extend([0u8; 16]);
        fat.extend(X86_64.to_be_bytes());
        fat.extend([0u8; 16]);
        fs::write(&path, &fat).unwrap();
        assert!(has_x86_64(&path));
        let mut arm = Vec::new();
        arm.extend(0xCAFE_BABEu32.to_be_bytes());
        arm.extend(1u32.to_be_bytes());
        arm.extend(0x0100_000Cu32.to_be_bytes());
        arm.extend([0u8; 16]);
        fs::write(&path, &arm).unwrap();
        assert!(!has_x86_64(&path));
    }

    #[test]
    fn channel_from_storage() {
        let home = TempHome::new();
        let p = home.make_prefix("a", Some(json!({"UpdateControllerCacheChannel": "zbisect012",
            "UpdateControllerCacheJsonPayload": "{\"version\":\"0.745.100.7451059\",\"clientVersionUpload\":\"version-f72abf01b4134489\"}"})));
        let account = Account::new("alt1", p, "", false);
        assert_eq!(channel(&account, None), "zbisect012");
        assert_eq!(last_check(&account).unwrap().guid, "version-f72abf01b4134489");
        assert_eq!(channel(&Account::new("x", home.make_prefix("b", None), "", false), None), "production");
    }

    #[test]
    fn channel_sources_in_order() {
        let home = TempHome::new();
        let p = home.make_prefix("order", Some(json!({"UpdateControllerCacheChannel": ""})));
        let account = Account::new("main", p, "", false);
        assert_eq!(channel(&account, None), "production");
        let prefs = account.home().join(CHANNEL_PREFS);
        fs::create_dir_all(prefs.parent().unwrap()).unwrap();
        let mut dict = plist::Dictionary::new();
        dict.insert("www.roblox.com".into(), "zbisect012".into());
        plist::Value::Dictionary(dict).to_file_xml(&prefs).unwrap();
        assert_eq!(channel(&account, None), "zbisect012"); // newer than the cached check
        let link = "roblox-player:1+launchmode:play+gameinfo:x+channel:zbisect010+launchtime:1";
        assert_eq!(channel(&account, Some(link)), "zbisect010"); // the link wins
        assert_eq!(channel(&account, Some("roblox-player:1+channel:+launchtime:1")), "production");
        assert_eq!(channel(&account, Some("roblox://experiences/start?placeId=1")), "zbisect012");
    }

    #[test]
    fn offline_uses_the_last_answer() {
        let _home = TempHome::new();
        remember(&Build::new("version-0123456789abcdef", "1.0", "production"));
        // nothing answers on this address
        let found = resolve_with_base("http://127.0.0.1:9/x", "production").unwrap();
        assert_eq!(found.version, "1.0");
    }

    /// resolve() against another server, for the offline test.
    fn resolve_with_base(base: &str, channel_name: &str) -> Option<Build> {
        match ask(base, channel_name, Duration::from_millis(300)) {
            Answer::Build(b) => Some(b),
            _ => {
                let cached = fsutil::read_json(&channels_cache())?;
                let entry = cached.get(channel_name)?;
                Some(Build::new(entry["guid"].as_str()?, entry["version"].as_str().unwrap_or(""), channel_name))
            }
        }
    }

    #[test]
    fn gc_keeps_used_and_newest_unused() {
        let _home = TempHome::new();
        for (i, guid) in ["version-000000000000000a", "version-000000000000000b", "version-000000000000000c"].iter().enumerate() {
            let folder = paths::builds().join(guid);
            fs::create_dir_all(folder.join("Roblox.app")).unwrap();
            fs::write(folder.join("build.json"), json!({"guid": guid, "fetched": i}).to_string()).unwrap();
        }
        let account = Account::new("main", "/nonexistent", "", false);
        fs::create_dir_all(account.folder()).unwrap();
        fs::write(account.folder().join("build.json"), json!({"guid": "version-000000000000000a"}).to_string()).unwrap();
        assert_eq!(gc(&[account]), ["version-000000000000000b"]);
        assert!(paths::builds().join("version-000000000000000a").exists());
        assert!(paths::builds().join("version-000000000000000c").exists());
    }

    #[test]
    fn provision_copies_and_records() {
        let home = TempHome::new();
        let build = Build::new("version-00000000000000aa", "0.1.0.1", "production");
        fs::create_dir_all(paths::downloads()).unwrap();
        let archive = paths::downloads().join(format!("{}-RobloxPlayer.zip", build.guid));
        fs::write(&archive, zip_of(&player("0.1.0.1"))).unwrap();
        let account = Account::new("alt1", home.make_prefix("p", None), "", false);
        let bundle = provision(&account, &build, &mut |_, _| {}).unwrap();
        assert_eq!(bundle_version(&bundle).as_deref(), Some("0.1.0.1"));
        assert!(up_to_date(&account, &build));
        assert!(!archive.exists()); // the zip goes once unpacked
        let mut flags = Map::new();
        flags.insert("DFIntTaskSchedulerTargetFps".into(), json!(9999));
        write_fast_flags(&bundle, &flags).unwrap();
        let written: Value = serde_json::from_str(&fs::read_to_string(bundle.join(FLAGS_FILE)).unwrap()).unwrap();
        assert_eq!(written, json!({"DFIntTaskSchedulerTargetFps": 9999}));
        assert!(!build.store().join(FLAGS_FILE).exists()); // the shared build stays clean
        write_fast_flags(&bundle, &Map::new()).unwrap();
        assert!(!bundle.join(FLAGS_FILE).exists());
    }
}

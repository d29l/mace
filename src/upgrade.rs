//! Mace updating itself from its GitHub releases. Only the copy install.sh
//! puts in ~/.local/bin is updated: a build in a source folder (`--dev`) is
//! left to git, and a package's copy to its package manager.

use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::{VERSION, builds, fsutil, launch, panel, paths, settings};

/// The newest release, as GitHub's API describes it.
const LATEST: &str = "https://api.github.com/repos/d29l/mace/releases/latest";
/// A release's download: a `mace-VERSION` folder with the program, the
/// panel's library, Mace's files and install.sh (made by dev/package.sh).
/// Every copy out there looks for this name and layout: never change them.
pub const TARBALL: &str = "mace-x86_64-linux.tar.gz";
/// What GitHub said is reused for this long (seconds), so games starting
/// together ask it once.
const REUSE: u64 = 10 * 60;
/// While Mace Settings is open or a game runs, GitHub is asked again this often.
pub const EVERY: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    tarball: String,
    checksum: String,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// What was learned at `then` can still be used at `now` (a time in the future can't).
fn fresh(then: u64, now: u64) -> bool {
    then <= now && now - then < REUSE
}

/// "v1.2.3" or "1.2.3" → [1, 2, 3]
fn parse(version: &str) -> Option<[u64; 3]> {
    let mut parts = version.trim().trim_start_matches('v').split('.');
    let mut out = [0; 3];
    for slot in &mut out {
        *slot = parts.next()?.parse().ok()?;
    }
    parts.next().is_none().then_some(out)
}

/// Whether `candidate` is a later version than `current`.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    matches!((parse(candidate), parse(current)), (Some(a), Some(b)) if a > b)
}

/// The copy of Mace that updates itself: the program install.sh put in
/// ~/.local/bin, when it's the one running (not a link to a source folder's build).
pub fn installed_copy() -> Option<PathBuf> {
    let bin = paths::home().join(".local/bin/mace");
    let plain = fs::symlink_metadata(&bin).is_ok_and(|m| m.file_type().is_file());
    let same = fs::canonicalize(&bin).ok()? == fs::canonicalize(launch::mace_exe()).ok()?;
    (plain && same && paths::data().join("app/data").is_dir()).then_some(bin)
}

/// A release from GitHub's description of it; None without a download for Linux.
fn release_from(body: &Value) -> Option<Release> {
    let version = body.get("tag_name")?.as_str()?.trim_start_matches('v').to_string();
    parse(&version)?;
    let url = |name: &str| -> Option<String> {
        body.get("assets")?.as_array()?.iter()
            .find(|asset| asset.get("name").and_then(Value::as_str) == Some(name))?
            .get("browser_download_url")?.as_str().map(String::from)
    };
    Some(Release { tarball: url(TARBALL)?, checksum: url(&format!("{TARBALL}.sha256"))?, version })
}

/// The newest release, or None when there's none yet.
pub fn latest() -> anyhow::Result<Option<Release>> {
    // MACE_RELEASES_URL: a test server in GitHub's place
    let url = std::env::var("MACE_RELEASES_URL").unwrap_or_else(|_| LATEST.into());
    let response = builds::agent(Duration::from_secs(15)).get(&url).header("Accept", "application/vnd.github+json").call();
    let bytes = match response {
        Ok(mut response) => response.body_mut().with_config().limit(4 << 20).read_to_vec()?,
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        Err(error) => return Err(anyhow::Error::from(error).context("asking GitHub for the newest Mace")),
    };
    Ok(release_from(&serde_json::from_slice(&bytes)?))
}

fn record_file() -> PathBuf {
    paths::cache().join("update.json")
}

/// What Mace last heard: when it asked, the newest version, when a game last said so.
fn record() -> Map<String, Value> {
    match fsutil::read_json(&record_file()) {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

fn note(key: &str, value: Value) {
    let mut map = record();
    map.insert(key.into(), value);
    let _ = fsutil::write_json(&record_file(), &Value::Object(map));
}

fn remember(version: Option<&str>) {
    note("newest", json!(version));
    note("checked", json!(now()));
}

/// The newest version there is, asking GitHub unless it was asked in the last ten minutes.
pub fn newest() -> anyhow::Result<Option<String>> {
    let map = record();
    if map.get("checked").and_then(Value::as_u64).is_some_and(|t| fresh(t, now())) {
        return Ok(map.get("newest").and_then(Value::as_str).map(String::from));
    }
    let version = latest()?.map(|r| r.version);
    remember(version.as_deref());
    Ok(version)
}

/// A newer Mace than this one, when this copy updates itself and checks are on.
pub fn available() -> Option<String> {
    if !settings::load().check_updates || installed_copy().is_none() {
        return None;
    }
    newest().ok().flatten().filter(|v| is_newer(v, VERSION))
}

/// Install the newest release when it's newer than this one. Returns its version.
pub fn update_now() -> anyhow::Result<Option<String>> {
    let release = latest()?;
    remember(release.as_ref().map(|r| r.version.as_str()));
    match release {
        Some(release) if is_newer(&release.version, VERSION) => {
            install(&release)?;
            Ok(Some(release.version))
        }
        _ => Ok(None),
    }
}

/// Put `release` in place of this copy. Running games keep the files they started with.
pub fn install(release: &Release) -> anyhow::Result<()> {
    let bin = installed_copy().context("only the copy install.sh puts in ~/.local/bin updates itself")?;
    install_to(release, &bin, &paths::data().join("app"))?;
    // the new version's first run writes its menu entries (desktop::refresh_after_update)
    let _ = Command::new(&bin).arg("--version").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    Ok(())
}

fn download(url: &str, limit: u64) -> anyhow::Result<Vec<u8>> {
    let agent = builds::agent(Duration::from_secs(30));
    Ok(agent.get(url).call()?.body_mut().with_config().limit(limit).read_to_vec()?)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// One update at a time (the window's button, `mace update`).
fn lock(dir: &Path) -> anyhow::Result<fs::File> {
    let file = fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(".update.lock"))?;
    // SAFETY: flock on a descriptor this file owns; it's let go when the file closes.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        bail!("another update is running");
    }
    Ok(file)
}

/// Download, check and unpack `release`, then swap it in for the program at
/// `bin` and the files in `app`. Nothing changes unless all of it is ready.
fn install_to(release: &Release, bin: &Path, app: &Path) -> anyhow::Result<()> {
    let parent = app.parent().context("no folder for Mace's files")?;
    fs::create_dir_all(parent)?;
    let _lock = lock(parent)?;
    let tarball = download(&release.tarball, 512 << 20).context("downloading the update")?;
    let sums = download(&release.checksum, 64 << 10).context("downloading the update's checksum")?;
    let want = String::from_utf8_lossy(&sums).split_whitespace().next().unwrap_or_default().to_ascii_lowercase();
    if want != sha256_hex(&tarball) {
        bail!("the download is damaged; try again");
    }
    // unpacked beside the files it replaces, so they can be renamed into place
    let work = tempfile::Builder::new().prefix(".update-").tempdir_in(parent)?;
    let archive = work.path().join(TARBALL);
    fs::write(&archive, &tarball)?;
    let unpacked = work.path().join("unpacked");
    fs::create_dir(&unpacked)?;
    let tar = Command::new("tar").arg("-xzf").arg(&archive).arg("-C").arg(&unpacked)
        .stdin(Stdio::null()).output().context("running tar")?;
    if !tar.status.success() {
        bail!("couldn't unpack the update: {}", String::from_utf8_lossy(&tar.stderr).trim());
    }
    let top = fs::read_dir(&unpacked)?.flatten().map(|e| e.path()).find(|p| p.is_dir()).context("the update is empty")?;
    let (program, library, data) = (top.join("mace"), top.join(panel::LIBRARY), top.join("data"));
    if !(program.is_file() && library.is_file() && data.is_dir()) {
        bail!("the update is missing files");
    }
    // it has to start here (this system's GTK and glibc) before anything is replaced
    let ran = Command::new(&program).arg("--version").stdin(Stdio::null()).output().context("starting the new version")?;
    if !ran.status.success() || String::from_utf8_lossy(&ran.stdout).trim() != format!("mace {}", release.version) {
        let why = String::from_utf8_lossy(&ran.stderr);
        bail!("Mace {} doesn't start on this system: {}", release.version, why.trim().lines().last().unwrap_or("no reason given"));
    }
    // the new files beside the old ones
    let (new_app, old_app) = (parent.join("app.new"), parent.join("app.old"));
    for dir in [&new_app, &old_app] {
        let _ = fs::remove_dir_all(dir);
    }
    fs::create_dir_all(new_app.join("lib"))?;
    fs::rename(&data, new_app.join("data"))?;
    fs::rename(&library, new_app.join("lib").join(panel::LIBRARY))?;
    let new_bin = bin.with_file_name(".mace.new");
    fs::copy(&program, &new_bin).context("copying the new program")?;
    fs::set_permissions(&new_bin, fs::Permissions::from_mode(0o755))?;
    // then swapped in, the program last; if that fails the old files go back
    let had_app = app.exists();
    if had_app {
        fs::rename(app, &old_app)?;
    }
    if let Err(error) = fs::rename(&new_app, app) {
        if had_app {
            let _ = fs::rename(&old_app, app);
        }
        let _ = fs::remove_file(&new_bin);
        return Err(anyhow::Error::from(error).context("putting the update in place"));
    }
    if let Err(error) = fs::rename(&new_bin, bin) {
        let _ = fs::rename(app, &new_app);
        if had_app {
            let _ = fs::rename(&old_app, app);
        }
        let _ = fs::remove_file(&new_bin);
        let _ = fs::remove_dir_all(&new_app);
        return Err(anyhow::Error::from(error).context("putting the update in place"));
    }
    let _ = fs::remove_dir_all(&old_app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn versions_compare_by_number() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("0.2", "0.1.0") && !is_newer("0.2.0-rc1", "0.1.0") && !is_newer("", "0.1.0"));
        assert!(fresh(100, 100 + REUSE - 1) && !fresh(100, 100 + REUSE) && !fresh(200, 100));
    }

    /// A web server on a free local port, for the files `make` gives (it gets
    /// the server's address); counts the requests.
    fn serve(make: impl FnOnce(&str) -> Vec<(String, Vec<u8>)>) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let files = make(&base);
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                counter.fetch_add(1, Ordering::SeqCst);
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) <= 2 {
                        break;
                    }
                }
                let _ = match files.iter().find(|(p, _)| *p == path) {
                    Some((_, body)) => write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())
                        .and_then(|()| stream.write_all(body)),
                    None => write!(stream, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
                };
            }
        });
        (base, hits)
    }

    /// A release's tarball as dev/package.sh makes it; its program prints `says`.
    fn tarball(version: &str, says: &str) -> Vec<u8> {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().join(format!("mace-{version}"));
        fs::create_dir_all(top.join("data")).unwrap();
        fs::write(top.join("mace"), format!("#!/bin/sh\necho '{says}'\n")).unwrap();
        fs::set_permissions(top.join("mace"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(top.join(panel::LIBRARY), format!("library {version}")).unwrap();
        fs::write(top.join("data/mace.svg"), format!("logo {version}")).unwrap();
        let out = dir.path().join("out.tar.gz");
        let status = Command::new("tar").arg("-czf").arg(&out).arg("-C").arg(dir.path()).arg(format!("mace-{version}"))
            .status().unwrap();
        assert!(status.success());
        fs::read(out).unwrap()
    }

    fn release_json(base: &str, version: &str) -> Vec<u8> {
        json!({
            "tag_name": format!("v{version}"),
            "assets": [
                { "name": TARBALL, "browser_download_url": format!("{base}/{TARBALL}") },
                { "name": format!("{TARBALL}.sha256"), "browser_download_url": format!("{base}/{TARBALL}.sha256") },
            ],
        }).to_string().into_bytes()
    }

    /// An installed copy: the program and its files, as install.sh leaves them.
    fn installed(home: &TempHome) -> (PathBuf, PathBuf) {
        let bin = home.path().join("bin/mace");
        let app = paths::data().join("app");
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        fs::write(&bin, "old program").unwrap();
        fs::create_dir_all(app.join("lib")).unwrap();
        fs::create_dir_all(app.join("data")).unwrap();
        fs::write(app.join("lib").join(panel::LIBRARY), "old library").unwrap();
        fs::write(app.join("data/mace.svg"), "old logo").unwrap();
        (bin, app)
    }

    #[test]
    fn a_release_replaces_the_installed_copy() {
        let home = TempHome::new();
        let (bin, app) = installed(&home);
        let package = tarball("9.9.9", "mace 9.9.9");
        let sum = format!("{}  {TARBALL}\n", sha256_hex(&package));
        let (base, _) = serve(|base| vec![
            ("/latest".into(), release_json(base, "9.9.9")),
            (format!("/{TARBALL}"), package),
            (format!("/{TARBALL}.sha256"), sum.into_bytes()),
        ]);
        let release = release_from(&serde_json::from_slice(&release_json(&base, "9.9.9")).unwrap()).unwrap();
        assert_eq!(release.version, "9.9.9");
        install_to(&release, &bin, &app).unwrap();
        assert!(fs::read_to_string(&bin).unwrap().contains("mace 9.9.9"));
        assert!(fs::metadata(&bin).unwrap().permissions().mode() & 0o111 != 0);
        assert_eq!(fs::read_to_string(app.join("lib").join(panel::LIBRARY)).unwrap(), "library 9.9.9");
        assert_eq!(fs::read_to_string(app.join("data/mace.svg")).unwrap(), "logo 9.9.9");
        // nothing left behind beside them
        let mut left: Vec<String> = fs::read_dir(paths::data()).unwrap().flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, [".update.lock", "app"]);
        assert_eq!(fs::read_dir(bin.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn a_bad_download_changes_nothing() {
        let home = TempHome::new();
        let (bin, app) = installed(&home);
        // damaged on the way
        let package = tarball("9.9.9", "mace 9.9.9");
        let (base, _) = serve(|_| vec![
            (format!("/{TARBALL}"), package),
            (format!("/{TARBALL}.sha256"), format!("{}  {TARBALL}\n", "0".repeat(64)).into_bytes()),
        ]);
        let release = |base: &str| Release { version: "9.9.9".into(), tarball: format!("{base}/{TARBALL}"),
                                             checksum: format!("{base}/{TARBALL}.sha256") };
        let error = install_to(&release(&base), &bin, &app).unwrap_err();
        assert!(error.to_string().contains("damaged"), "{error:#}");
        // a program that doesn't start here (its libraries are missing, say)
        let package = tarball("9.9.9", "error while loading shared libraries");
        let sum = format!("{}  {TARBALL}\n", sha256_hex(&package));
        let (base, _) = serve(|_| vec![(format!("/{TARBALL}"), package), (format!("/{TARBALL}.sha256"), sum.into_bytes())]);
        let error = install_to(&release(&base), &bin, &app).unwrap_err();
        assert!(error.to_string().contains("doesn't start"), "{error:#}");
        assert_eq!(fs::read_to_string(&bin).unwrap(), "old program");
        assert_eq!(fs::read_to_string(app.join("data/mace.svg")).unwrap(), "old logo");
        assert!(!paths::data().join("app.new").exists() && !bin.with_file_name(".mace.new").exists());
    }

    #[test]
    fn games_starting_together_ask_github_once() {
        let _home = TempHome::new();
        let (base, hits) = serve(|base| vec![("/latest".into(), release_json(base, "9.9.9"))]);
        // SAFETY: TempHome holds the lock for tests that touch the environment.
        unsafe { std::env::set_var("MACE_RELEASES_URL", format!("{base}/latest")) };
        assert_eq!(newest().unwrap().as_deref(), Some("9.9.9"));
        assert_eq!(newest().unwrap().as_deref(), Some("9.9.9"));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        // ten minutes later it's asked again
        note("checked", json!(now() - REUSE));
        assert_eq!(newest().unwrap().as_deref(), Some("9.9.9"));
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        // no releases yet
        unsafe { std::env::set_var("MACE_RELEASES_URL", format!("{base}/nothing")) };
        assert_eq!(latest().unwrap(), None);
        unsafe { std::env::remove_var("MACE_RELEASES_URL") };
    }

    #[test]
    fn a_release_without_a_download_is_skipped() {
        let body = json!({ "tag_name": "v0.2.0", "assets": [] });
        assert_eq!(release_from(&body), None);
        let body = json!({ "tag_name": "nightly", "assets": [] });
        assert_eq!(release_from(&body), None);
    }
}

//! One Roblox start, from the download to the game, run in its own process
//! (`mace supervise ACCOUNT [--url URL]`, started by launch::start).
//!
//! - It puts the account's build in place, reporting progress to the loading
//!   window and to $XDG_RUNTIME_DIR/mace/launch/.
//! - It starts the client and reads its output: Rosemary's per-second Metal
//!   statistics give the frame rate the panel shows; everything else goes to
//!   the account's log in ~/.local/share/mace/logs/.
//! - When Roblox starts its own updater anyway (the account moved to another
//!   channel since Mace last looked), the updater is stopped before it shows
//!   up, the version Roblox asked for is installed by Mace, and Roblox is
//!   started again with the same link, so the game you tried to join opens.
//! - It stays until the client quits, since the client writes into its pipe.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{Map, Value, json};

use crate::accounts::{self, Account};
use crate::builds::{self, Build};
use crate::servers::unix_time;
use crate::settings::{self, Settings};
use crate::{fsutil, launch, paths, processes, servers};

const WINDOW_TIMEOUT: Duration = Duration::from_secs(120);
// what the client logs when it hands over to its updater: a new version, or
// finishing one an earlier updater started
const UPDATER: [&str; 2] = ["Found new version and the updater launched", "launchFinalizeUpdate"];
static FRAMES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bframes=([0-9.]+)/s").unwrap());
static PLACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)placeId(?:=|%3D)(\d+)").unwrap());
// The join reply in the client's log names who is playing; for a roblox.com link
// that is whoever the browser is signed in as, whatever prefix the client runs in.
static JOINED_AS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?:"UserId"%3a|userid:)(\d+)"#).unwrap());

pub fn status_dir() -> PathBuf {
    paths::runtime().join("launch")
}

pub fn stats_dir() -> PathBuf {
    paths::runtime().join("stats")
}

pub fn read_status(account_id: &str) -> Map<String, Value> {
    match fsutil::read_json(&status_dir().join(format!("{account_id}.json"))) {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

pub fn read_fps(account_id: &str) -> Option<u64> {
    let data = fsutil::read_json(&stats_dir().join(format!("{account_id}.json")))?;
    let time = data.get("time").and_then(Value::as_f64).unwrap_or(0.0);
    (unix_time() - time < 3.0).then(|| data.get("fps").and_then(Value::as_u64)).flatten()
}

/// Where the launch is, for the loading window, the window and `mace list`.
#[derive(Clone)]
pub struct Reporter {
    account_id: String,
    data: Arc<Mutex<Map<String, Value>>>,
}

impl Reporter {
    pub fn new(account: &Account, url: Option<&str>) -> Reporter {
        let mut data = Map::new();
        for (key, value) in [("account", json!(account.id)), ("name", json!(account.name())), ("avatar", Value::Null),
                             ("game", Value::Null), ("state", json!("preparing")), ("message", json!("")),
                             ("progress", Value::Null), ("pid", json!(std::process::id())),
                             ("started", json!(unix_time()))] {
            data.insert(key.into(), value);
        }
        let report = Reporter { account_id: account.id.clone(), data: Arc::new(Mutex::new(data)) };
        let (details, account, place) = (report.clone(), account.clone(),
                                         url.and_then(|u| PLACE.captures(u)).map(|m| m[1].to_string()));
        // the avatar and the game's name, looked up while the rest goes on
        std::thread::spawn(move || {
            let who = account.identity();
            if !who.is_empty() {
                if let Some(path) = servers::avatar(&who.id) {
                    details.update(&[("avatar", json!(path))]);
                }
            }
            if let Some(place) = place {
                let universe = servers::fetch_json(&format!("https://apis.roblox.com/universes/v1/places/{place}/universe"))
                    .and_then(|d| d.get("universeId").map(|u| u.to_string()));
                if let Some(name) = servers::game_name(universe.as_deref()) {
                    details.update(&[("game", json!(name))]);
                }
            }
        });
        report
    }

    /// The launch and the lookups report from different threads: one at a time.
    pub fn update(&self, fields: &[(&str, Value)]) {
        let mut data = self.data.lock().unwrap();
        for (key, value) in fields {
            data.insert((*key).to_string(), value.clone());
        }
        // a missed progress update must never stop a start
        let _ = fsutil::write_atomic(&status_dir().join(format!("{}.json", self.account_id)),
                                     Value::Object(data.clone()).to_string().as_bytes());
    }

    pub fn set(&self, state: &str, message: &str, progress: Option<f64>) {
        self.update(&[("state", json!(state)), ("message", json!(message)), ("progress", json!(progress))]);
    }

    fn download(&self, version: String) -> impl FnMut(u64, u64) {
        let report = self.clone();
        move |done, total| {
            let share = (total > 0).then(|| done as f64 / total as f64);
            report.set("downloading", &format!("Downloading Roblox {version}"), share);
        }
    }
}

/// The loading window, when there is a desktop to show it on.
fn open_loading_window(account_id: &str, config: &Settings) {
    if !config.loading_screen || (std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none()) {
        return;
    }
    let _ = Command::new(launch::mace_exe())
        .args(["loading", account_id])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

pub fn notify(title: &str, body: &str) {
    if cfg!(test) {
        return; // never on the desktop of whoever runs the tests
    }
    crate::cli::notify(title, body, &[], false);
}

/// After a start from roblox.com: remember which account the browser is signed
/// in as, so its next links open in that account's prefix.
fn learn_browser_user(account: Account, since: f64) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if let Some(log) = servers::newest_player_log(&account.logs()) {
            let modified = log.metadata().and_then(|m| m.modified()).ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0.0, |d| d.as_secs_f64());
            if modified >= since {
                let text = std::fs::read_to_string(&log).unwrap_or_default();
                if let Some(m) = JOINED_AS.captures(&text) {
                    let user = m[1].to_string();
                    let _ = accounts::set_browser_user(&user);
                    let mine = account.identity().id;
                    if !mine.is_empty() && user != mine {
                        let other = accounts::accounts().into_iter().find(|a| a.identity().id == user);
                        let browser = other.as_ref().map_or_else(|| format!("user {user}"), Account::name);
                        let next = match &other {
                            Some(other) => format!("{}'s Roblox from now on.", other.name()),
                            None => "the first account from now on.".into(),
                        };
                        notify(&format!("{} is signed in as another account now", account.name()),
                               &format!("roblox.com links sign Roblox in as the browser's account ({browser}), so \
                                         {name}'s Roblox now is that account. Sign {name} back in from its Roblox \
                                         window. Links open in {next}", name = account.name()));
                    }
                    return;
                }
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn updater_logged(account: &Account, since: f64) -> bool {
    let Some(log) = servers::newest_player_log(&account.logs()) else { return false };
    let modified = log.metadata().and_then(|m| m.modified()).ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0.0, |d| d.as_secs_f64());
    modified >= since && std::fs::read_to_string(&log).is_ok_and(|text| UPDATER.iter().any(|marker| text.contains(marker)))
}

/// Copy the client's output to its log, except Rosemary's statistics, which
/// become the frame rate; they also turn up in the middle of a line of Roblox's.
pub fn pump(stream: impl std::io::Read, log_path: &Path, account_id: &str) {
    let stats_file = stats_dir().join(format!("{account_id}.json"));
    let Ok(mut log) = std::fs::OpenOptions::new().create(true).append(true).open(log_path) else { return };
    let marker: &[u8] = b"rosemary: Metal:";
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        match line.windows(marker.len()).position(|w| w == marker) {
            Some(at) => {
                let rest = String::from_utf8_lossy(&line[at..]);
                if let Some(m) = FRAMES.captures(&rest) {
                    if let Ok(fps) = m[1].parse::<f64>() {
                        let _ = fsutil::write_atomic(&stats_file,
                                                     json!({"fps": fps.round() as u64, "time": unix_time()}).to_string().as_bytes());
                    }
                }
                if at > 0 {
                    let _ = log.write_all(&line[..at]);
                    let _ = log.write_all(b"\n");
                }
            }
            None => {
                let _ = log.write_all(&line);
            }
        }
    }
    let _ = std::fs::remove_file(&stats_file);
}

#[derive(PartialEq, Debug)]
enum Outcome {
    /// Roblox's window came up; the client ran until it was closed.
    Played,
    /// Roblox handed over to its own updater (stopped by us).
    Updated,
    /// The client quit before it showed a window, without an updater.
    Vanished,
}

/// After the client quits without a window, how long its updater may take to
/// show up: the client starts it and quits at once.
const UPDATER_GRACE: Duration = Duration::from_secs(4);

fn window_up(pids: &[i32], running_for: Duration) -> bool {
    if cfg!(test) {
        return running_for > Duration::from_millis(300); // the fake client has no window
    }
    if *processes::HYPRLAND {
        processes::window_of(pids).is_some()
    } else {
        running_for > Duration::from_secs(12) // no way to see windows: assume it's up by now
    }
}

/// While the game runs: Roblox can start its updater at any time (the server
/// may move the account to another channel mid-game), and an update it leaves
/// waiting makes the client quit to install it when it is next given a link.
/// The updater is stopped as soon as it shows up, before it downloads.
fn watch_session(account: &Account, child: &mut std::process::Child) -> anyhow::Result<()> {
    while child.try_wait()?.is_none() {
        if processes::stop_updaters(account) {
            builds::clear_updater_leftovers(account);
            note(account, "Roblox started its updater during the game; stopped it (Mace installs versions)");
        }
        std::thread::sleep(SESSION_POLL);
    }
    Ok(())
}

const SESSION_POLL: Duration = Duration::from_millis(500);

/// A line of Mace's own in the account's log.
fn note(account: &Account, text: &str) {
    if let Ok(mut log) = std::fs::OpenOptions::new().create(true).append(true)
        .open(paths::logs().join(format!("{}.log", account.id)))
    {
        let _ = writeln!(log, "mace: {text}");
    }
}

/// Run the client once.
fn run_client(account: &Account, bundle: &Path, url: Option<&str>, config: &Settings, report: &Reporter)
              -> anyhow::Result<Outcome> {
    let s = config.for_account(&account.id);
    let (argv, mut env) = launch::command(account, bundle, url, config)?;
    if s.panel_fps {
        env.insert("ROSEMARY_METAL_STATS".into(), "1".into());
    }
    std::fs::create_dir_all(paths::logs())?;
    let started = unix_time();
    let clock = Instant::now();
    let (reader, writer) = std::io::pipe()?;
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .env_clear()
        .envs(&env)
        .stdin(Stdio::null())
        .stdout(writer.try_clone()?)
        .stderr(writer)
        .spawn()?;
    let log_path = paths::logs().join(format!("{}.log", account.id));
    let id = account.id.clone();
    let pumping = std::thread::spawn(move || pump(reader, &log_path, &id));
    report.set("starting", "Starting Roblox", None);
    let (mut updated, mut shown) = (false, false);
    while child.try_wait()?.is_none() && clock.elapsed() < WINDOW_TIMEOUT && !shown {
        if processes::stop_updaters(account) {
            if !updated {
                updated = true;
                report.set("updating", "Roblox wants another version for this account", None);
            }
        } else if !updated {
            shown = window_up(&processes::prefix_pids(&account.prefix, false, None), clock.elapsed());
            if shown {
                report.set("running", "Roblox is open", None);
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    if shown {
        watch_session(account, &mut child)?;
    }
    if !shown && !updated && child.try_wait()?.is_none() {
        // no window in all that time: a client stuck somewhere, start over
        let _ = child.kill();
    }
    child.wait()?;
    if !shown {
        // the client starts its updater and quits straight away
        let grace = Instant::now();
        while grace.elapsed() < UPDATER_GRACE {
            updated |= processes::stop_updaters(account);
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let _ = pumping.join();
    Ok(if updated || (!shown && updater_logged(account, started)) {
        Outcome::Updated
    } else if shown {
        Outcome::Played
    } else {
        Outcome::Vanished
    })
}

fn last_line(error: &anyhow::Error) -> String {
    let text = format!("{error:#}");
    text.trim().lines().last().unwrap_or("Roblox didn't start").to_string()
}

/// Starts before Mace gives up on a launch.
const ATTEMPTS: usize = 4;

pub fn run(account_id: &str, url: Option<&str>) -> i32 {
    let account = match accounts::get(account_id) {
        Ok(account) => account,
        Err(error) => {
            eprintln!("mace: {error}");
            return 1;
        }
    };
    let config = settings::load();
    let report = Reporter::new(&account, url);
    open_loading_window(&account.id, &config);
    if url.is_some_and(|u| u.starts_with("roblox-player:") && u.contains("gameinfo:")) {
        let (who, since) = (account.clone(), unix_time());
        std::thread::spawn(move || learn_browser_user(who, since));
    }
    let fail = |message: &str| {
        report.set("failed", message, None);
        notify(&format!("{} didn't start", account.name()), message);
        1
    };
    let mut build: Option<Build> = None;
    let mut retried = false;
    for attempt in 1..=ATTEMPTS {
        report.set("preparing", "Checking which Roblox this account needs", None);
        if build.is_none() {
            build = builds::wanted(&account, url);
        }
        let version = build.as_ref().map(|b| b.version.clone()).unwrap_or_default();
        // also puts back a copy Roblox's updater got at, and clears what it left behind
        let bundle = match launch::prepare(&account, &mut report.download(version), &config, url, build.clone()) {
            Ok(bundle) => bundle,
            Err(error) => return fail(&last_line(&error)),
        };
        let outcome = match run_client(&account, &bundle, url, &config, &report) {
            Ok(outcome) => outcome,
            Err(error) => return fail(&last_line(&error)),
        };
        if outcome == Outcome::Played {
            report.set("done", "", None);
            let _ = std::fs::remove_file(status_dir().join(format!("{}.json", account.id)));
            return 0;
        }
        if attempt == ATTEMPTS {
            break;
        }
        builds::clear_updater_leftovers(&account);
        let asked = (outcome == Outcome::Updated).then(|| builds::last_check(&account)).flatten();
        match asked {
            // the version the client was just told to run
            Some(asked) if build.as_ref().is_none_or(|b| b.guid != asked.guid) => {
                report.set("updating", &format!("Installing Roblox {}, then opening the game again", asked.version), None);
                build = Some(asked);
            }
            // the same version: the updater was finishing an old update, or
            // the client fell over; start once more on a checked copy
            _ if !retried => {
                retried = true;
                report.set("preparing", "Roblox didn't open; starting it again", None);
            }
            _ => break,
        }
    }
    fail("Roblox keeps closing before its window opens; see `mace why`")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;

    #[test]
    fn fps_inside_a_line() {
        let home = TempHome::new();
        std::fs::create_dir_all(paths::logs()).unwrap();
        let log = paths::logs().join("y.log");
        pump(&b"[FLog::Datrosemary: Metal: stats draws=1/s frames=77/s\nhello\n"[..], &log, "main");
        assert_eq!(std::fs::read(&log).unwrap(), b"[FLog::Dat\nhello\n");
        drop(home);
    }

    #[test]
    fn read_fps_is_fresh_only() {
        let _home = TempHome::new();
        let file = stats_dir().join("main.json");
        fsutil::write_atomic(&file, json!({"fps": 200, "time": unix_time()}).to_string().as_bytes()).unwrap();
        assert_eq!(read_fps("main"), Some(200));
        fsutil::write_atomic(&file, json!({"fps": 200, "time": unix_time() - 10.0}).to_string().as_bytes()).unwrap();
        assert_eq!(read_fps("main"), None);
    }

    const FAKE_RUNNER: &str = r#"#!/bin/bash
# fake rosemary: `run BUNDLE [-protocolString URL]`
echo "$2 $4" >> "$FAKE_STATE/calls"
home="$ROSEMARY_PREFIX/root/Users/$FAKE_USER"
if [ ! -e "$FAKE_STATE/updated" ]; then
    touch "$FAKE_STATE/updated"
    printf '%s' '{"UpdateControllerCacheChannel": "", "UpdateControllerCacheJsonPayload": "{\"version\":\"0.2.0.2\",\"clientVersionUpload\":\"version-00000000000000b2\"}"}' \
        > "$home/Library/Roblox/LocalStorage/appStorage.json"
    echo "x Found new version and the updater launched. Drain reporting and quit." \
        > "$home/Library/Logs/Roblox/0.1.0.1_20260101T000000Z_Player_aaaaa_last.log"
    (exec -a RobloxPlayerInstaller sleep 30) &
    echo $! > "$FAKE_STATE/installer"
    sleep 1.5
    exit 0
fi
echo "rosemary: Metal: stats draws=900/s frames=144/s present_micros=10/s"
sleep 0.5
echo "an ordinary line"
exit 0
"#;

    /// The update path, with a fake Rosemary playing Roblox.
    #[test]
    fn update_then_relaunch_with_the_same_link() {
        use crate::builds::tests::{player, zip_of};
        let home = TempHome::new();
        let state = home.path().join("fake");
        std::fs::create_dir_all(&state).unwrap();
        let runner = home.fake_runner(FAKE_RUNNER);
        let s = Settings::from_stored(&json!({"rosemary": runner, "gamemode": false, "cpu_cores": "",
                                              "loading_screen": false, "panel_fps": true}));
        settings::save(&s).unwrap();
        let prefix = home.make_prefix("main", Some(json!({})));
        let account = accounts::register(Account::new("main", &prefix, "", false)).unwrap();
        let old = Build::new("version-00000000000000a1", "0.1.0.1", "production");
        let new = Build::new("version-00000000000000b2", "0.2.0.2", "production");
        std::fs::create_dir_all(paths::downloads()).unwrap();
        for build in [&old, &new] {
            std::fs::write(paths::downloads().join(format!("{}-RobloxPlayer.zip", build.guid)),
                           zip_of(&player(&build.version))).unwrap();
        }
        builds::remember(&old); // what "Roblox" says production is on
        // SAFETY: TempHome holds the environment lock.
        unsafe {
            std::env::set_var("MACE_OFFLINE", "1");
            std::env::set_var("FAKE_STATE", &state);
            std::env::set_var("FAKE_USER", paths::user());
        }
        let url = "roblox://experiences/start?placeId=1818";
        let code = run("main", Some(url));
        unsafe {
            std::env::remove_var("MACE_OFFLINE");
            std::env::remove_var("FAKE_STATE");
            std::env::remove_var("FAKE_USER");
        }
        assert_eq!(code, 0);
        let calls = std::fs::read_to_string(state.join("calls")).unwrap();
        let calls: Vec<&str> = calls.lines().collect();
        assert_eq!(calls.len(), 2); // started again after the update
        assert!(calls.iter().all(|c| c.ends_with(url))); // with the same link
        assert_eq!(builds::installed(&account).unwrap().guid, new.guid);
        assert_eq!(builds::bundle_version(&account.bundle()).as_deref(), Some("0.2.0.2"));
        let installer: i32 = std::fs::read_to_string(state.join("installer")).unwrap().trim().parse().unwrap();
        std::thread::sleep(Duration::from_millis(200));
        // SAFETY: probing a pid.
        assert_ne!(unsafe { libc::kill(installer, 0) }, 0); // Roblox's updater was stopped
        let log = std::fs::read_to_string(paths::logs().join("main.log")).unwrap();
        assert!(log.contains("an ordinary line"));
        assert!(!log.contains("rosemary: Metal:")); // statistics don't go to the log
        assert!(read_status("main").is_empty()); // done: no status left behind
    }

    #[test]
    fn writes_from_many_threads() {
        let home = TempHome::new();
        let account = Account::new("main", home.make_prefix("r", None), "", false);
        let report = Reporter { account_id: account.id.clone(), data: Arc::new(Mutex::new(Map::new())) };
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let report = report.clone();
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        report.update(&[("progress", json!(i as f64 / 10.0)), ("account", json!("main"))]);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(read_status("main")["account"], "main");
        let left: Vec<_> = std::fs::read_dir(status_dir()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left, ["main.json"]); // no leftovers
    }

    /// Settings, an account and a one-build store for a fake Rosemary.
    fn fake_setup(home: &TempHome, script: &str) -> (PathBuf, Account, Build) {
        use crate::builds::tests::{player, zip_of};
        let state = home.path().join("fake");
        std::fs::create_dir_all(&state).unwrap();
        let runner = home.fake_runner(script);
        let s = Settings::from_stored(&json!({"rosemary": runner, "gamemode": false, "cpu_cores": "",
                                              "loading_screen": false, "panel_fps": false}));
        settings::save(&s).unwrap();
        let build = Build::new("version-00000000000000a1", "0.1.0.1", "production");
        let storage = json!({"UpdateControllerCacheChannel": "",
                             "UpdateControllerCacheJsonPayload": json!({"version": build.version,
                                                                        "clientVersionUpload": build.guid}).to_string()});
        let prefix = home.make_prefix("main", Some(storage));
        let account = accounts::register(Account::new("main", &prefix, "", false)).unwrap();
        std::fs::create_dir_all(paths::downloads()).unwrap();
        std::fs::write(paths::downloads().join(format!("{}-RobloxPlayer.zip", build.guid)),
                       zip_of(&player(&build.version))).unwrap();
        builds::remember(&build);
        (state, account, build)
    }

    fn run_fake(state: &Path, account: &Account, url: &str) -> (i32, Vec<String>) {
        // SAFETY: the caller's TempHome holds the environment lock.
        unsafe {
            std::env::set_var("MACE_OFFLINE", "1");
            std::env::set_var("FAKE_STATE", state);
            std::env::set_var("FAKE_USER", paths::user());
            std::env::set_var("FAKE_BUNDLE", account.bundle());
        }
        let code = run(&account.id, Some(url));
        unsafe {
            for var in ["MACE_OFFLINE", "FAKE_STATE", "FAKE_USER", "FAKE_BUNDLE"] {
                std::env::remove_var(var);
            }
        }
        let calls = std::fs::read_to_string(state.join("calls")).unwrap_or_default();
        (code, calls.lines().map(str::to_string).collect())
    }

    /// What happened for real: an updater left half an update behind, the
    /// client handed over to it to finish and quit, and the updater, showing up
    /// only after that, put an old bundle over the account's copy.
    const FINISHING_RUNNER: &str = r#"#!/bin/bash
echo "$2 $4" >> "$FAKE_STATE/calls"
lib="$ROSEMARY_PREFIX/root/Users/$FAKE_USER/Library/Roblox"
[ -e "$lib/UnzippedBundle" ] && touch "$FAKE_STATE/saw-leftovers"
if [ ! -e "$FAKE_STATE/finished" ]; then
    touch "$FAKE_STATE/finished"
    (sleep 0.5; rm "$FAKE_BUNDLE/Contents/Info.plist"; exec -a RobloxPlayerInstaller sleep 30) &
    echo $! > "$FAKE_STATE/installer"
    exit 0
fi
sleep 0.6
exit 0
"#;

    #[test]
    fn an_updater_finishing_an_old_update_is_undone() {
        let home = TempHome::new();
        let (state, account, build) = fake_setup(&home, FINISHING_RUNNER);
        let lib = account.home().join("Library/Roblox");
        std::fs::create_dir_all(lib.join("UnzippedBundle/RobloxPlayer.app")).unwrap();
        std::fs::create_dir_all(lib.join("RobloxPlayerInstaller")).unwrap();
        std::fs::write(lib.join("RobloxPlayerInstaller/InstallerInfo.log"), "1 IN_PROGRESS 2 Unzip Finished\n").unwrap();
        let url = "roblox-player:1+launchmode:play+gameinfo:abc+placelauncherurl:x";
        let (code, calls) = run_fake(&state, &account, url);
        assert_eq!(code, 0);
        assert!(!state.join("saw-leftovers").exists()); // cleared before the client ran
        assert_eq!(calls.len(), 2); // opened again
        assert!(calls.iter().all(|c| c.ends_with(url))); // into the same game
        assert_eq!(builds::bundle_version(&account.bundle()).as_deref(), Some(build.version.as_str())); // repaired
        let installer: i32 = std::fs::read_to_string(state.join("installer")).unwrap().trim().parse().unwrap();
        // SAFETY: probing a pid.
        assert_ne!(unsafe { libc::kill(installer, 0) }, 0); // Roblox's updater was stopped
        assert!(read_status("main").is_empty());
    }

    #[test]
    fn a_client_that_never_opens_is_a_failure_not_silence() {
        let home = TempHome::new();
        let (state, account, _) = fake_setup(&home, "#!/bin/bash\necho \"$2 $4\" >> \"$FAKE_STATE/calls\"\nexit 0\n");
        let (code, calls) = run_fake(&state, &account, "roblox://experiences/start?placeId=1818");
        assert_eq!(code, 1);
        assert_eq!(calls.len(), 2); // tried once more first
        let status = read_status("main");
        assert_eq!(status["state"], "failed"); // the loading window says so
        assert!(status["message"].as_str().unwrap().contains("mace why"));
    }

    /// The server moved the account to another channel mid-game, and the client
    /// started its updater in the background, as on 2026-11-17.
    const MID_GAME_RUNNER: &str = r#"#!/bin/bash
echo "$2 $4" >> "$FAKE_STATE/calls"
lib="$ROSEMARY_PREFIX/root/Users/$FAKE_USER/Library/Roblox"
sleep 1
mkdir -p "$lib/UnzippedBundle" "$lib/RobloxPlayerInstaller"
echo "1 IN_PROGRESS 2 Installer started" > "$lib/RobloxPlayerInstaller/InstallerInfo.log"
(exec -a RobloxPlayerInstaller sleep 30) &
echo $! > "$FAKE_STATE/installer"
sleep 1.5
exit 0
"#;

    #[test]
    fn an_updater_during_the_game_is_stopped() {
        let home = TempHome::new();
        let (state, account, _) = fake_setup(&home, MID_GAME_RUNNER);
        let (code, calls) = run_fake(&state, &account, "roblox://experiences/start?placeId=1818");
        assert_eq!(code, 0);
        assert_eq!(calls.len(), 1); // the game wasn't started again
        let installer: i32 = std::fs::read_to_string(state.join("installer")).unwrap().trim().parse().unwrap();
        // SAFETY: probing a pid.
        assert_ne!(unsafe { libc::kill(installer, 0) }, 0);
        let lib = account.home().join("Library/Roblox");
        assert!(!lib.join("UnzippedBundle").exists());
        assert!(!lib.join("RobloxPlayerInstaller/InstallerInfo.log").exists());
        let log = std::fs::read_to_string(paths::logs().join("main.log")).unwrap();
        assert!(log.contains("updater during the game"));
    }
}


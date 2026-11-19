//! Starting an account's Roblox: its build, its settings, its prefix.

use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, LazyLock};

use anyhow::bail;
use regex::Regex;

use crate::accounts::{self, Account};
use crate::builds::{self, Build};
use crate::settings::{self, Settings};
use crate::{fsutil, links, mods, panel, paths, processes, runner, servers, supervisor};

/// Variables Mace sets from its settings. Inherited values are dropped first,
/// so an account never picks up another launcher's (or another account's) choice.
const MANAGED_ENV: [&str; 11] = [
    "ROSEMARY_PREFIX", "ROSEMARY_PREFIX_HOME", "ROSEMARY_PREFIX_SETUP", "ROSEMARY_METAL_PRESENTATION_MODE",
    "ROSEMARY_SCREEN_REFRESH_RATE", "ROSEMARY_DECORATIONS", "ROSEMARY_MENU", "ROSEMARY_CONTROL_EDIT_KEYS",
    "ROSEMARY_CONFINE_POINTER", "MANGOHUD", "MACE_PANEL",
];

pub const SETTINGS_FILE: &str = "Library/Roblox/GlobalBasicSettings_13.xml";
static FRAMERATE_CAP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(<int name="FramerateCap">)-?\d+(</int>)"#).unwrap());
static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*=").unwrap());

pub type Env = BTreeMap<String, String>;

/// Split a command line as a shell would: words, quotes and backslashes.
pub fn shell_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    word.push(q);
                }
            }
            '"' => {
                in_word = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' => {
                            if let Some(&next) = chars.peek() {
                                if matches!(next, '"' | '\\' | '$' | '`') {
                                    word.push(next);
                                    chars.next();
                                    continue;
                                }
                            }
                            word.push('\\');
                        }
                        other => word.push(other),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(next) = chars.next() {
                    word.push(next);
                }
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            other => {
                in_word = true;
                word.push(other);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    words
}

/// A wrapper command split around Roblox's: "VAR=1 gamescope -f --" goes before
/// it, and %command% marks where it goes when there is more after.
pub fn wrapper(text: &str, env: &mut Env) -> (Vec<String>, Vec<String>) {
    if text.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mut words = shell_words(text);
    while words.first().is_some_and(|w| ASSIGNMENT.is_match(w)) {
        let word = words.remove(0);
        let (name, value) = word.split_once('=').unwrap();
        env.insert(name.to_string(), value.to_string());
    }
    if let Some(first) = words.first_mut() {
        if let Some(rest) = first.strip_prefix("~/") {
            *first = paths::home().join(rest).to_string_lossy().into_owned();
        }
    }
    match words.iter().position(|w| w == "%command%") {
        Some(at) => (words[..at].to_vec(), words[at + 1..].to_vec()),
        None => (words, Vec::new()),
    }
}

/// The command line and environment that run `bundle` for `account`.
pub fn command(account: &Account, bundle: &Path, url: Option<&str>, config: &Settings) -> anyhow::Result<(Vec<String>, Env)> {
    let s = config.for_account(&account.id);
    let mut env: Env = std::env::vars().filter(|(k, _)| !MANAGED_ENV.contains(&k.as_str())).collect();
    env.insert("ROSEMARY_PREFIX".into(), account.prefix.to_string_lossy().into_owned());
    env.insert("ROSEMARY_PREFIX_SETUP".into(), "download".into()); // the Darwin base, should the cache lose it
    if !s.presentation_mode.is_empty() {
        env.insert("ROSEMARY_METAL_PRESENTATION_MODE".into(), s.presentation_mode.clone());
    }
    if s.refresh_rate > 0 {
        env.insert("ROSEMARY_SCREEN_REFRESH_RATE".into(), s.refresh_rate.to_string());
    }
    env.insert("ROSEMARY_DECORATIONS".into(), s.decorations.clone());
    env.insert("ROSEMARY_MENU".into(), s.menu.clone());
    if s.control_edit_keys {
        env.insert("ROSEMARY_CONTROL_EDIT_KEYS".into(), "1".into());
    }
    if s.confine_pointer {
        env.insert("ROSEMARY_CONFINE_POINTER".into(), "1".into());
    }
    if s.mangohud {
        env.insert("MANGOHUD".into(), "1".into());
    }
    for (name, value) in &s.extra_env {
        env.insert(name.clone(), value.as_str().map_or_else(|| value.to_string(), String::from));
    }

    let rosemary = runner::find(&s.rosemary)?;
    if config.panel
        && let Err(why) = panel::add_env(&mut env, account, &rosemary)
    {
        supervisor::note_for(&account.id, &format!("no in-game panel: {why}"));
    }
    let mut core = vec![rosemary.to_string_lossy().into_owned(), "run".into(),
                        bundle.to_string_lossy().into_owned()];
    if let Some(url) = url {
        core.extend(["-protocolString".into(), url.to_string()]);
    }
    if s.gamemode && fsutil::which("gamemoderun").is_some() {
        core.insert(0, "gamemoderun".into());
    }
    if !s.cpu_cores.is_empty() && fsutil::which("taskset").is_some() {
        core.splice(0..0, ["taskset".into(), "-c".into(), s.cpu_cores.clone()]);
    }
    let (before, after) = wrapper(&s.wrapper, &mut env);
    Ok(([before, core, after].concat(), env))
}

/// Write Roblox's own frame-rate setting (the in-game menu stops at 240). Only
/// while the account isn't running: Roblox rewrites the file on quit.
pub fn set_framerate_cap(account: &Account, cap: u64) -> bool {
    let path = account.home().join(SETTINGS_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else { return false };
    if !FRAMERATE_CAP.is_match(&text) {
        return false;
    }
    let updated = FRAMERATE_CAP.replace_all(&text, format!("${{1}}{cap}${{2}}"));
    if updated != text {
        let _ = fsutil::write_atomic(&path, updated.as_bytes());
    }
    true
}

/// Before a start: the account's copy of the build its channel wants (or
/// `build`), with its FastFlags and frame-rate cap in place.
pub fn prepare(account: &Account, progress: &mut dyn FnMut(u64, u64), config: &Settings, url: Option<&str>,
               build: Option<Build>) -> anyhow::Result<PathBuf> {
    let s = config.for_account(&account.id);
    builds::clear_updater_leftovers(account);
    let bundle = match build.or_else(|| builds::wanted(account, url)) {
        Some(build) => builds::provision(account, &build, progress)?,
        None if account.bundle().exists() => account.bundle(),
        None => bail!("can't look up the Roblox version (offline?)"),
    };
    builds::write_fast_flags(&bundle, &s.fast_flags)?;
    // a broken mod never stops a start
    if let Err(error) = mods::apply(account) {
        supervisor::note_for(&account.id, &format!("mods not applied: {error:#}"));
    }
    if s.fps_cap > 0 {
        set_framerate_cap(account, s.fps_cap);
    }
    Ok(bundle)
}

/// A link can also name another account: its current server is joined.
pub fn resolve_link(link: Option<&str>, account: &Account) -> anyhow::Result<Option<String>> {
    let Some(link) = link.filter(|l| !l.trim().is_empty()) else { return Ok(None) };
    let link = match accounts::find(link) {
        Ok(other) => {
            if other.id == account.id {
                bail!("an account can't join itself");
            }
            let (server, _) = servers::current(&other);
            match server.get("link").and_then(|v| v.as_str()) {
                Some(link) => link.to_string(),
                None => bail!("{} isn't in a server", other.name()),
            }
        }
        Err(_) => link.to_string(),
    };
    Ok(Some(links::join_url(&link)?))
}

pub(crate) fn spawn_detached(argv: &[String], env: Option<&Env>, log: Option<&Path>) -> anyhow::Result<()> {
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]).stdin(Stdio::null());
    if let Some(env) = env {
        command.env_clear().envs(env);
    }
    match log {
        Some(path) => {
            let file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
            command.stdout(file.try_clone()?).stderr(file);
        }
        None => {
            command.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    // SAFETY: setsid in the child before exec; async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    // reap it when it ends, should this process (the window, the helper) still be around
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

pub fn mace_exe() -> PathBuf {
    std::env::current_exe().map(still_here).unwrap_or_else(|_| PathBuf::from("mace"))
}

/// A program replaced on disk while it runs (an update) reads as "…/mace
/// (deleted)"; the new one is at the same path.
fn still_here(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|p| p.strip_suffix(" (deleted)")) {
        Some(p) => PathBuf::from(p),
        None => path,
    }
}

/// Give a running client a link, so it moves to that server without closing.
///
/// A second `rosemary run` of a running bundle is handed to one of the
/// bundle's processes, and Roblox's crash handler lives in the bundle too: when
/// it gets the link, it drops it. It only sends crash reports (which Rosemary
/// can't anyway), so it is stopped first and the game is the one left.
///
/// That `rosemary run` finds the running client through the prefix's process
/// server. A client started while that server was shutting down isn't known to
/// it, and the `rosemary run` then starts a second client, which would sign in
/// and get the first one kicked: it is stopped at once, and the answer is
/// `Blind` (as it is straight away for a client known to have started so).
fn hand_over(account: &Account, argv: &[String], env: &Env) -> anyhow::Result<Handed> {
    // a client that started without Rosemary's process server can't be found: don't even try
    if crate::supervisor::blind_marker(&account.id).exists() {
        return Ok(Handed::Blind);
    }
    // an update waiting to be installed makes the client quit on a new link
    processes::stop_updaters(account);
    builds::clear_updater_leftovers(account);
    processes::stop_crash_handlers(account);
    let player_log = servers::newest_player_log(&account.logs());
    let seen = player_log.as_ref().and_then(|p| p.metadata().ok()).map_or(0, |m| m.len());
    std::fs::create_dir_all(paths::logs())?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(paths::logs().join(format!("{}.log", account.id)))?;
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // its own group: if it turns out to be a second client, all of it goes at once
        .process_group(0)
        .spawn()?;
    let group = child.id() as i32;
    let kill_group = || {
        // SAFETY: the group of the process we just started.
        unsafe { libc::killpg(group, libc::SIGKILL) };
    };
    // what Rosemary says goes to the account's log, and is listened to
    let second = Arc::new(AtomicBool::new(false));
    let handed_to = Arc::new(AtomicI32::new(0));
    let streams: [Option<Box<dyn std::io::Read + Send>>; 2] =
        [child.stdout.take().map(|s| Box::new(s) as _), child.stderr.take().map(|s| Box::new(s) as _)];
    for stream in streams.into_iter().flatten() {
        let (second, handed_to, mut log) = (second.clone(), handed_to.clone(), log.try_clone()?);
        std::thread::spawn(move || {
            use std::io::{BufRead, Write};
            for line in std::io::BufReader::new(stream).lines().map_while(Result::ok) {
                let _ = writeln!(log, "{line}");
                if SECOND_CLIENT.iter().any(|s| line.contains(s)) {
                    second.store(true, Ordering::Relaxed);
                }
                if let Some(pid) = HANDED.captures(&line).and_then(|m| m[1].parse::<i32>().ok()) {
                    handed_to.store(pid, Ordering::Relaxed);
                }
            }
        });
    }
    let clock = std::time::Instant::now();
    let took = |path: &Path| {
        use std::io::{Read, Seek, SeekFrom};
        let mut text = String::new();
        std::fs::File::open(path).ok().and_then(|mut f| {
            f.seek(SeekFrom::Start(seen)).ok()?;
            f.take(1 << 20).read_to_string(&mut text).ok()
        });
        text.contains(OPENED_URL)
    };
    while clock.elapsed() < HAND_OVER_TIMEOUT {
        // Rosemary is starting another client (it couldn't find the running one): stop it before it
        // signs in, which would get the running one kicked
        if second.load(Ordering::Relaxed) || servers::newest_player_log(&account.logs()) != player_log {
            kill_group();
            let _ = child.wait();
            return Ok(Handed::Blind);
        }
        if player_log.as_deref().is_some_and(took) {
            let _ = child.wait();
            return Ok(Handed::Taken);
        }
        // Rosemary gave it to the game itself (Roblox only logs links it starts with, not ones it gets while running)
        let pid = handed_to.swap(0, Ordering::Relaxed);
        if pid > 0 && processes::running(std::slice::from_ref(account), None).get(&account.id).is_some_and(|p| p.contains(&pid)) {
            let _ = child.wait();
            return Ok(Handed::Taken);
        }
        if let Some(status) = child.try_wait()?
            && !status.success()
        {
            bail!("Rosemary couldn't hand {} the link", account.name());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    kill_group();
    let _ = child.wait();
    bail!("{}'s Roblox didn't respond. Try again, or restart it", account.name())
}

/// How a hand-over went.
#[derive(Debug, PartialEq)]
enum Handed {
    /// The running client took the link.
    Taken,
    /// Rosemary couldn't find the running client (and a second one, if it began, was stopped).
    Blind,
}

/// What the client logs when it is given a link.
const OPENED_URL: &str = "(AppDelegate) application:openURLs";
/// What a `rosemary run` says when it gave the link to the running client.
static HANDED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"is already running \(pid (\d+)\) and was handed this launch").unwrap());
/// What a `rosemary run` says when it's about to start a client of its own
/// instead of handing the link over.
const SECOND_CLIENT: [&str; 2] = ["process server is unavailable", "imports unbound"];
const HAND_OVER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(if cfg!(test) { 1 } else { 8 });

/// Start an account, joining `link` if given. A new start runs under a
/// supervisor (`mace supervise`), which returns at once. A running account is
/// handed the link instead, as a Mac hands a URL to a running app (started
/// again with it if it can't be handed links), or, with no link, has its
/// window raised. Returns the command line used.
pub fn start(account: &Account, link: Option<&str>, dry_run: bool, remember: bool) -> anyhow::Result<Vec<String>> {
    let config = settings::load();
    let url = resolve_link(link, account)?;
    let live = processes::running(&accounts::accounts(), None).remove(&account.id).unwrap_or_default();
    if !live.is_empty() && url.is_none() {
        processes::focus(&live);
        return Ok(Vec::new());
    }
    if dry_run {
        return Ok(command(account, &account.bundle(), url.as_deref(), &config)?.0);
    }
    std::fs::create_dir_all(paths::logs())?;
    if !live.is_empty() {
        let (argv, env) = command(account, &account.bundle(), url.as_deref(), &config)?;
        if hand_over(account, &argv, &env)? == Handed::Taken {
            if remember {
                accounts::set_last_used(account)?;
            }
            return Ok(argv);
        }
        // it can't be handed links: start it again, into that server, rather than have two sign in
        supervisor::note_for(&account.id, "couldn't hand the link to the running Roblox; restarting it into the server");
        processes::stop(account, &accounts::accounts(), std::time::Duration::from_secs(2));
    }
    let mut argv = vec![mace_exe().to_string_lossy().into_owned(), "supervise".into(), account.id.clone()];
    if let Some(url) = &url {
        argv.extend(["--url".into(), url.clone()]);
    }
    spawn_detached(&argv, None, None)?;
    if remember {
        accounts::set_last_used(account)?;
    }
    Ok(argv)
}

/// Bring every other account into the server `target` is in.
pub fn gather(target: &Account) -> anyhow::Result<Vec<String>> {
    let (server, _) = servers::current(target);
    let Some(link) = server.get("link").and_then(|v| v.as_str()) else {
        bail!("{} isn't in a server", target.name());
    };
    let others: Vec<Account> = accounts::accounts().into_iter().filter(|a| a.id != target.id).collect();
    if others.is_empty() {
        bail!("there are no other accounts");
    }
    for account in &others {
        start(account, Some(link), false, false)?;
    }
    Ok(others.iter().map(Account::name).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;
    use serde_json::json;

    #[test]
    fn a_program_replaced_while_running_is_found_at_its_path() {
        assert_eq!(still_here("/usr/local/bin/mace (deleted)".into()), PathBuf::from("/usr/local/bin/mace"));
        assert_eq!(still_here("/usr/local/bin/mace".into()), PathBuf::from("/usr/local/bin/mace"));
    }

    fn setup(home: &TempHome) -> (Account, Account) {
        let runner = home.fake_runner("#!/bin/sh\nexit 0\n");
        let s = Settings::from_stored(&json!({
            "rosemary": runner, "cpu_cores": "1-5,7-11",
            "extra_env": {"MANGOHUD": "1", "ROSEMARY_COMMAND_KEY": "super+ctrl:edit"},
            "accounts": {"alt1": {"refresh_rate": 60, "presentation_mode": "fifo"}},
        }));
        settings::save(&s).unwrap();
        (Account::new("main", home.make_prefix("main", None), "", false),
         Account::new("alt1", home.make_prefix("alt1", None), "", false))
    }

    #[test]
    fn command_and_env() {
        let home = TempHome::new();
        let (main, _) = setup(&home);
        let config = settings::load();
        let (argv, env) = command(&main, &main.bundle(), Some("roblox://experiences/start?placeId=1"), &config).unwrap();
        let rosemary = argv.iter().position(|a| a == "run").unwrap() - 1;
        assert_eq!(&argv[rosemary + 1..], ["run", &main.bundle().to_string_lossy(), "-protocolString",
                                           "roblox://experiences/start?placeId=1"]);
        if fsutil::which("taskset").is_some() {
            assert_eq!(&argv[..3], ["taskset", "-c", "1-5,7-11"]);
        }
        assert_eq!(env["ROSEMARY_PREFIX"], main.prefix.to_string_lossy());
        assert_eq!(env["ROSEMARY_MENU"], "none");
        assert_eq!(env["ROSEMARY_SCREEN_REFRESH_RATE"], "9999");
        assert_eq!(env["ROSEMARY_COMMAND_KEY"], "super+ctrl:edit");
    }

    #[test]
    fn account_overrides() {
        let home = TempHome::new();
        let (_, alt) = setup(&home);
        let (_, env) = command(&alt, &alt.bundle(), None, &settings::load()).unwrap();
        assert_eq!(env["ROSEMARY_SCREEN_REFRESH_RATE"], "60");
        assert_eq!(env["ROSEMARY_METAL_PRESENTATION_MODE"], "fifo");
    }

    #[test]
    fn wrapper_split() {
        let mut env = Env::new();
        assert_eq!(wrapper("DXVK_HUD=1 gamescope -f -- %command% --after", &mut env),
                   (vec!["gamescope".into(), "-f".into(), "--".into()], vec!["--after".into()]));
        assert_eq!(env["DXVK_HUD"], "1");
        assert_eq!(shell_words(r#"a 'b c' "d \"e\"" f\ g"#), ["a", "b c", "d \"e\"", "f g"]);
    }

    #[test]
    fn framerate_cap() {
        let home = TempHome::new();
        let (main, _) = setup(&home);
        let path = main.home().join(SETTINGS_FILE);
        std::fs::write(&path, "<Properties>\n\t<int name=\"FramerateCap\">240</int>\n</Properties>\n").unwrap();
        assert!(set_framerate_cap(&main, 9999));
        assert!(std::fs::read_to_string(&path).unwrap().contains("<int name=\"FramerateCap\">9999</int>"));
        std::fs::write(&path, "<Properties/>").unwrap();
        assert!(!set_framerate_cap(&main, 9999));
    }

    /// A running account, faked: a "client" running the bundle and a "crash
    /// handler" running from inside it, both in the account's prefix.
    fn live_account(home: &TempHome, runner: &str) -> (Account, std::process::Child, std::process::Child, PathBuf) {
        let runner = home.fake_runner(runner);
        settings::save(&Settings::from_stored(&json!({"rosemary": runner, "gamemode": false, "cpu_cores": ""}))).unwrap();
        let account = accounts::register(Account::new("main", home.make_prefix("main", Some(json!({}))), "", false)).unwrap();
        let player_log = account.logs().join("0.1.0.1_20260101T000000Z_Player_aaaaa_last.log");
        std::fs::write(&player_log, "x ! Joining game 'a' place 1 at 10.0.0.1\n").unwrap();
        let idle = home.path().join("idle");
        std::fs::write(&idle, "#!/bin/bash\nsleep 30\n").unwrap();
        let spawn = |program: PathBuf| {
            Command::new("bash").arg(&idle).arg("run").arg(program)
                .env("ROSEMARY_PREFIX", &account.prefix).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap()
        };
        let client = spawn(account.bundle());
        let handler = spawn(account.bundle().join("Contents/MacOS/RobloxCrashHandler"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while processes::running(&accounts::accounts(), None).get("main").is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        (account, client, handler, player_log)
    }

    #[test]
    fn a_running_client_is_handed_the_link_and_stays() {
        let home = TempHome::new();
        let (account, mut client, mut handler, player_log) = live_account(&home,
            "#!/bin/bash\necho \"x (AppDelegate) application:openURLs:$4\" >> \"$FAKE_PLAYER_LOG\"\n");
        let lib = account.home().join("Library/Roblox");
        std::fs::create_dir_all(lib.join("UnzippedBundle/RobloxPlayer.app")).unwrap();
        // SAFETY: TempHome holds the environment lock.
        unsafe { std::env::set_var("FAKE_PLAYER_LOG", &player_log) };
        let result = start(&account, Some("roblox://experiences/start?placeId=1818"), false, false);
        unsafe { std::env::remove_var("FAKE_PLAYER_LOG") };
        result.unwrap();
        assert!(handler.try_wait().unwrap().is_some()); // the crash handler was stopped
        assert!(client.try_wait().unwrap().is_none()); // the game kept running
        assert!(std::fs::read_to_string(&player_log).unwrap().contains("openURLs:roblox://"));
        assert!(read_status_missing(&account)); // no new start, no loading window
        assert!(!lib.join("UnzippedBundle").exists()); // a waiting update would make it quit
        let _ = client.kill();
        let _ = client.wait();
    }

    #[test]
    fn a_link_rosemary_gives_the_running_game_is_taken() {
        let home = TempHome::new();
        // as Rosemary says it; the game logs nothing about links it gets while running
        let (account, mut client, _handler, _) = live_account(&home,
            "#!/bin/bash\necho \"rosemary: $3 is already running (pid $FAKE_CLIENT_PID) and was handed this launch\" >&2\n");
        // SAFETY: TempHome holds the environment lock.
        unsafe { std::env::set_var("FAKE_CLIENT_PID", client.id().to_string()) };
        let result = start(&account, Some("roblox://experiences/start?placeId=1818"), false, false);
        unsafe { std::env::remove_var("FAKE_CLIENT_PID") };
        result.unwrap(); // not "didn't respond" after the wait
        assert!(client.try_wait().unwrap().is_none());
        let _ = client.kill();
        let _ = client.wait();
    }

    #[test]
    fn a_link_handed_to_another_process_is_not_taken() {
        let home = TempHome::new();
        let (account, mut client, _handler, _) = live_account(&home,
            "#!/bin/bash\necho \"rosemary: $3 is already running (pid 1) and was handed this launch\"\n");
        assert!(start(&account, Some("roblox://experiences/start?placeId=1818"), false, false).is_err());
        let _ = client.kill();
        let _ = client.wait();
    }

    #[test]
    fn a_link_the_client_never_takes_is_an_error() {
        let home = TempHome::new();
        let (account, mut client, _handler, _) = live_account(&home, "#!/bin/bash\nexit 0\n");
        let error = start(&account, Some("roblox://experiences/start?placeId=1818"), false, false).unwrap_err();
        assert!(error.to_string().contains("didn't respond"), "{error}");
        assert!(client.try_wait().unwrap().is_none());
        let _ = client.kill();
        let _ = client.wait();
    }

    fn read_status_missing(account: &Account) -> bool {
        crate::supervisor::read_status(&account.id).is_empty()
    }

    fn hand_over_args(account: &Account) -> (Vec<String>, Env) {
        command(account, &account.bundle(), Some("roblox://experiences/start?placeId=1818"), &settings::load()).unwrap()
    }

    #[test]
    fn a_hand_over_that_would_start_a_second_client_is_stopped() {
        let home = TempHome::new();
        // Rosemary can't find the running client: it says so, and goes on to start one of its own
        let (account, mut client, _handler, _) = live_account(&home,
            "#!/bin/bash\necho 'rosemary: the process server is unavailable (the prefix is shutting down); \
             this process cannot see any other' >&2\nsleep 30\n");
        let (argv, env) = hand_over_args(&account);
        let clock = std::time::Instant::now();
        assert_eq!(hand_over(&account, &argv, &env).unwrap(), Handed::Blind);
        // killed, not waited for (that would take 30 s); the 2 s here are the fake crash handler, a
        // child of this test, lingering as a zombie while it is stopped
        assert!(clock.elapsed() < std::time::Duration::from_secs(5), "{:?}", clock.elapsed());
        assert!(client.try_wait().unwrap().is_none(), "the running game itself is left alone");
        let _ = client.kill();
        let _ = client.wait();
    }

    #[test]
    fn a_client_that_started_blind_isnt_handed_links() {
        let home = TempHome::new();
        let (account, mut client, _handler, player_log) = live_account(&home,
            "#!/bin/bash\necho \"x (AppDelegate) application:openURLs:$4\" >> \"$FAKE_PLAYER_LOG\"\n");
        std::fs::create_dir_all(paths::runtime()).unwrap();
        std::fs::write(crate::supervisor::blind_marker("main"), b"").unwrap();
        let (argv, env) = hand_over_args(&account);
        assert_eq!(hand_over(&account, &argv, &env).unwrap(), Handed::Blind);
        assert!(!std::fs::read_to_string(&player_log).unwrap().contains("openURLs"), "nothing was run");
        let _ = client.kill();
        let _ = client.wait();
    }

    #[test]
    fn leftovers_go_before_a_client_starts_but_not_while_one_runs() {
        let home = TempHome::new();
        let (account, mut client, mut handler, _) = live_account(&home, "#!/bin/bash\nexit 0\n");
        let everyone = accounts::accounts();
        assert_eq!(processes::clear_leftovers(&account, &everyone), 0, "a client runs: its helpers are its own");
        assert!(handler.try_wait().unwrap().is_none());
        let _ = client.kill();
        let _ = client.wait();
        // the client is gone; its crash handler lingers on, as Roblox's helpers do
        assert!(processes::clear_leftovers(&account, &everyone) >= 1);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while handler.try_wait().unwrap().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(handler.try_wait().unwrap().is_some());
    }
}

//! Starting an account's Roblox: its build, its settings, its prefix.

use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;

use anyhow::bail;
use regex::Regex;

use crate::accounts::{self, Account};
use crate::builds::{self, Build};
use crate::settings::{self, Settings};
use crate::{fsutil, links, paths, processes, runner, servers};

/// Variables Mace sets from its settings. Inherited values are dropped first,
/// so an account never picks up another launcher's (or another account's) choice.
const MANAGED_ENV: [&str; 10] = [
    "ROSEMARY_PREFIX", "ROSEMARY_PREFIX_HOME", "ROSEMARY_PREFIX_SETUP", "ROSEMARY_METAL_PRESENTATION_MODE",
    "ROSEMARY_SCREEN_REFRESH_RATE", "ROSEMARY_DECORATIONS", "ROSEMARY_MENU", "ROSEMARY_CONTROL_EDIT_KEYS",
    "ROSEMARY_CONFINE_POINTER", "MANGOHUD",
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

    let mut core = vec![runner::find(&s.rosemary)?.to_string_lossy().into_owned(), "run".into(),
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
    let bundle = match build.or_else(|| builds::wanted(account, url)) {
        Some(build) => builds::provision(account, &build, progress)?,
        None if account.bundle().exists() => account.bundle(),
        None => bail!("can't find out which Roblox to download (is the internet down?)"),
    };
    builds::write_fast_flags(&bundle, &s.fast_flags)?;
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

fn spawn_detached(argv: &[String], env: Option<&Env>, log: Option<&Path>) -> anyhow::Result<()> {
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
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("mace"))
}

/// Start an account, joining `link` if given. A new start runs under a
/// supervisor (`mace supervise`), which returns at once. A running account is
/// handed the link instead, as a Mac hands a URL to a running app, or, with no
/// link, has its window raised. Returns the command line used.
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
    let argv = if !live.is_empty() {
        let (argv, env) = command(account, &account.bundle(), url.as_deref(), &config)?;
        spawn_detached(&argv, Some(&env), Some(&paths::logs().join(format!("{}.log", account.id))))?;
        argv
    } else {
        let mut argv = vec![mace_exe().to_string_lossy().into_owned(), "supervise".into(), account.id.clone()];
        if let Some(url) = &url {
            argv.extend(["--url".into(), url.clone()]);
        }
        spawn_detached(&argv, None, None)?;
        argv
    };
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
}

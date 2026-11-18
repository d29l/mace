//! Running Roblox clients, told apart by the Rosemary prefix in their
//! environment, so each one belongs to exactly one account.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::accounts::Account;
use crate::fsutil;

pub static HYPRLAND: LazyLock<bool> = LazyLock::new(|| {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some_and(|v| !v.is_empty()) && fsutil::which("hyprctl").is_some()
});
const WRAPPERS: [&str; 5] = ["sh", "taskset", "gamemoderun", "mangohud", "env"];

pub struct Process {
    pub prefix: String,
    pub args: Vec<Vec<u8>>,
}

/// This user's processes that run in a Rosemary prefix: pid -> prefix and argv.
/// One pass over /proc, for callers that ask several questions at once.
pub type Snapshot = BTreeMap<i32, Process>;

fn args_of(pid: i32) -> Option<Vec<Vec<u8>>> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    Some(raw.split(|&b| b == 0).map(<[u8]>::to_vec).collect())
}

fn prefix_of(pid: i32) -> Option<String> {
    let raw = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    let value = raw.split(|&b| b == 0).find_map(|item| item.strip_prefix(b"ROSEMARY_PREFIX="))?;
    let path = std::path::PathBuf::from(String::from_utf8_lossy(value).into_owned());
    Some(std::fs::canonicalize(&path).unwrap_or(path).to_string_lossy().into_owned())
}

pub fn scan() -> Snapshot {
    // SAFETY: getuid can't fail.
    let uid = unsafe { libc::getuid() };
    let mut found = Snapshot::new();
    let Ok(entries) = std::fs::read_dir("/proc") else { return found };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else { continue };
        if entry.metadata().map(|m| m.uid()).ok() != Some(uid) {
            continue;
        }
        if let (Some(prefix), Some(args)) = (prefix_of(pid), args_of(pid)) {
            found.insert(pid, Process { prefix, args });
        }
    }
    found
}

fn real(path: &Path) -> String {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()).to_string_lossy().into_owned()
}

/// account id -> pids of its `rosemary run …/Roblox.app` client processes.
pub fn running(accounts: &[Account], snapshot: Option<&Snapshot>) -> HashMap<String, Vec<i32>> {
    let owned;
    let procs = match snapshot {
        Some(s) => s,
        None => {
            owned = scan();
            &owned
        }
    };
    let by_prefix: HashMap<String, &str> = accounts.iter().map(|a| (real(&a.prefix), a.id.as_str())).collect();
    let mut found: HashMap<String, Vec<i32>> = HashMap::new();
    for (pid, process) in procs {
        let args = &process.args;
        if !args.iter().any(|a| a == b"run") || !args.iter().any(|a| trim_slash(a).ends_with(b"/Roblox.app")) {
            continue;
        }
        let program = Path::new(std::str::from_utf8(&args[0]).unwrap_or(""));
        let base = program.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if WRAPPERS.contains(&base.as_str()) {
            continue;
        }
        if let Some(id) = by_prefix.get(&process.prefix) {
            found.entry((*id).to_string()).or_default().push(*pid);
        }
    }
    for pids in found.values_mut() {
        pids.sort();
    }
    found
}

fn trim_slash(arg: &[u8]) -> &[u8] {
    let mut end = arg.len();
    while end > 0 && arg[end - 1] == b'/' {
        end -= 1;
    }
    &arg[..end]
}

/// Whether one of `pids` runs `bundle` (so it was started by Mace).
pub fn from_bundle(pids: &[i32], bundle: &Path) -> bool {
    let needle = bundle.to_string_lossy();
    pids.iter().any(|pid| args_of(*pid).is_some_and(|args| args.iter().any(|a| trim_slash(a) == needle.as_bytes())))
}

/// Every process of this user's in `prefix`; its process server only if asked.
pub fn prefix_pids(prefix: &Path, servers: bool, snapshot: Option<&Snapshot>) -> Vec<i32> {
    let owned;
    let procs = match snapshot {
        Some(s) => s,
        None => {
            owned = scan();
            &owned
        }
    };
    let want = real(prefix);
    let me = std::process::id() as i32;
    procs
        .iter()
        .filter(|(pid, p)| {
            **pid != me && p.prefix == want && (servers || !p.args.iter().skip(1).take(2).any(|a| a == b"server"))
        })
        .map(|(pid, _)| *pid)
        .collect()
}

fn hyprctl(args: &[&str]) -> String {
    Command::new("hyprctl").args(args).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

/// Hyprland's windows (empty elsewhere).
pub fn windows() -> Vec<Value> {
    if !*HYPRLAND {
        return Vec::new();
    }
    serde_json::from_str::<Value>(&hyprctl(&["clients", "-j"])).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default()
}

fn pid_of(window: &Value) -> i64 {
    window.get("pid").and_then(Value::as_i64).unwrap_or(-1)
}

pub fn window_of(pids: &[i32]) -> Option<Value> {
    windows().into_iter().find(|w| pids.contains(&(pid_of(w) as i32)))
}

pub fn active_window() -> Option<Value> {
    if !*HYPRLAND {
        return None;
    }
    let window: Value = serde_json::from_str(&hyprctl(&["activewindow", "-j"])).ok()?;
    (pid_of(&window) > 0).then_some(window)
}

pub fn monitors() -> Vec<Value> {
    if !*HYPRLAND {
        return Vec::new();
    }
    serde_json::from_str::<Value>(&hyprctl(&["monitors", "-j"])).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default()
}

/// Run one Hyprland dispatcher. This Hyprland takes Lua, e.g.
/// hl.dsp.window.close({ window = "pid:123" }).
pub fn dispatch(lua: &str) {
    if *HYPRLAND {
        let _ = Command::new("hyprctl").args(["dispatch", lua]).output();
    }
}

pub fn focus(pids: &[i32]) -> bool {
    match window_of(pids) {
        Some(window) => {
            dispatch(&format!("hl.dsp.focus({{ window = \"pid:{}\" }})", pid_of(&window)));
            true
        }
        None => false,
    }
}

/// Close the windows as their close button does, so Roblox quits as usual and
/// saves its settings. Hyprland only.
pub fn close_windows(pids: &[i32]) -> bool {
    let mut closed = false;
    for window in windows() {
        if pids.contains(&(pid_of(&window) as i32)) {
            dispatch(&format!("hl.dsp.window.close({{ window = \"pid:{}\" }})", pid_of(&window)));
            closed = true;
        }
    }
    closed
}

/// Kill the processes in the account's prefix whose command line names
/// `program`. Returns their pids.
pub fn kill_in_prefix(account: &Account, program: &str) -> Vec<i32> {
    let found: Vec<i32> = prefix_pids(&account.prefix, false, None)
        .into_iter()
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/cmdline"))
                .is_ok_and(|args| args.windows(program.len()).any(|w| w == program.as_bytes()))
        })
        .collect();
    for pid in &found {
        // SAFETY: plain kill of a pid of ours.
        unsafe { libc::kill(*pid, libc::SIGKILL) };
    }
    found
}

/// Stop Roblox's own updater in the account's prefix. Mace picks the version,
/// and the updater crashes under Rosemary, closes the game to install, and
/// leaves half an update behind. Returns whether one was running.
pub fn stop_updaters(account: &Account) -> bool {
    !kill_in_prefix(account, "RobloxPlayerInstaller").is_empty()
}

/// Stop Roblox's crash handler in the account's prefix, and wait until
/// Rosemary has forgotten it (its socket in the process server is gone), so a
/// link handed to the bundle reaches the game. Returns the pids stopped.
pub fn stop_crash_handlers(account: &Account) -> Vec<i32> {
    let handlers = kill_in_prefix(account, "RobloxCrashHandler");
    let registered = |pid: i32| {
        std::fs::read_dir(crate::paths::runtime_root().join("rosemary")).into_iter().flatten().flatten()
            .any(|server| server.path().join("applications").join(pid.to_string()).exists())
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline
        && handlers.iter().any(|pid| Path::new(&format!("/proc/{pid}")).exists() || registered(*pid))
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    handlers
}

/// Quit an account's Roblox: close its window, then kill what is left of it
/// after `grace` (Roblox ignores SIGTERM).
pub fn stop(account: &Account, accounts: &[Account], grace: Duration) -> bool {
    let alive = || running(accounts, None).get(&account.id).cloned().unwrap_or_default();
    let pids = alive();
    if pids.is_empty() {
        return true;
    }
    if close_windows(&pids) {
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline && !alive().is_empty() {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    for pid in prefix_pids(&account.prefix, false, None) {
        // SAFETY: plain kill of a pid of ours.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && !alive().is_empty() {
        std::thread::sleep(Duration::from_millis(100));
    }
    alive().is_empty()
}

/// The prefix's process server, if one runs: its pid and the directory it serves.
fn process_server(prefix: &Path, snapshot: &Snapshot) -> Option<(i32, PathBuf)> {
    let want = real(prefix);
    snapshot.iter().find_map(|(pid, p)| {
        if p.prefix != want || !p.args.iter().skip(1).take(2).any(|a| a == b"server") {
            return None;
        }
        let at = p.args.iter().position(|a| a == b"--directory")?;
        Some((*pid, PathBuf::from(String::from_utf8_lossy(p.args.get(at + 1)?).into_owned())))
    })
}

/// Before a client starts in an account's prefix, nothing of an earlier one
/// may linger. Roblox's helpers (the menu bar, the crash handler) can outlive
/// its client, and Rosemary's process server stays "shutting down" while
/// they're around: a client started then can't be seen by later `rosemary
/// run`s, so a link handed to it starts a second client instead (which signs
/// in and gets the first one kicked). Stops the helpers, and waits for a
/// server on its way out to finish. Returns how many helpers were stopped.
pub fn clear_leftovers(account: &Account, accounts: &[Account]) -> usize {
    let snapshot = scan();
    if running(accounts, Some(&snapshot)).contains_key(&account.id) {
        return 0; // a client runs: nothing is left over
    }
    let helpers = prefix_pids(&account.prefix, false, Some(&snapshot));
    for pid in &helpers {
        // SAFETY: plain kill of a pid of ours.
        unsafe { libc::kill(*pid, libc::SIGKILL) };
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let now = scan();
        let lingering = !prefix_pids(&account.prefix, false, Some(&now)).is_empty();
        let server = process_server(&account.prefix, &now);
        let healthy = server.as_ref()
            .is_some_and(|(_, dir)| std::os::unix::net::UnixStream::connect(dir.join("socket")).is_ok());
        if !lingering && (server.is_none() || healthy) {
            break;
        }
        if Instant::now() >= deadline {
            if let Some((pid, _)) = server.filter(|_| !healthy) {
                // SAFETY: plain kill of a pid of ours (a server that wouldn't finish).
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    helpers.len()
}

// ---------------------------------------------------------------- sound

pub fn sink_inputs() -> Vec<Value> {
    Command::new("pactl")
        .args(["-f", "json", "list", "sink-inputs"])
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

/// The account's sound streams: those whose process is in its prefix.
pub fn account_streams(account: &Account, snapshot: Option<&Snapshot>, inputs: Option<&[Value]>) -> Vec<Value> {
    let pids: HashSet<String> =
        prefix_pids(&account.prefix, false, snapshot).into_iter().map(|p| p.to_string()).collect();
    let owned;
    let inputs = match inputs {
        Some(i) => i,
        None => {
            owned = sink_inputs();
            &owned
        }
    };
    inputs
        .iter()
        .filter(|s| {
            let pid = s.pointer("/properties/application.process.id");
            let pid = match pid {
                Some(Value::String(p)) => p.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            };
            pids.contains(&pid)
        })
        .cloned()
        .collect()
}

pub fn muted(account: &Account, snapshot: Option<&Snapshot>, inputs: Option<&[Value]>) -> Option<bool> {
    let streams = account_streams(account, snapshot, inputs);
    (!streams.is_empty()).then(|| streams.iter().all(|s| s.get("mute").and_then(Value::as_bool).unwrap_or(false)))
}

/// Mute or unmute an account's sound (None toggles). Only its playback streams
/// change; the mic and every other app are left alone.
pub fn set_muted(account: &Account, mute: Option<bool>) -> Option<bool> {
    let streams = account_streams(account, None, None);
    if streams.is_empty() {
        return None;
    }
    let mute = mute.unwrap_or_else(|| !streams.iter().all(|s| s.get("mute").and_then(Value::as_bool).unwrap_or(false)));
    for stream in &streams {
        if let Some(index) = stream.get("index").and_then(Value::as_u64) {
            let _ = Command::new("pactl")
                .args(["set-sink-input-mute", &index.to_string(), if mute { "1" } else { "0" }])
                .output();
        }
    }
    Some(mute)
}

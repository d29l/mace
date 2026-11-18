//! `mace why`: what happened at an account's last start, in plain words, read
//! from Roblox's own logs, its updater's log and Mace's log of the launch.

use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::SystemTime;

use regex::Regex;

use crate::accounts::Account;
use crate::servers::read_tail;
use crate::{builds, paths, servers};

static WANTED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"version response: \{"version":"([0-9.]+)""#).unwrap());
static JOINED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"! Joining game '[0-9a-f-]+' place (\d+)").unwrap());
static MOVED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"Successfully moved .* to (.*Roblox\.app)").unwrap());
static SELECTOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"unrecognized selector sent to instance of (\w+): ([\w:]+)").unwrap());
static FAULT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"rosemary: fault: (\w+)").unwrap());
const FORCED: &str = "Update mode is chosen as FORCE";
const UPDATER_STARTED: &str = "Found new version and the updater launched";
const QUIT: &str = "applicationWillTerminate";
const FINISHING: &str = "launchFinalizeUpdate";
const SHUTDOWN: &str = "[FLog::SingleSurfaceApp] shutDown";

fn modified(path: &PathBuf) -> SystemTime {
    path.metadata().and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH)
}

fn installer_logs(account: &Account) -> Vec<PathBuf> {
    let mut logs: Vec<PathBuf> = std::fs::read_dir(account.logs())
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.file_name().is_some_and(|n| {
                    let n = n.to_string_lossy();
                    n.starts_with("RobloxPlayerInstaller_") && n.ends_with(".log")
                }))
                .collect()
        })
        .unwrap_or_default();
    logs.sort_by_key(modified);
    logs
}

/// Findings about the account's newest start, most important first.
pub fn diagnose(account: &Account, live: bool) -> Vec<String> {
    let mut found = Vec::new();
    let launch_log = paths::logs().join(format!("{}.log", account.id));
    let Some(log) = servers::newest_player_log(&account.logs()) else {
        return vec![format!("Roblox hasn't written a log for this account yet: it has never started, or it failed \
                             before Roblox itself ran. See Mace's log: {}", launch_log.display())];
    };
    let text = read_tail(&log, 4 << 20);
    let have = builds::bundle_version(&account.bundle());
    let version = log.file_name().map(|n| n.to_string_lossy().split('_').next().unwrap_or("").to_string()).unwrap_or_default();

    if text.contains(FORCED) && text.contains(UPDATER_STARTED) {
        let wanted = WANTED.captures(&text).map_or("another version".to_string(), |m| m[1].to_string());
        found.push(format!("Roblox {version} was told to run {wanted} for this account's channel, so it started its own \
                            updater (the installer window) and quit."));
        let since = modified(&log) - std::time::Duration::from_secs(5);
        if let Some(installer) = installer_logs(account).into_iter().filter(|p| modified(p) >= since).last() {
            let itext = read_tail(&installer, 4 << 20);
            let selector = SELECTOR.captures(&itext);
            let fault = FAULT.captures(&itext);
            let moved = MOVED.is_match(&itext);
            if moved && (selector.is_some() || fault.is_some()) {
                let what = match (&selector, &fault) {
                    (Some(s), _) => format!("{} {}", &s[1], &s[2]),
                    (None, Some(f)) => f[1].to_string(),
                    _ => unreachable!(),
                };
                found.push(format!("The updater installed the new version, then crashed before reopening Roblox \
                                    ({what}, a Rosemary bug). Starting the account again works."));
            } else if moved {
                found.push("The updater installed the new version.".into());
            } else if itext.contains(QUIT) {
                found.push("The updater was closed before it installed anything, so the old version stayed.".into());
            } else if selector.is_some() || fault.is_some() {
                found.push("The updater crashed before it installed anything.".into());
            }
        }
        found.push("Mace downloads the right version itself before each start, so this only happens when Roblox \
                    moves an account to another channel between two starts."
            .into());
    } else if text.contains(FINISHING) {
        found.push(format!("Roblox {version} found an update that an earlier updater left unfinished, handed over to \
                            the updater to finish it, and quit."));
        let since = modified(&log) - std::time::Duration::from_secs(5);
        let moved = installer_logs(account).into_iter().filter(|p| modified(p) >= since)
            .any(|p| MOVED.is_match(&read_tail(&p, 4 << 20)));
        if moved {
            found.push("The updater put the old version it had unpacked back then over this account's copy.".into());
        }
        found.push("Mace clears those leftovers and checks the copy before each start, and starts Roblox again when \
                    this happens anyway; `mace update` does the same without starting it."
            .into());
    } else if let Some(m) = JOINED.captures(&text) {
        found.push(format!("Roblox {version} started and joined place {}.{}", &m[1],
                           if live { " It is still running." } else { "" }));
    } else if live {
        found.push(format!("Roblox {version} is running and hasn't joined a game yet."));
    } else if text.contains(SHUTDOWN) {
        found.push(format!("Roblox {version} started and quit without joining a game."));
    } else {
        found.push(format!("Roblox {version} stopped without saying why in its log (a crash, or it was killed)."));
    }

    let tail = read_tail(&launch_log, 8000);
    if let (Some(fault), false) = (FAULT.captures(&tail), live) {
        let detail = SELECTOR.captures(&tail).map_or(String::new(), |s| format!(" after {} {}", &s[1], &s[2]));
        found.push(format!("Rosemary reported a {} fault{detail} (in {}).", &fault[1], launch_log.display()));
    }
    if let Some(have) = have {
        if !version.is_empty() && !have.starts_with(&version) && !live {
            found.push(format!("This account's copy of Roblox is now {have}."));
        }
    }
    found.extend(panel(&tail));
    found
}

/// Why the in-game panel isn't there, if Mace's log of the launch says.
fn panel(launch_log: &str) -> Option<String> {
    let last = |prefix: &str| launch_log.lines().rev().find_map(|l| l.strip_prefix(prefix).map(str::to_string));
    if let Some(why) = last("mace: no in-game panel: ") {
        return Some(format!("There was no in-game panel: {why}."));
    }
    last("mace-panel: the panel is off for this session: ")
        .map(|why| format!("The in-game panel turned itself off ({why}); the game carried on without it."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;

    fn stamp(seconds: f64) -> String {
        format!("2026-11-15T20:00:00.000Z,{seconds:.6},d5617800,6")
    }

    #[test]
    fn installer_crash() {
        let home = TempHome::new();
        let account = Account::new("main", home.make_prefix("d", None), "", false);
        let lines = [
            format!("{},Debug [FLog::UpdateController] version response: {{\"version\":\"0.747.0.7471481\",\"clientVersionUpload\":\"version-8001f8762ae44efe\",\"bootstrapperVersion\":\"\"}}", stamp(0.4)),
            format!("{},Info [FLog::UpdateController] Update mode is chosen as FORCE. Telemetry sent", stamp(0.47)),
            format!("{} [FLog::Output] Found new version and the updater launched. Drain reporting and quit.", stamp(0.48)),
            format!("{} [FLog::SingleSurfaceApp] shutDown: (stage:None).", stamp(2.0)),
        ];
        std::fs::write(account.logs().join("0.745.100.7451059_20261115T203023Z_Player_00bc0_last.log"),
                       lines.join("\n") + "\n").unwrap();
        let installer = account.logs().join("RobloxPlayerInstaller_2026-11-15_21:30:24.log");
        std::fs::write(&installer, "INFO +[MacUtil moveItemAtPath:toFolder:withName:error:] Successfully moved \
            /Users/w/Library/Roblox/UnzippedBundle/RobloxPlayer.app to /Volumes/Host/x/Roblox.app.\n\
            rosemary: objc: unrecognized selector sent to instance of NSMutableSet: initWithCapacity:\n\
            rosemary: fault: SIGABRT at 0x0\n").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options().write(true).open(&installer).unwrap().set_modified(later).unwrap();
        let found = diagnose(&account, false);
        assert!(found[0].contains("0.747.0.7471481"));
        assert!(found[1].contains("installed the new version, then crashed"));
        assert!(found[1].contains("NSMutableSet initWithCapacity:"));
    }

    #[test]
    fn finishing_an_old_update() {
        let home = TempHome::new();
        let account = Account::new("main", home.make_prefix("f", None), "", false);
        std::fs::write(account.logs().join("0.747.0.7471481_20261117T150004Z_Player_2e3d8_last.log"),
                       format!("{},Info [FLog::UpdateController] MacUpdateController::launchFinalizeUpdate: launching \
                                finalize stage\n", stamp(0.88))).unwrap();
        std::fs::write(account.logs().join("RobloxPlayerInstaller_2026-11-17_16:00:04.log"),
                       "INFO +[MacUtil moveItemAtPath:toFolder:withName:error:] Successfully moved \
                        /Users/w/Library/Roblox/UnzippedBundle/RobloxPlayer.app to /Volumes/Host/x/Roblox.app.\n").unwrap();
        let found = diagnose(&account, false);
        assert!(found[0].contains("left unfinished"), "{found:?}");
        assert!(found[1].contains("old version"), "{found:?}");
    }

    #[test]
    fn clean_join_and_no_log() {
        let home = TempHome::new();
        let account = Account::new("main", home.make_prefix("j", None), "", false);
        assert!(diagnose(&account, false)[0].contains("hasn't written a log"));
        std::fs::write(account.logs().join("0.747.0.7471481_20261115T200000Z_Player_abcde_last.log"),
                       format!("{} [FLog::Output] ! Joining game '11111111-2222-4333-8444-555555555555' place 1818 at 10.0.0.1\n", stamp(1.0))).unwrap();
        assert!(diagnose(&account, false)[0].contains("joined place 1818"));
    }

    #[test]
    fn the_panel_says_why_it_isnt_there() {
        assert_eq!(panel("mace: started\nmace: no in-game panel: the panel's library isn't installed\n").as_deref(),
                   Some("There was no in-game panel: the panel's library isn't installed."));
        assert!(panel("mace-panel: the panel is off for this session: drawing failed: ERROR_DEVICE_LOST\n").unwrap()
            .contains("turned itself off (drawing failed"));
        assert_eq!(panel("mace: the in-game panel is ready (Roblox pid 4, wayland)\n"), None);
    }
}

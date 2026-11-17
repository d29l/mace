//! The command line. Commands write to the `out` they're given, so the overlay
//! helper can run them without their output reaching its protocol stream.

use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail};
use serde_json::{Map, Value, json};

use crate::accounts::{self, Account};
use crate::servers::{self, LogFollower};
use crate::{builds, desktop, doctor, launch, migrate, paths, processes, settings, supervisor};

pub const HELP: &str = "mace: the macOS Roblox client on Linux, on several accounts at once.

    mace                          the window
    mace list [--json]            accounts, whether they run, their servers and builds
    mace start ACCOUNT [LINK]     start an account; LINK can also be another account
    mace stop ACCOUNT
    mace open URL                 open a roblox.com link (on the account the browser is signed in as)
    mace gather ACCOUNT           every other account joins ACCOUNT's server
    mace rejoin ACCOUNT           join the server ACCOUNT is in again
    mace link [ACCOUNT]           copy a join link (ACCOUNT's, the focused window's, the latest)
    mace job [ACCOUNT]            copy the job ID instead
    mace focus ACCOUNT            raise ACCOUNT's window
    mace mute ACCOUNT [on|off]    mute or unmute ACCOUNT's sound (toggles without on/off)
    mace update [ACCOUNT|--all]   download the Roblox build each account's channel wants
    mace gc                       delete builds no account uses
    mace why [ACCOUNT]            what happened at ACCOUNT's last start
    mace migrate                  set up from Nutmeg (once)
    mace integrate [--undo]       menu entries and roblox.com's Play button
    mace watch [--dry-run]        notifications on every join (the toasts service)
    mace overlay-state            JSON for the in-game panel
    mace active                   exit 0 when an account started by Mace is running
    mace --version

ACCOUNT is an id (main, alt1, ...), a name given in Mace, a username or a display name.
LINK is a join link, a game/share/roblox:// link, or \"PLACEID JOBID\".";

/// A notification; with actions, it waits and returns the one clicked.
pub fn notify(title: &str, body: &str, actions: &[(&str, &str)], dry_run: bool) -> Option<String> {
    if dry_run {
        println!("[toast] {title} | {body} | {actions:?}");
        return None;
    }
    if crate::fsutil::which("notify-send").is_none() {
        println!("{title}: {body}");
        return None;
    }
    let logo = paths::logo();
    let icon = if logo.exists() { logo.to_string_lossy().into_owned() } else { crate::APP_ID.into() };
    let mut command = Command::new("notify-send");
    command.args(["-a", "Mace", "-i", &icon]);
    for (name, text) in actions {
        command.arg(format!("--action={name}={text}"));
    }
    let output = command.args([title, body]).output().ok()?;
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn copy(text: &str, dry_run: bool) {
    if dry_run {
        println!("[copy] {text}");
        return;
    }
    let mut tools: Vec<&[&str]> = vec![&["wl-copy"], &["xclip", "-selection", "clipboard"], &["xsel", "--clipboard", "--input"]];
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        tools.rotate_left(1);
    }
    for tool in tools {
        if crate::fsutil::which(tool[0]).is_some() {
            if let Ok(mut child) = Command::new(tool[0]).args(&tool[1..]).stdin(Stdio::piped()).spawn() {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(text.as_bytes());
                }
                let _ = child.wait();
                return;
            }
        }
    }
    println!("{text}");
}

fn live() -> HashMap<String, Vec<i32>> {
    processes::running(&accounts::accounts(), None)
}

pub fn focused_account() -> Option<Account> {
    let window = processes::active_window()?;
    let pid = window.get("pid")?.as_i64()? as i32;
    let id = live().into_iter().find(|(_, pids)| pids.contains(&pid))?.0;
    accounts::get(&id).ok()
}

fn account_arg(args: &[String], focused: bool) -> anyhow::Result<Account> {
    if let Some(query) = args.first() {
        return accounts::find(query);
    }
    if focused {
        if let Some(account) = focused_account() {
            return Ok(account);
        }
    }
    bail!("which account? (see `mace list`)")
}

fn current_servers(live: &HashMap<String, Vec<i32>>) -> HashMap<String, Map<String, Value>> {
    accounts::accounts()
        .into_iter()
        .filter(|a| live.contains_key(&a.id))
        .filter_map(|a| {
            let (server, _) = servers::current(&a);
            (!server.is_empty()).then(|| (a.id, server))
        })
        .collect()
}

fn str_of<'a>(map: &'a Map<String, Value>, key: &str) -> &'a str {
    map.get(key).and_then(Value::as_str).unwrap_or("")
}

// ---------------------------------------------------------------- commands

fn cmd_list(args: &[String], out: &mut dyn Write) -> anyhow::Result<()> {
    let live = live();
    let found = current_servers(&live);
    let mut rows = Vec::new();
    for account in accounts::accounts() {
        let who = account.identity();
        let have = builds::installed(&account);
        let server = found.get(&account.id).map(servers::describe);
        let launching = supervisor::read_status(&account.id);
        rows.push(json!({
            "id": account.id, "name": account.name_with(&who), "username": who.username,
            "running": live.contains_key(&account.id), "channel": builds::channel(&account, None),
            "build": have.map(|b| b.version), "server": server,
            "launch": (!launching.is_empty()).then_some(Value::Object(launching)),
            "fps": supervisor::read_fps(&account.id),
        }));
    }
    if args.iter().any(|a| a == "--json") {
        writeln!(out, "{}", serde_json::to_string_pretty(&rows)?)?;
        return Ok(());
    }
    let username = |r: &Value| r["username"].as_str().unwrap_or("").to_string();
    for r in &rows {
        let name = username(r);
        let twice = !name.is_empty() && rows.iter().filter(|o| username(o) == name).count() > 1;
        let user = if name.is_empty() { "(not signed in)".into() }
                   else if twice { format!("@{name} (twice!)") } else { format!("@{name}") };
        let mut state = if r["running"].as_bool() == Some(true) { "running" } else { "stopped" }.to_string();
        if let Some(launch_state) = r["launch"]["state"].as_str() {
            if launch_state != "running" && launch_state != "done" {
                state = launch_state.into();
            }
        }
        let where_ = match &r["server"] {
            Value::Object(s) => format!("  {}  {}", s.get("name").and_then(Value::as_str).unwrap_or(str_of(s, "place")),
                                        str_of(s, "link")),
            _ => String::new(),
        };
        writeln!(out, "{:<6} {:<16} {:<20} {:<8} {:<18} {}{}", r["id"].as_str().unwrap_or(""),
                 r["name"].as_str().unwrap_or(""), user, state, r["build"].as_str().unwrap_or("-"),
                 r["channel"].as_str().unwrap_or(""), where_)?;
    }
    let mut seen: Vec<String> = Vec::new();
    for r in &rows {
        let name = username(r);
        let ids: Vec<&str> = rows.iter().filter(|o| username(o) == name).filter_map(|o| o["id"].as_str()).collect();
        if !name.is_empty() && ids.len() > 1 && !seen.contains(&name) {
            eprintln!("\nnote: {} are all signed in as @{name}; sign the wrong one(s) back in from their Roblox window",
                      ids.join(", "));
            seen.push(name);
        }
    }
    Ok(())
}

fn cmd_start(args: &[String], out: &mut dyn Write) -> anyhow::Result<()> {
    let dry = args.iter().any(|a| a == "--dry-run");
    // starts from the in-game panel don't count as a choice of account
    let remember = !args.iter().any(|a| a == "--no-remember");
    let args: Vec<String> = args.iter().filter(|a| *a != "--dry-run" && *a != "--no-remember").cloned().collect();
    let Some(query) = args.first() else { bail!("usage: mace start ACCOUNT [LINK] [--dry-run]") };
    let account = accounts::find(query)?;
    let link = args[1..].join(" ");
    let argv = launch::start(&account, (!link.is_empty()).then_some(link.as_str()), dry, remember)?;
    if dry {
        writeln!(out, "{}", argv.join(" "))?;
    } else if argv.iter().any(|a| a == "supervise") {
        writeln!(out, "starting {} (progress: the loading window, `mace list`, or `mace why`)", account.name())?;
    }
    Ok(())
}

fn cmd_open(args: &[String], _out: &mut dyn Write) -> anyhow::Result<()> {
    let Some(url) = args.first() else { bail!("usage: mace open URL") };
    // The link signs Roblox in as the browser's account: open it where that account
    // lives. Unknown yet: the first account (never "last used", which may be an alt
    // whose login the link would replace).
    let account = accounts::browser_account()
        .or_else(|| accounts::accounts().into_iter().next())
        .ok_or_else(|| anyhow!("there are no accounts yet: open Mace and add one"))?;
    if let Err(error) = launch::start(&account, Some(url), false, false) {
        // from a browser click: say it where it's seen
        notify("Couldn't start Roblox", &error.to_string(), &[], false);
        return Err(error);
    }
    Ok(())
}

fn cmd_stop(args: &[String], _out: &mut dyn Write) -> anyhow::Result<()> {
    let account = account_arg(args, false)?;
    if !processes::stop(&account, &accounts::accounts(), Duration::from_secs(2)) {
        bail!("{} is still running", account.name());
    }
    Ok(())
}

fn cmd_gather(args: &[String], out: &mut dyn Write) -> anyhow::Result<()> {
    writeln!(out, "joining: {}", launch::gather(&account_arg(args, true)?)?.join(", "))?;
    Ok(())
}

fn cmd_rejoin(args: &[String], _out: &mut dyn Write) -> anyhow::Result<()> {
    let account = account_arg(args, true)?;
    let (server, _) = servers::current(&account);
    let Some(link) = server.get("link").and_then(Value::as_str) else { bail!("{} isn't in a server", account.name()) };
    launch::start(&account, Some(link), false, false)?;
    Ok(())
}

fn copy_server(args: &[String], what: &str) -> anyhow::Result<()> {
    let live = live();
    let found = current_servers(&live);
    let account = match args.first() {
        Some(query) => Some(accounts::find(query)?),
        None => focused_account().filter(|a| found.contains_key(&a.id)).or_else(|| {
            let latest = found.iter().max_by(|a, b| {
                a.1.get("since").and_then(Value::as_f64).unwrap_or(0.0)
                    .total_cmp(&b.1.get("since").and_then(Value::as_f64).unwrap_or(0.0))
            })?;
            accounts::get(latest.0).ok()
        }),
    };
    let Some(server) = account.and_then(|a| found.get(&a.id).cloned()) else {
        notify("Not in a server", "Join a Roblox server first", &[], false);
        return Ok(());
    };
    copy(str_of(&server, what), false);
    let described = servers::describe(&server);
    let name = described.get("name").and_then(Value::as_str).unwrap_or("this server").to_string();
    if what == "link" {
        notify("Copied join link", &format!("Anyone with it can join {name}"), &[], false);
    } else {
        notify("Copied job ID", &format!("{name}: {}", str_of(&server, "job")), &[], false);
    }
    Ok(())
}

fn cmd_focus(args: &[String], _out: &mut dyn Write) -> anyhow::Result<()> {
    let account = account_arg(args, false)?;
    let pids = live().remove(&account.id).unwrap_or_default();
    if pids.is_empty() || !processes::focus(&pids) {
        bail!("{} has no window", account.name());
    }
    Ok(())
}

fn cmd_mute(args: &[String], out: &mut dyn Write) -> anyhow::Result<()> {
    let account = account_arg(&args[..args.len().min(1)], false)?;
    let want = match args.get(1).map(String::as_str) {
        Some("on") => Some(true),
        Some("off") => Some(false),
        _ => None,
    };
    match processes::set_muted(&account, want) {
        Some(muted) => writeln!(out, "{}", if muted { "muted" } else { "unmuted" })?,
        None => bail!("{} isn't playing any sound", account.name()),
    }
    Ok(())
}

fn progress_bar(label: String) -> impl FnMut(u64, u64) {
    let mut shown = u64::MAX;
    move |done, total| {
        let percent = if total > 0 { done * 100 / total } else { 0 };
        if percent != shown {
            shown = percent;
            eprint!("\r{label}: {percent}% of {} MB", total / 1_000_000);
            if percent == 100 {
                eprintln!();
            }
        }
    }
}

fn cmd_update(args: &[String], out: &mut dyn Write) -> anyhow::Result<()> {
    let everyone = args.is_empty() || args.iter().any(|a| a == "--all");
    let targets = if everyone { accounts::accounts() } else { vec![accounts::find(&args[0])?] };
    let live = live();
    let config = settings::load();
    for account in targets {
        let Some(build) = builds::wanted(&account, None) else {
            eprintln!("{}: can't reach Roblox to check its version", account.name());
            continue;
        };
        if builds::up_to_date(&account, &build) {
            writeln!(out, "{}: Roblox {} ({}), up to date", account.name(), build.version, build.channel)?;
            continue;
        }
        let mut bar = progress_bar(format!("Roblox {}", build.version));
        if live.contains_key(&account.id) {
            builds::ensure(&build, &mut bar)?;
            writeln!(out, "{}: Roblox {} downloaded; it is used at the next start", account.name(), build.version)?;
            continue;
        }
        let bundle = builds::provision(&account, &build, &mut bar)?;
        builds::write_fast_flags(&bundle, &config.for_account(&account.id).fast_flags)?;
        writeln!(out, "{}: Roblox {} ({}) installed", account.name(), build.version, build.channel)?;
    }
    Ok(())
}

fn cmd_why(args: &[String], out: &mut dyn Write) -> anyhow::Result<()> {
    let account = match args.first() {
        Some(query) => accounts::find(query)?,
        None => accounts::last_used().ok_or_else(|| anyhow!("there are no accounts"))?,
    };
    writeln!(out, "{} ({}):", account.name(), account.id)?;
    for line in doctor::diagnose(&account, live().contains_key(&account.id)) {
        writeln!(out, "- {line}")?;
    }
    Ok(())
}

fn cmd_migrate(_args: &[String], out: &mut dyn Write) -> anyhow::Result<()> {
    for line in migrate::run()? {
        writeln!(out, "{line}")?;
    }
    writeln!(out, "downloading each account's Roblox build:")?;
    cmd_update(&["--all".into()], out)
}

fn cmd_integrate(args: &[String], out: &mut dyn Write) -> anyhow::Result<()> {
    let lines = if args.iter().any(|a| a == "--undo") {
        desktop::uninstall()
    } else {
        let command = crate::fsutil::which("mace").unwrap_or_else(launch::mace_exe);
        desktop::install(&command)?
    };
    for line in lines {
        writeln!(out, "{line}")?;
    }
    Ok(())
}

/// Notify each join, on every account, with a button to copy its link.
fn cmd_watch(args: &[String], _out: &mut dyn Write) -> anyhow::Result<()> {
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let mut followers: HashMap<String, LogFollower> = HashMap::new();
    let mut quiet: Vec<String> = Vec::new();
    let mut names: HashMap<String, String> = HashMap::new();
    let mut next_accounts = Instant::now();
    loop {
        if Instant::now() >= next_accounts {
            next_accounts = Instant::now() + Duration::from_secs(5);
            for account in accounts::accounts() {
                names.insert(account.id.clone(), account.name());
                followers.entry(account.id.clone()).or_insert_with(|| {
                    quiet.push(account.id.clone()); // what is already logged is not news
                    LogFollower::new(&account.logs())
                });
            }
        }
        for (id, follower) in followers.iter_mut() {
            let joined = follower.poll();
            if let Some(at) = quiet.iter().position(|q| q == id) {
                quiet.remove(at);
                continue;
            }
            for server in joined {
                let many = names.len() > 1;
                let who = names.get(id).cloned().unwrap_or_else(|| id.clone());
                std::thread::spawn(move || {
                    let info = servers::describe(&server);
                    let place = info.get("location").and_then(Value::as_str).unwrap_or("an unknown location");
                    let mut body = format!("Server in {place}");
                    if many {
                        body.push_str(&format!("\nas {who}"));
                    }
                    let title = match info.get("name").and_then(Value::as_str) {
                        Some(name) => format!("Joined {name}"),
                        None => "Joined a server".into(),
                    };
                    if notify(&title, &body, &[("copy", "Copy join link")], dry_run).as_deref() == Some("copy") {
                        copy(str_of(&server, "link"), dry_run);
                    }
                });
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// What the in-game panel shows: the focused Roblox window, its account and
/// server, and every other account (with its window's address, for switching).
/// One pass over /proc, one pactl and three hyprctl calls, however many
/// accounts; a long-running caller passes `followers` to read only what's new.
pub fn overlay_state(followers: Option<&mut HashMap<String, LogFollower>>) -> Value {
    let everyone = accounts::accounts();
    let snapshot = processes::scan();
    let live = processes::running(&everyone, Some(&snapshot));
    let windows = processes::windows();
    let window = processes::active_window();
    let inputs = if live.is_empty() { Vec::new() } else { processes::sink_inputs() };
    let monitors = processes::monitors();
    let window_pid = window.as_ref().and_then(|w| w.get("pid")).and_then(Value::as_i64).unwrap_or(-1) as i32;
    let focused_id = live.iter().find(|(_, pids)| pids.contains(&window_pid)).map(|(id, _)| id.clone());
    let focused_monitor = monitors.iter().find(|m| m.get("focused") == Some(&json!(true)))
        .and_then(|m| m.get("name")).cloned().unwrap_or(Value::Null);
    let mut followers = followers;
    let mut rows = Vec::new();
    for account in &everyone {
        let who = account.identity();
        let mut server = Map::new();
        let mut address = Value::Null;
        if live.contains_key(&account.id) {
            let current = match followers.as_deref_mut() {
                Some(map) => {
                    let follower = map.entry(account.id.clone()).or_insert_with(|| LogFollower::new(&account.logs()));
                    follower.poll();
                    if follower.server.contains_key("ip") { follower.server.clone() } else { Map::new() }
                }
                None => servers::current(account).0,
            };
            if !current.is_empty() {
                server = servers::describe(&current);
            }
            let pids = processes::prefix_pids(&account.prefix, false, Some(&snapshot));
            address = windows.iter()
                .find(|w| w.get("pid").and_then(Value::as_i64).is_some_and(|p| pids.contains(&(p as i32))))
                .and_then(|w| w.get("address")).cloned().unwrap_or(Value::Null);
        }
        let avatar = (!who.is_empty()).then(|| servers::cached_avatar(&who.id)).flatten();
        let muted = if live.contains_key(&account.id) {
            json!(processes::muted(account, Some(&snapshot), Some(&inputs)))
        } else {
            Value::Null
        };
        let server_out = if server.is_empty() {
            Value::Null
        } else {
            let mut picked = Map::new();
            for key in ["place", "job", "link", "name", "location", "since"] {
                picked.insert(key.into(), server.get(key).cloned().unwrap_or(Value::Null));
            }
            Value::Object(picked)
        };
        rows.push(json!({
            "id": account.id, "name": account.name_with(&who), "username": who.username,
            "avatar": avatar, "fps": supervisor::read_fps(&account.id),
            "launch": supervisor::read_status(&account.id).get("state").cloned().unwrap_or(Value::Null),
            "running": live.contains_key(&account.id), "focused": Some(&account.id) == focused_id.as_ref(),
            "address": address, "muted": muted, "server": server_out,
        }));
    }
    if let Some(map) = followers {
        map.retain(|id, _| live.contains_key(id));
    }
    let geometry = match (&window, &focused_id) {
        (Some(window), Some(_)) => {
            let monitor = monitors.iter().find(|m| m.get("id") == window.get("monitor")).cloned().unwrap_or(json!({}));
            json!({
                "x": window["at"][0], "y": window["at"][1], "width": window["size"][0], "height": window["size"][1],
                "monitor": monitor.get("name"), "address": window.get("address"),
                "monitor_x": monitor.get("x").cloned().unwrap_or(json!(0)),
                "monitor_y": monitor.get("y").cloned().unwrap_or(json!(0)),
            })
        }
        _ => Value::Null,
    };
    json!({"focused": focused_id, "window": geometry, "monitor": focused_monitor, "accounts": rows})
}

fn cmd_overlay_state(_args: &[String], out: &mut dyn Write) -> anyhow::Result<()> {
    writeln!(out, "{}", overlay_state(None))?;
    Ok(())
}

/// Exit status 0 when a Roblox started by Mace (from an account's own copy) runs.
pub fn mace_running() -> bool {
    let everyone = accounts::accounts();
    let live = processes::running(&everyone, None);
    everyone.iter().any(|a| live.get(&a.id).is_some_and(|pids| processes::from_bundle(pids, &a.bundle())))
}

pub type CommandFn = fn(&[String], &mut dyn Write) -> anyhow::Result<()>;

pub fn command(name: &str) -> Option<CommandFn> {
    Some(match name {
        "list" => cmd_list,
        "start" => cmd_start,
        "stop" => cmd_stop,
        "open" => cmd_open,
        "gather" => cmd_gather,
        "rejoin" => cmd_rejoin,
        "link" => |a, _| copy_server(a, "link"),
        "job" => |a, _| copy_server(a, "job"),
        "focus" => cmd_focus,
        "mute" => cmd_mute,
        "update" => cmd_update,
        "gc" => |_, out| {
            let removed = builds::gc(&accounts::accounts());
            if removed.is_empty() { writeln!(out, "nothing to remove")? } else { writeln!(out, "removed: {}", removed.join(", "))? }
            Ok(())
        },
        "why" => cmd_why,
        "migrate" => cmd_migrate,
        "integrate" => cmd_integrate,
        "watch" => cmd_watch,
        "overlay-state" => cmd_overlay_state,
        _ => return None,
    })
}

/// Run a subcommand; the exit status.
pub fn run(args: &[String]) -> i32 {
    let Some(name) = args.first() else { return 2 };
    match name.as_str() {
        "-V" | "--version" => {
            println!("mace {}", crate::VERSION);
            return 0;
        }
        "-h" | "--help" | "help" => {
            println!("{HELP}");
            return 0;
        }
        "active" => return if mace_running() { 0 } else { 1 },
        "loading" => {
            let Some(id) = args.get(1) else { return 2 };
            return crate::ui::run_loading(id);
        }
        "supervise" => {
            let Some(id) = args.get(1) else { return 2 };
            let url = args.iter().position(|a| a == "--url").and_then(|at| args.get(at + 1));
            return supervisor::run(id, url.map(String::as_str));
        }
        _ => {}
    }
    let Some(run) = command(name) else {
        println!("{HELP}");
        return 2;
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match run(&args[1..], &mut out) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("mace: {error:#}");
            1
        }
    }
}

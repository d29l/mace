//! The command line. Commands write to the `out` they're given, so the
//! in-game panel's requests can run them without printing anything.

use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{anyhow, bail};
use serde_json::{Map, Value, json};

use crate::accounts::{self, Account};
use crate::servers;
use crate::{builds, desktop, doctor, launch, migrate, mods, processes, settings, supervisor, upgrade};

pub const HELP: &str = "mace: macOS Roblox on Linux, with multiple accounts.

    mace                          open the window
    mace list [--json]            accounts and what they're doing
    mace start ACCOUNT [LINK]     start an account (LINK may be another account)
    mace stop ACCOUNT
    mace open URL                 open a roblox.com link
    mace gather ACCOUNT           bring every account to ACCOUNT's server
    mace rejoin ACCOUNT           rejoin ACCOUNT's server
    mace link [ACCOUNT]           copy the join link
    mace job [ACCOUNT]            copy the job ID
    mace focus ACCOUNT            focus ACCOUNT's window
    mace mute ACCOUNT [on|off]    mute or unmute (toggles without on/off)
    mace update [ACCOUNT|--all]   update Roblox
    mace gc                       delete unused builds
    mace why [ACCOUNT]            explain the last start
    mace migrate                  import from Nutmeg
    mace integrate [--undo]       menu entries and roblox.com links
    mace servers ACCOUNT [--fewest] [--json]
                                  list the game's public servers
    mace panel [ACCOUNT]          toggle the in-game panel
    mace upgrade                  update Mace itself
    mace --version

ACCOUNT: an id (main, alt1, ...), name, username or display name.
LINK: a join link, a game/share/roblox:// link, or \"PLACEID JOBID\".";

/// A notification; with actions, it waits and returns the one clicked. The
/// icon goes by its name in the icon theme (installed by `mace integrate`):
/// a file path would arrive as an image beside the text instead.
pub fn notify(title: &str, body: &str, actions: &[(&str, &str)], dry_run: bool) -> Option<String> {
    if dry_run {
        println!("[toast] {title} | {body} | {actions:?}");
        return None;
    }
    if crate::fsutil::which("notify-send").is_none() {
        println!("{title}: {body}");
        return None;
    }
    let mut command = Command::new("notify-send");
    command.args(["-a", "Mace"]);
    command.args(["-i", crate::APP_ID]);
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
            eprintln!("\nnote: {} are all signed in as @{name}",
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
        writeln!(out, "starting {}", account.name())?;
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
        .ok_or_else(|| anyhow!("no accounts yet: add one in Mace"))?;
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
    let Some((account, server)) = account.and_then(|a| found.get(&a.id).cloned().map(|s| (a, s))) else {
        bail!("not in a server");
    };
    copy(str_of(&server, what), false);
    let described = servers::describe(&server);
    let name = described.get("name").and_then(Value::as_str).unwrap_or("this server").to_string();
    let (title, body) = if what == "link" {
        ("Join link copied".to_string(), format!("Anyone with it can join {name}"))
    } else {
        ("Job ID copied".to_string(), format!("{name}: {}", str_of(&server, "job")))
    };
    // said in the game; on the command line too, for when it was typed there
    crate::panel::toast(&account.id, &title, &body);
    println!("{title}: {body}");
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
        if !live.contains_key(&account.id) && builds::clear_updater_leftovers(&account) {
            writeln!(out, "{}: cleared an unfinished Roblox update", account.name())?;
        }
        let Some(build) = builds::wanted(&account, None) else {
            eprintln!("{}: can't check for updates (offline?)", account.name());
            continue;
        };
        if builds::up_to_date(&account, &build) {
            writeln!(out, "{}: Roblox {} ({}), up to date", account.name(), build.version, build.channel)?;
            continue;
        }
        let mut bar = progress_bar(format!("Roblox {}", build.version));
        if live.contains_key(&account.id) {
            builds::ensure(&build, &mut bar)?;
            writeln!(out, "{}: Roblox {} downloaded, used at the next start", account.name(), build.version)?;
            continue;
        }
        let bundle = builds::provision(&account, &build, &mut bar)?;
        builds::write_fast_flags(&bundle, &config.for_account(&account.id).fast_flags)?;
        if let Err(error) = mods::apply(&account) {
            eprintln!("{}: mods not applied: {error:#}", account.name());
        }
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
    writeln!(out, "updating:")?;
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


fn cmd_servers(args: &[String], out: &mut dyn Write) -> anyhow::Result<()> {
    let json = args.iter().any(|a| a == "--json");
    let sort = if args.iter().any(|a| a == "--fewest") { mace_proto::Sort::Fewest } else { mace_proto::Sort::Most };
    let Some(query) = args.iter().find(|a| !a.starts_with("--")) else {
        bail!("usage: mace servers ACCOUNT [--fewest] [--json]")
    };
    let account = accounts::find(query)?;
    let (server, _) = servers::current(&account);
    let Some(place) = server.get("place").and_then(Value::as_str) else { bail!("{} isn't in a game", account.name()) };
    let here = server.get("job").and_then(Value::as_str).unwrap_or("");
    let page = servers::public_page(place, sort, None);
    if json {
        writeln!(out, "{}", serde_json::to_string(&page)?)?;
        return Ok(());
    }
    if !page.error.is_empty() {
        bail!("{}", page.error);
    }
    for s in &page.list {
        let fps = s.fps.map(|f| format!("{:.0} FPS", f)).unwrap_or_default();
        let ping = s.ping.map(|p| format!("~{p} ms")).unwrap_or_default();
        let mark = if s.id == here { "  (you're here)" } else { "" };
        writeln!(out, "{:>3}/{:<3} {:>7} {:>8}  {}  {}{}", s.playing, s.max, fps, ping, s.id,
                 s.location.as_deref().unwrap_or(""), mark)?;
    }
    if page.next.is_some() {
        writeln!(out, "(more in the in-game panel)")?;
    }
    Ok(())
}

fn cmd_upgrade(_args: &[String], out: &mut dyn Write) -> anyhow::Result<()> {
    if upgrade::installed_copy().is_none() {
        bail!("this Mace runs from {}, not the copy install.sh makes: update it with git pull and ./install.sh",
              launch::mace_exe().display());
    }
    writeln!(out, "looking for a newer Mace than {}", crate::VERSION)?;
    match upgrade::update_now()? {
        Some(version) => writeln!(out, "Mace {version} installed; games already running keep the old one")?,
        None => writeln!(out, "Mace {} is up to date", crate::VERSION)?,
    }
    Ok(())
}

fn cmd_panel(args: &[String], _out: &mut dyn Write) -> anyhow::Result<()> {
    let args: Vec<&String> = args.iter().filter(|a| *a != "toggle").collect();
    let account = match args.first() {
        Some(query) => accounts::find(query)?,
        None => {
            let everyone = accounts::accounts();
            let live = processes::running(&everyone, None);
            let mut running = everyone.into_iter().filter(|a| live.contains_key(&a.id));
            match (running.next(), running.next()) {
                (Some(only), None) => only,
                (None, _) => bail!("no account is running"),
                _ => bail!("several accounts are running: mace panel ACCOUNT"),
            }
        }
    };
    crate::panel::toggle(&account)
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
        "servers" => cmd_servers,
        "panel" => cmd_panel,
        "upgrade" => cmd_upgrade,
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

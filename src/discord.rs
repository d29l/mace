//! Your Discord status ("Rich Presence"): the game you're playing, as which
//! account and since when, told to the Discord app on this computer over its
//! local socket. Every running account's supervisor runs `run`, but only the
//! one holding a lock talks to Discord; when its game closes, another takes over.

use std::collections::HashMap;
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use serde_json::{Map, Value, json};

use crate::accounts::{self, Account};
use crate::servers::{self, LogFollower};
use crate::{panel, paths, processes, settings, supervisor};

/// Mace's application on Discord, named "Roblox": that name is the "Playing ___".
/// Not a secret: every Mace uses it.
pub const APP_ID: &str = "1572147768573698148";

const HANDSHAKE: u32 = 0;
const FRAME: u32 = 1;
const CLOSE: u32 = 2;
const PING: u32 = 3;
const PONG: u32 = 4;

/// How often the game shown is looked at again.
const EVERY: Duration = Duration::from_secs(3);
/// How long to wait before trying a Discord that wasn't there.
const RETRY: Duration = Duration::from_secs(15);

/// Where Discord listens: the app itself, or Flatpak, Snap and Vesktop installs.
fn sockets() -> Vec<PathBuf> {
    let root = paths::runtime_root();
    let dirs = [
        root.clone(),
        root.join("app/com.discordapp.Discord"),
        root.join("app/com.discordapp.DiscordCanary"),
        root.join("app/dev.vencord.Vesktop"),
        root.join("snap.discord"),
        root.join("snap.discord-canary"),
    ];
    dirs.iter().flat_map(|d| (0..10).map(move |i| d.join(format!("discord-ipc-{i}")))).collect()
}

/// A connection to the Discord app.
pub struct Discord {
    stream: UnixStream,
    nonce: u64,
}

impl Discord {
    pub fn connect(app_id: &str) -> anyhow::Result<Discord> {
        for path in sockets() {
            let Ok(stream) = UnixStream::connect(&path) else { continue };
            stream.set_read_timeout(Some(Duration::from_secs(3)))?;
            stream.set_write_timeout(Some(Duration::from_secs(3)))?;
            let mut discord = Discord { stream, nonce: 0 };
            discord.send(HANDSHAKE, &json!({ "v": 1, "client_id": app_id }))?;
            loop {
                let (op, data) = discord.read()?;
                match op {
                    FRAME if data.get("evt").and_then(Value::as_str) == Some("READY") => return Ok(discord),
                    FRAME => continue,
                    PING => discord.send(PONG, &data)?,
                    CLOSE => bail!("Discord said no: {}", data.get("message").and_then(Value::as_str).unwrap_or("")),
                    _ => bail!("Discord answered with {op}"),
                }
            }
        }
        bail!("Discord isn't running")
    }

    fn send(&mut self, op: u32, data: &Value) -> anyhow::Result<()> {
        let body = data.to_string();
        let mut frame = Vec::with_capacity(8 + body.len());
        frame.extend(op.to_le_bytes());
        frame.extend((body.len() as u32).to_le_bytes());
        frame.extend(body.as_bytes());
        self.stream.write_all(&frame).context("writing to Discord")
    }

    fn read(&mut self) -> anyhow::Result<(u32, Value)> {
        let mut head = [0u8; 8];
        self.stream.read_exact(&mut head).context("reading from Discord")?;
        let op = u32::from_le_bytes(head[..4].try_into().unwrap());
        let len = u32::from_le_bytes(head[4..].try_into().unwrap()) as usize;
        if len > 1 << 20 {
            bail!("Discord sent {len} bytes");
        }
        let mut body = vec![0; len];
        self.stream.read_exact(&mut body).context("reading from Discord")?;
        Ok((op, serde_json::from_slice(&body).unwrap_or(Value::Null)))
    }

    /// Show this activity, or none.
    pub fn set(&mut self, activity: Option<&Value>) -> anyhow::Result<()> {
        self.nonce += 1;
        let nonce = self.nonce.to_string();
        self.send(FRAME, &json!({
            "cmd": "SET_ACTIVITY",
            "args": { "pid": std::process::id(), "activity": activity },
            "nonce": nonce,
        }))?;
        loop {
            let (op, data) = self.read()?;
            match op {
                PING => self.send(PONG, &data)?,
                CLOSE => bail!("Discord closed the connection"),
                FRAME if data.get("nonce").and_then(Value::as_str) == Some(nonce.as_str()) => {
                    if data.get("evt").and_then(Value::as_str) == Some("ERROR") {
                        bail!("Discord: {}", data.pointer("/data/message").and_then(Value::as_str).unwrap_or("error"));
                    }
                    return Ok(());
                }
                _ => {}
            }
        }
    }

    /// Discord went away (quit or restarted).
    fn gone(&mut self) -> bool {
        let mut byte = [0u8; 1];
        // SAFETY: a peek of at most one byte into a buffer of one, on a socket this owns.
        let n = unsafe {
            libc::recv(self.stream.as_raw_fd(), byte.as_mut_ptr().cast(), 1, libc::MSG_PEEK | libc::MSG_DONTWAIT)
        };
        match n {
            0 => true,
            n if n > 0 => false,
            _ => std::io::Error::last_os_error().kind() != ErrorKind::WouldBlock,
        }
    }
}

/// What the status says.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Presence {
    pub game: String,
    pub creator: Option<String>,
    /// The game's icon, on Roblox's image servers.
    pub icon: Option<String>,
    /// When you joined the game (unix seconds).
    pub since: f64,
    /// "Display (@username)", and the account's headshot.
    pub account: Option<(String, Option<String>)>,
    /// A link anyone can open to join the server.
    pub join: Option<String>,
}

/// Discord wants texts of 2 to 128 characters.
fn text(value: &str) -> String {
    let mut out: String = value.chars().take(128).collect();
    while out.chars().count() < 2 {
        out.push(' ');
    }
    out
}

pub fn activity(p: &Presence) -> Value {
    let mut activity = Map::new();
    activity.insert("type".into(), json!(0));
    activity.insert("details".into(), json!(text(&p.game)));
    if let Some(creator) = &p.creator {
        activity.insert("state".into(), json!(text(&format!("by {creator}"))));
    }
    activity.insert("timestamps".into(), json!({ "start": (p.since * 1000.0) as i64 }));
    let mut assets = Map::new();
    if let Some(icon) = &p.icon {
        assets.insert("large_image".into(), json!(icon));
        assets.insert("large_text".into(), json!(text(&p.game)));
    }
    if let Some((who, picture)) = &p.account {
        if let Some(picture) = picture {
            assets.insert("small_image".into(), json!(picture));
        }
        assets.insert("small_text".into(), json!(text(who)));
    }
    if !assets.is_empty() {
        activity.insert("assets".into(), Value::Object(assets));
    }
    if let Some(link) = p.join.as_ref().filter(|l| l.starts_with("https://") && l.len() <= 512) {
        activity.insert("buttons".into(), json!([{ "label": "Join server", "url": link }]));
    }
    Value::Object(activity)
}

/// The game to show, of the accounts in one: the one you played last, or else
/// whichever joined its server most recently.
fn pick(in_game: Vec<(Account, Map<String, Value>)>, focused: Option<&str>) -> Option<(Account, Map<String, Value>)> {
    let since = |s: &Map<String, Value>| s.get("since").and_then(Value::as_f64).unwrap_or(0.0);
    match in_game.iter().position(|(a, _)| Some(a.id.as_str()) == focused) {
        Some(i) => in_game.into_iter().nth(i),
        None => in_game.into_iter().max_by(|a, b| since(&a.1).total_cmp(&since(&b.1))),
    }
}

/// The lock that makes one Mace process the one talking to Discord.
fn take_lock() -> Option<File> {
    let dir = paths::runtime();
    std::fs::create_dir_all(&dir).ok()?;
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("discord.lock")).ok()?;
    // SAFETY: flock on a descriptor this file owns; it's let go when the file closes.
    (unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0).then_some(file)
}

/// Picks the game to show and works out its status.
#[derive(Default)]
struct Watcher {
    followers: HashMap<String, LogFollower>,
    /// The game shown and since when, kept while you teleport around in it.
    started: Option<(String, f64)>,
    /// The status worked out for (account, server, join button), and when:
    /// looked up again only when that changes, or a minute on if a lookup failed.
    made: Option<((String, String, bool), Presence, Instant)>,
}

impl Watcher {
    fn presence(&mut self, join_button: bool) -> Option<(String, Presence)> {
        let everyone = accounts::accounts();
        let live = processes::running(&everyone, None);
        self.followers.retain(|id, _| live.contains_key(id));
        let mut in_game = Vec::new();
        for account in everyone.into_iter().filter(|a| live.contains_key(&a.id)) {
            let follower = self.followers.entry(account.id.clone()).or_insert_with(|| LogFollower::new(&account.logs()));
            follower.poll();
            if follower.server.contains_key("ip") {
                let server = follower.server.clone();
                in_game.push((account, server));
            }
        }
        let Some((account, server)) = pick(in_game, panel::last_focused().as_deref()) else {
            (self.started, self.made) = (None, None);
            return None;
        };
        let get = |k: &str| server.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()).map(String::from);
        let key = (account.id.clone(), get("job").unwrap_or_default(), join_button);
        if let Some((made_for, presence, at)) = &self.made {
            let whole = presence.icon.is_some() && presence.account.as_ref().is_none_or(|(_, picture)| picture.is_some());
            if *made_for == key && (whole || at.elapsed() < Duration::from_secs(60)) {
                return Some((account.id, presence.clone()));
            }
        }
        let universe = get("universe").or_else(|| get("place")).unwrap_or_default();
        let info = servers::game_info(get("universe").as_deref());
        let game = info.as_ref().map(|i| i.name.clone())
            .or_else(|| servers::describe(&server).get("name").and_then(Value::as_str).map(String::from))
            .unwrap_or_else(|| "A Roblox game".into());
        let joined = server.get("since").and_then(Value::as_f64).unwrap_or_else(servers::unix_time);
        let since = match &self.started {
            Some((shown, since)) if *shown == universe => *since,
            _ => joined,
        };
        self.started = Some((universe, since));
        let who = account.identity();
        let account_line = (!who.is_empty()).then(|| {
            let name = if who.display.is_empty() || who.display == who.username { format!("@{}", who.username) }
                       else { format!("{} (@{})", who.display, who.username) };
            (name, servers::avatar_url(&who.id))
        });
        let presence = Presence {
            game,
            creator: info.and_then(|i| i.creator),
            icon: get("universe").and_then(|u| servers::game_icon_url(&u, 512)),
            since,
            account: account_line,
            join: if join_button { get("link") } else { None },
        };
        self.made = Some((key, presence.clone(), Instant::now()));
        Some((account.id, presence))
    }
}

/// Keep Discord's status on the game you're playing until `stopped` (this
/// supervisor's account is done); `me` is the account whose log notes it.
pub fn run(me: &str, stopped: &AtomicBool) {
    if !APP_ID.is_empty() {
        serve(APP_ID, me, stopped, EVERY);
    }
}

fn serve(app_id: &str, me: &str, stopped: &AtomicBool, every: Duration) {
    let mut lock: Option<File> = None;
    let mut discord: Option<Discord> = None;
    let mut shown: Option<Value> = None;
    let mut watcher = Watcher::default();
    let mut retry_at = Instant::now();
    loop {
        let until = Instant::now() + every;
        while Instant::now() < until {
            if stopped.load(Ordering::Relaxed) {
                if let Some(discord) = discord.as_mut() {
                    let _ = discord.set(None);
                }
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let config = settings::load();
        if !config.discord {
            // off: clear the status, and let go so nothing talks to Discord
            if let Some(mut d) = discord.take() {
                let _ = d.set(None);
            }
            (lock, shown) = (None, None);
            continue;
        }
        if lock.is_none() {
            lock = take_lock();
            if lock.is_none() {
                continue;
            }
        }
        if discord.as_mut().is_some_and(Discord::gone) {
            (discord, shown) = (None, None);
        }
        let wanted = watcher.presence(config.discord_join);
        let activity = wanted.as_ref().map(|(_, p)| activity(p));
        if activity == shown && discord.is_some() {
            continue;
        }
        if activity.is_none() && discord.is_none() {
            shown = None;
            continue;
        }
        if discord.is_none() {
            if Instant::now() < retry_at {
                continue;
            }
            match Discord::connect(app_id) {
                Ok(d) => discord = Some(d),
                Err(_) => {
                    retry_at = Instant::now() + RETRY;
                    continue;
                }
            }
        }
        let Some(d) = discord.as_mut() else { continue };
        match d.set(activity.as_ref()) {
            Ok(()) => {
                let what = wanted.as_ref().map_or("nothing".into(), |(account, p)| format!("{} ({account})", p.game));
                supervisor::note_for(me, &format!("discord: showing {what}"));
                shown = activity;
            }
            Err(error) => {
                supervisor::note_for(me, &format!("discord: {error:#}"));
                (discord, shown) = (None, None);
                retry_at = Instant::now() + RETRY;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;

    fn frame(op: u32, data: &Value) -> Vec<u8> {
        let body = data.to_string();
        let mut out = op.to_le_bytes().to_vec();
        out.extend((body.len() as u32).to_le_bytes());
        out.extend(body.as_bytes());
        out
    }

    fn read_frame(stream: &mut UnixStream) -> Option<(u32, Value)> {
        let mut head = [0u8; 8];
        stream.read_exact(&mut head).ok()?;
        let len = u32::from_le_bytes(head[4..].try_into().unwrap()) as usize;
        let mut body = vec![0; len];
        stream.read_exact(&mut body).ok()?;
        Some((u32::from_le_bytes(head[..4].try_into().unwrap()), serde_json::from_slice(&body).unwrap()))
    }

    /// A stand-in Discord: answers the handshake and every command (ERROR for an
    /// activity whose details say "bad"), and passes on what it was sent.
    fn fake_discord(home: &TempHome) -> mpsc::Receiver<(u32, Value)> {
        let listener = UnixListener::bind(home.path().join("run/discord-ipc-0")).unwrap();
        let (tell, heard) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                while let Some((op, data)) = read_frame(&mut stream) {
                    let _ = tell.send((op, data.clone()));
                    let answer = if op == HANDSHAKE {
                        json!({ "cmd": "DISPATCH", "evt": "READY", "data": { "v": 1 } })
                    } else if data.pointer("/args/activity/details") == Some(&json!("bad")) {
                        json!({ "cmd": "SET_ACTIVITY", "evt": "ERROR", "nonce": data["nonce"], "data": { "message": "bad" } })
                    } else {
                        json!({ "cmd": "SET_ACTIVITY", "evt": null, "nonce": data["nonce"], "data": {} })
                    };
                    // a ping first, as Discord may send one any time
                    if op != HANDSHAKE {
                        let _ = stream.write_all(&frame(PING, &json!({ "n": 1 })));
                    }
                    let _ = stream.write_all(&frame(FRAME, &answer));
                }
            }
        });
        heard
    }

    fn presence() -> Presence {
        Presence {
            game: "Bee Swarm Simulator".into(),
            creator: Some("Onett".into()),
            icon: Some("https://tr.rbxcdn.com/icon/512/Png".into()),
            since: 1_700_000_000.5,
            account: Some(("Player One (@playerone)".into(), Some("https://tr.rbxcdn.com/head/150/Png".into()))),
            join: Some("https://www.roblox.com/games/start?placeId=1&gameInstanceId=j".into()),
        }
    }

    #[test]
    fn the_status_names_the_game_the_account_and_maybe_a_join_button() {
        let full = activity(&presence());
        assert_eq!(full["details"], "Bee Swarm Simulator");
        assert_eq!(full["state"], "by Onett");
        assert_eq!(full["timestamps"]["start"], 1_700_000_000_500i64);
        assert_eq!(full["assets"]["large_image"], "https://tr.rbxcdn.com/icon/512/Png");
        assert_eq!(full["assets"]["small_image"], "https://tr.rbxcdn.com/head/150/Png");
        assert_eq!(full["assets"]["small_text"], "Player One (@playerone)");
        assert_eq!(full["buttons"][0]["label"], "Join server");
        let plain = activity(&Presence { join: None, creator: None, account: None, icon: None, game: "X".into(), ..presence() });
        assert_eq!(plain["details"], "X ");
        assert!(plain.get("buttons").is_none() && plain.get("assets").is_none() && plain.get("state").is_none());
        // Discord takes only https links in buttons
        let odd = activity(&Presence { join: Some("roblox://experiences/start?placeId=1".into()), ..presence() });
        assert!(odd.get("buttons").is_none());
    }

    #[test]
    fn it_talks_to_discord_over_its_socket() {
        let home = TempHome::new();
        assert!(Discord::connect("123").is_err(), "no Discord yet");
        let heard = fake_discord(&home);
        let mut discord = Discord::connect("123").unwrap();
        let (op, hello) = heard.recv().unwrap();
        assert_eq!((op, hello), (HANDSHAKE, json!({ "v": 1, "client_id": "123" })));
        let shown = activity(&presence());
        discord.set(Some(&shown)).unwrap();
        let (_, sent) = heard.recv().unwrap();
        assert_eq!(sent["cmd"], "SET_ACTIVITY");
        assert_eq!(sent["args"]["pid"], std::process::id());
        assert_eq!(sent["args"]["activity"], shown);
        // the ping was answered with a pong
        assert_eq!(heard.recv().unwrap(), (PONG, json!({ "n": 1 })));
        discord.set(None).unwrap();
        let (_, cleared) = heard.recv().unwrap();
        assert_eq!(cleared["args"]["activity"], Value::Null);
        let error = discord.set(Some(&json!({ "details": "bad" }))).unwrap_err();
        assert!(error.to_string().contains("bad"), "{error}");
        assert!(!discord.gone());
    }

    #[test]
    fn the_game_being_played_reaches_discord() {
        let home = TempHome::new();
        let storage = json!({ "UserId": "42", "Username": "playerone", "DisplayName": "Player One" });
        let account = accounts::register(Account::new("main", home.make_prefix("main", Some(storage)), "", false)).unwrap();
        let log = account.logs().join("0.1.0.1_20261115T200000Z_Player_aaaaa_last.log");
        std::fs::write(&log, servers::tests::join_lines().join("\n") + "\n").unwrap();
        // Roblox's answers, so nothing goes online
        servers::remember_json("https://games.roblox.com/v1/games?universeIds=777",
            json!({ "data": [{ "name": "Obby", "creator": { "name": "Bob" }, "rootPlaceId": 1818 }] }));
        servers::remember_json(
            "https://thumbnails.roblox.com/v1/games/icons?universeIds=777&size=512x512&format=Png&isCircular=false",
            json!({ "data": [{ "state": "Completed", "imageUrl": "https://tr.rbxcdn.com/obby/512" }] }));
        servers::remember_json(
            "https://thumbnails.roblox.com/v1/users/avatar-headshot?userIds=42&size=150x150&format=Png&isCircular=false",
            json!({ "data": [{ "state": "Completed", "imageUrl": "https://tr.rbxcdn.com/player/150" }] }));
        // the account's Roblox, running (as far as anyone can tell)
        let idle = home.path().join("idle");
        std::fs::write(&idle, "#!/bin/bash\nsleep 30\n").unwrap();
        let mut game = std::process::Command::new("bash").arg(&idle).arg("run").arg(account.bundle())
            .env("ROSEMARY_PREFIX", &account.prefix).spawn().unwrap();
        let heard = fake_discord(&home);
        let stopped = std::sync::Arc::new(AtomicBool::new(false));
        let serving = {
            let stopped = stopped.clone();
            std::thread::spawn(move || serve("123", "main", &stopped, Duration::from_millis(100)))
        };
        let next_activity = || loop {
            let (op, data) = heard.recv_timeout(Duration::from_secs(10)).expect("Discord heard nothing");
            if op == FRAME {
                return data["args"]["activity"].clone();
            }
        };
        let shown = next_activity();
        assert_eq!(shown["details"], "Obby");
        assert_eq!(shown["state"], "by Bob");
        assert_eq!(shown["timestamps"]["start"], 1794772800000i64);
        assert_eq!(shown["assets"]["large_image"], "https://tr.rbxcdn.com/obby/512");
        assert_eq!(shown["assets"]["small_image"], "https://tr.rbxcdn.com/player/150");
        assert_eq!(shown["assets"]["small_text"], "Player One (@playerone)");
        assert!(shown.get("buttons").is_none(), "no join button unless it's turned on");
        // turned on: the button comes
        settings::save(&settings::Settings { discord_join: true, ..Default::default() }).unwrap();
        let shown = next_activity();
        assert_eq!(shown["buttons"][0]["url"], format!("https://www.roblox.com/games/start?placeId=1818&gameInstanceId={}",
                                                        servers::tests::JOB));
        // the game ends: the status goes
        let _ = game.kill();
        let _ = game.wait();
        assert_eq!(next_activity(), Value::Null);
        stopped.store(true, Ordering::Relaxed);
        serving.join().unwrap();
    }

    #[test]
    fn one_process_talks_to_discord() {
        let _home = TempHome::new();
        let first = take_lock().unwrap();
        assert!(take_lock().is_none());
        drop(first);
        assert!(take_lock().is_some());
    }

    #[test]
    fn the_game_you_played_last_is_shown() {
        let home = TempHome::new();
        let account = |id: &str| Account::new(id, home.make_prefix(id, None), "", false);
        let server = |since: f64| json!({ "since": since }).as_object().unwrap().clone();
        let playing = || vec![(account("main"), server(100.0)), (account("alt"), server(200.0))];
        assert_eq!(pick(playing(), Some("main")).unwrap().0.id, "main");
        // nothing focused, or the focused one isn't in a game: the newest join
        assert_eq!(pick(playing(), None).unwrap().0.id, "alt");
        assert_eq!(pick(playing(), Some("gone")).unwrap().0.id, "alt");
        assert!(pick(Vec::new(), Some("main")).is_none());
    }
}

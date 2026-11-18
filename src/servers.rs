//! Which server each account is in, read from its Roblox logs, and lookups for
//! the game's name, the server's location and avatars.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use regex::Regex;
use serde_json::{Map, Value, json};

use crate::accounts::Account;
use crate::links::share_link;
use crate::{builds, fsutil, paths};
use mace_proto::{PublicServer, ServerPage, Sort};

static JOINING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"! Joining game '([0-9a-f-]+)' place (\d+) at ").unwrap());
static UNIVERSE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"placeid:(\d+), userid:\d+, universeid:(\d+)").unwrap());
static UDMUX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"UDMUX Address = ([0-9.]+), Port = (\d+)").unwrap());
// also logged for the old server just after a teleport's join, so it only
// counts when it isn't right behind one
const DISCONNECTED: &str = "Time to disconnect replication data";
const LEFT_HOME: &str = "leaveUGCGameInternal";
static STAMP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+Z,([0-9.]+),").unwrap());
pub static PLAYER_LOG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"_Player_[0-9a-f]+_last\.log$").unwrap());
const TELEPORT_GRACE: f64 = 10.0; // seconds of log time

pub fn unix_time() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

pub fn newest_player_log(logs: &Path) -> Option<PathBuf> {
    std::fs::read_dir(logs)
        .ok()?
        .flatten()
        .filter(|e| PLAYER_LOG.is_match(&e.file_name().to_string_lossy()))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max_by_key(|(time, _)| *time)
        .map(|(_, path)| path)
}

/// Follows the newest player log in a logs folder and knows the server the
/// client is in. `poll()` returns the servers joined since the last call.
pub struct LogFollower {
    logs: PathBuf,
    path: Option<PathBuf>,
    file: Option<BufReader<File>>,
    partial: Vec<u8>,
    pub server: Map<String, Value>,
    /// servers joined in this log, oldest first
    pub history: Vec<Map<String, Value>>,
    checked: Option<Instant>,
}

impl LogFollower {
    pub fn new(logs: &Path) -> LogFollower {
        LogFollower { logs: logs.to_path_buf(), path: None, file: None, partial: Vec::new(), server: Map::new(),
                      history: Vec::new(), checked: None }
    }

    pub fn poll(&mut self) -> Vec<Map<String, Value>> {
        let mut joined = Vec::new();
        if self.checked.is_none_or(|t| t.elapsed() > Duration::from_secs(2)) {
            self.checked = Some(Instant::now());
            if let Some(newest) = newest_player_log(&self.logs) {
                if Some(&newest) != self.path.as_ref() {
                    self.file = File::open(&newest).ok().map(BufReader::new);
                    self.path = Some(newest);
                    self.partial.clear();
                    self.server.clear();
                    self.history.clear();
                }
            }
        }
        let Some(mut file) = self.file.take() else { return joined };
        loop {
            let mut line = Vec::new();
            match file.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if !line.ends_with(b"\n") {
                self.partial.extend(line); // the rest is still being written
                break;
            }
            let mut whole = std::mem::take(&mut self.partial);
            whole.extend(line);
            if let Some(server) = self.handle(&String::from_utf8_lossy(&whole)) {
                joined.push(server);
            }
        }
        self.file = Some(file);
        joined
    }

    fn handle(&mut self, line: &str) -> Option<Map<String, Value>> {
        let clock = STAMP.captures(line).and_then(|m| m[1].parse::<f64>().ok());
        if let Some(m) = JOINING.captures(line) {
            let mut server = Map::new();
            server.insert("place".into(), json!(&m[2]));
            server.insert("job".into(), json!(&m[1]));
            server.insert("link".into(), json!(share_link(&m[2], &m[1])));
            server.insert("since".into(), json!(unix_time()));
            server.insert("joined_at".into(), json!(clock));
            self.server = server;
        } else if let Some(m) = UNIVERSE.captures(line) {
            if self.server.get("place").and_then(Value::as_str) == Some(&m[1]) {
                self.server.insert("universe".into(), json!(&m[2]));
            }
        } else if let Some(m) = UDMUX.captures(line) {
            if self.server.contains_key("job") && !self.server.contains_key("ip") {
                self.server.insert("ip".into(), json!(&m[1]));
                self.history.push(self.server.clone());
                return Some(self.server.clone());
            }
        } else if line.contains(LEFT_HOME) && !self.server.is_empty() {
            self.server.clear();
        } else if line.contains(DISCONNECTED) && !self.server.is_empty() {
            let joined_at = self.server.get("joined_at").and_then(Value::as_f64);
            match (joined_at, clock) {
                (Some(joined), Some(now)) if now - joined <= TELEPORT_GRACE => {}
                _ => self.server.clear(),
            }
        }
        None
    }

    pub fn close(&mut self) {
        self.file = None;
    }
}

static LOOKUPS: LazyLock<Mutex<HashMap<String, Value>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// A JSON answer from a web service, remembered for the life of this process.
pub fn fetch_json(url: &str) -> Option<Value> {
    if let Some(found) = LOOKUPS.lock().unwrap().get(url) {
        return Some(found.clone());
    }
    let data: Value = builds::agent(Duration::from_secs(6)).get(url).call().ok()?.body_mut().read_json().ok()?;
    LOOKUPS.lock().unwrap().insert(url.to_string(), data.clone());
    Some(data)
}

/// Pages of public servers are reused this long.
const PAGE_FRESH: Duration = Duration::from_secs(30);
/// After Roblox says "too many requests", nothing is asked for this long.
const SLOW_DOWN: Duration = Duration::from_secs(10);
static PAGES: LazyLock<Mutex<HashMap<String, (Instant, ServerPage)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static PAUSED_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);

/// A page of a game's public servers that have room, from Roblox (no login
/// needed): fullest first or emptiest first, 100 at a time.
pub fn public_page(place: &str, sort: Sort, cursor: Option<&str>) -> ServerPage {
    let mut page = ServerPage { place: place.into(), sort, cursor: cursor.map(String::from), ..Default::default() };
    if place.is_empty() || !place.bytes().all(|b| b.is_ascii_digit()) {
        page.error = "No game to list servers for".into();
        return page;
    }
    if std::env::var_os("MACE_OFFLINE").is_some() {
        page.error = "Offline".into();
        return page;
    }
    let key = format!("{place} {sort:?} {}", cursor.unwrap_or(""));
    if let Some((at, cached)) = PAGES.lock().unwrap().get(&key)
        && at.elapsed() < PAGE_FRESH
    {
        return cached.clone();
    }
    let pause = "Roblox asked for a break; try again in a few seconds";
    if PAUSED_UNTIL.lock().unwrap().is_some_and(|until| Instant::now() < until) {
        page.error = pause.into();
        return page;
    }
    let order = match sort { Sort::Most => "Desc", Sort::Fewest => "Asc" };
    let mut url = format!(
        "https://games.roblox.com/v1/games/{place}/servers/Public?sortOrder={order}&excludeFullGames=true&limit=100");
    if let Some(cursor) = cursor {
        url.push_str("&cursor=");
        url.push_str(&crate::links::encode(cursor));
    }
    match builds::agent(Duration::from_secs(8)).get(&url).call() {
        Ok(mut response) => match response.body_mut().read_json::<Value>() {
            Ok(data) => {
                fill_page(&data, &mut page);
                PAGES.lock().unwrap().insert(key, (Instant::now(), page.clone()));
            }
            Err(_) => page.error = "Roblox's answer didn't make sense".into(),
        },
        Err(ureq::Error::StatusCode(429)) => {
            *PAUSED_UNTIL.lock().unwrap() = Some(Instant::now() + SLOW_DOWN);
            page.error = pause.into();
        }
        Err(ureq::Error::StatusCode(code)) => page.error = format!("Roblox answered {code}"),
        Err(_) => page.error = "Couldn't reach Roblox".into(),
    }
    page
}

/// Roblox's answer as a page; servers one of your accounts has been in get their place.
fn fill_page(data: &Value, page: &mut ServerPage) {
    let known = match fsutil::read_json(&described_file()) {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    };
    page.next = data.get("nextPageCursor").and_then(Value::as_str).filter(|c| !c.is_empty()).map(String::from);
    for s in data.get("data").and_then(Value::as_array).into_iter().flatten() {
        let Some(id) = s.get("id").and_then(Value::as_str) else { continue };
        let number = |k: &str| s.get(k).and_then(Value::as_f64);
        page.list.push(PublicServer {
            id: id.to_string(),
            playing: number("playing").unwrap_or(0.0) as u32,
            max: number("maxPlayers").unwrap_or(0.0) as u32,
            fps: number("fps").map(|f| f as f32),
            ping: number("ping").map(|p| p as u32),
            location: known.get(id).and_then(|i| i.get("location")).and_then(Value::as_str).map(String::from),
        });
    }
}

pub fn game_name(universe: Option<&str>) -> Option<String> {
    let data = fetch_json(&format!("https://games.roblox.com/v1/games?universeIds={}", universe?))?;
    Some(data.get("data")?.get(0)?.get("name")?.as_str()?.to_string())
}

/// "🇩🇪 Frankfurt am Main, Hesse, DE", looked up on ipinfo.io.
pub fn location(ip: Option<&str>) -> Option<String> {
    let data = fetch_json(&format!("https://ipinfo.io/{}/json", ip?)).unwrap_or(Value::Null);
    let mut parts: Vec<String> = Vec::new();
    for key in ["city", "region", "country"] {
        if let Some(part) = data.get(key).and_then(Value::as_str).filter(|p| !p.is_empty()) {
            if !parts.iter().any(|p| p == part) {
                parts.push(part.to_string());
            }
        }
    }
    let country = data.get("country").and_then(Value::as_str).unwrap_or("");
    let flag: String = country
        .chars()
        .filter(char::is_ascii_uppercase)
        .filter_map(|c| char::from_u32(0x1F1E6 + (c as u32 - 'A' as u32)))
        .collect();
    let text = [flag, parts.join(", ")].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then_some(text)
}

fn avatar_path(user_id: &str) -> PathBuf {
    paths::cache().join("avatars").join(format!("{user_id}.png"))
}

/// The user's headshot, cached for a day.
pub fn avatar(user_id: &str) -> Option<PathBuf> {
    let path = avatar_path(user_id);
    let fresh = path
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < Duration::from_secs(86400));
    if fresh {
        return Some(path);
    }
    let fetched = (|| -> Option<Vec<u8>> {
        let data = fetch_json(&format!(
            "https://thumbnails.roblox.com/v1/users/avatar-headshot?userIds={user_id}&size=150x150&format=Png&isCircular=false"))?;
        let url = data.get("data")?.get(0)?.get("imageUrl")?.as_str()?.to_string();
        let mut body = Vec::new();
        builds::agent(Duration::from_secs(6)).get(&url).call().ok()?.body_mut().as_reader().read_to_end(&mut body).ok()?;
        Some(body)
    })();
    match fetched {
        Some(body) => {
            fsutil::write_atomic(&path, &body).ok()?;
            Some(path)
        }
        None => path.exists().then_some(path),
    }
}

/// The headshot as fetched before, whatever its age (the window and every start
/// refresh it daily); fetched now only when there is none yet.
pub fn cached_avatar(user_id: &str) -> Option<PathBuf> {
    let path = avatar_path(user_id);
    if path.exists() { Some(path) } else { avatar(user_id) }
}

/// (the server the account is in now, or empty; the servers it joined in this
/// session), read from its newest log once.
pub fn current(account: &Account) -> (Map<String, Value>, Vec<Map<String, Value>>) {
    let mut follower = LogFollower::new(&account.logs());
    follower.poll();
    let server = if follower.server.contains_key("ip") { follower.server.clone() } else { Map::new() };
    (server, follower.history)
}

fn described_file() -> PathBuf {
    paths::runtime().join("servers.json")
}

/// The server with its game's name and its location, looked up once per job
/// and kept for the session (in $XDG_RUNTIME_DIR), so callers stay quick.
pub fn describe(server: &Map<String, Value>) -> Map<String, Value> {
    let Some(job) = server.get("job").and_then(Value::as_str) else { return server.clone() };
    let mut known = match fsutil::read_json(&described_file()) {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    };
    let has_ip = server.contains_key("ip");
    let info = match known.get(job) {
        Some(info) if !(has_ip && info.get("location").is_none_or(Value::is_null)) => info.clone(),
        _ => {
            let info = json!({
                "name": game_name(server.get("universe").and_then(Value::as_str)),
                "location": location(server.get("ip").and_then(Value::as_str)),
            });
            known.insert(job.to_string(), info.clone());
            let _ = fsutil::write_atomic(&described_file(), Value::Object(known).to_string().as_bytes());
            info
        }
    };
    let mut out = server.clone();
    if let Value::Object(info) = info {
        out.extend(info);
    }
    out
}

/// Read up to `limit` bytes from the end of a file.
pub fn read_tail(path: &Path, limit: u64) -> String {
    let Ok(mut file) = File::open(path) else { return String::new() };
    let size = file.metadata().map_or(0, |m| m.len());
    let _ = file.seek(SeekFrom::Start(size.saturating_sub(limit)));
    let mut bytes = Vec::new();
    let _ = file.read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::testenv::TempHome;
    use std::io::Write;

    pub const JOB: &str = "11111111-2222-4333-8444-555555555555";
    pub const PLACE: &str = "1818";

    pub fn stamp(seconds: f64) -> String {
        format!("2026-11-15T20:00:00.000Z,{seconds:.6},d5617800,6")
    }

    pub fn join_lines() -> Vec<String> {
        vec![
            format!("{} [FLog::Output] ! Joining game '{JOB}' place {PLACE} at 10.0.0.1", stamp(1.0)),
            format!("{} [FLog::Network] placeid:{PLACE}, userid:1, universeid:777", stamp(2.0)),
            format!("{} [FLog::Network] UDMUX Address = 128.116.1.2, Port = 61639 | RCC Server Address = x", stamp(3.0)),
        ]
    }

    #[test]
    fn join_then_leave() {
        let home = TempHome::new();
        let account = Account::new("main", home.make_prefix("m", None), "", false);
        let log = account.logs().join("0.747.0.7471481_20261115T200000Z_Player_abcde_last.log");
        std::fs::write(&log, join_lines().join("\n") + "\n").unwrap();
        let mut follower = LogFollower::new(&account.logs());
        let joined = follower.poll();
        assert_eq!(joined.len(), 1);
        assert_eq!(joined[0]["job"], JOB);
        assert_eq!(joined[0]["universe"], "777");
        assert_eq!(joined[0]["ip"], "128.116.1.2");
        assert!(joined[0]["link"].as_str().unwrap().contains(&format!("gameInstanceId={JOB}")));
        let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        writeln!(f, "{} [FLog::Output] {DISCONNECTED}", stamp(4.0)).unwrap(); // teleport noise
        follower.poll();
        assert!(!follower.server.is_empty());
        write!(f, "{} [FLog::Output] {}", stamp(60.0), &DISCONNECTED[..5]).unwrap(); // half a line
        follower.poll();
        assert!(!follower.server.is_empty());
        writeln!(f, "{}", &DISCONNECTED[5..]).unwrap();
        follower.poll();
        assert!(follower.server.is_empty());
        assert_eq!(follower.history.len(), 1);
    }

    #[test]
    fn a_page_of_public_servers() {
        let _home = crate::testenv::TempHome::new();
        let job = "9d997143-521a-440f-9fca-b6684348f0f8";
        fsutil::write_atomic(&described_file(), json!({job: {"name": "Obby", "location": "Paris, FR"}}).to_string().as_bytes()).unwrap();
        let data = json!({"previousPageCursor": null, "nextPageCursor": "abc==", "data": [
            {"id": job, "maxPlayers": 35, "playing": 34, "playerTokens": [], "players": [], "fps": 60.01, "ping": 315},
            {"id": "beb8f15d-baa9-4c52-9839-d470d0eefe58", "maxPlayers": 35, "playing": 3, "fps": 59.5},
        ]});
        let mut page = ServerPage::default();
        fill_page(&data, &mut page);
        assert_eq!(page.next.as_deref(), Some("abc=="));
        assert_eq!(page.list.len(), 2);
        assert_eq!((page.list[0].playing, page.list[0].max, page.list[0].ping), (34, 35, Some(315)));
        assert_eq!(page.list[0].location.as_deref(), Some("Paris, FR"));
        assert_eq!((page.list[1].ping, page.list[1].location.as_deref()), (None, None));
        assert!(public_page("1; rm", Sort::Most, None).error.contains("No game"));
    }
}

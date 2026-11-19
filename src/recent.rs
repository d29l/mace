//! The games your accounts played lately, newest first, for the Play buttons
//! in Mace Settings and the in-game bar, and `mace recent`. Each account's
//! supervisor notes the games it joins.

use std::collections::HashMap;
use std::fs;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::servers::{self, GameInfo};
use crate::{fsutil, paths};

/// How many games are remembered.
const KEEP: usize = 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Game {
    pub universe: String,
    /// The place the game starts in.
    pub place: String,
    pub name: String,
    /// The account that played it last.
    pub account: String,
    /// When that was (unix seconds).
    pub when: f64,
}

fn file() -> PathBuf {
    paths::data().join("recent.json")
}

/// When the list last changed, to notice changes cheaply.
pub fn changed() -> Option<std::time::SystemTime> {
    file().metadata().and_then(|m| m.modified()).ok()
}

pub fn load() -> Vec<Game> {
    match fsutil::read_json(&file()) {
        Some(Value::Array(list)) => list.into_iter().filter_map(|g| serde_json::from_value(g).ok()).collect(),
        _ => Vec::new(),
    }
}

/// Change the list, one process at a time (every running account's supervisor may write).
fn change(edit: impl FnOnce(&mut Vec<Game>)) -> anyhow::Result<()> {
    let dir = paths::data();
    fs::create_dir_all(&dir)?;
    let lock = fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(".recent.lock"))?;
    // SAFETY: flock on a descriptor this file owns; it's let go when the file closes.
    unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
    let mut games = load();
    edit(&mut games);
    games.sort_by(|a, b| b.when.total_cmp(&a.when));
    games.truncate(KEEP);
    fsutil::write_atomic(&file(), &serde_json::to_vec_pretty(&games)?)
}

/// What to remember of a server an account joined (from its log, maybe
/// described), with what Roblox says about the game; None when there's no
/// game to name.
fn entry(account_id: &str, server: &Map<String, Value>, info: Option<GameInfo>, known: &[Game]) -> Option<Game> {
    let text = |k: &str| server.get(k).and_then(Value::as_str).filter(|v| !v.is_empty());
    let universe = text("universe")?;
    let joined = text("place")?;
    let before = known.iter().find(|g| g.universe == universe);
    let info = info.unwrap_or_default();
    let name = Some(info.name).filter(|n| !n.is_empty())
        .or_else(|| text("name").map(String::from))
        .or_else(|| before.map(|g| g.name.clone()))?;
    Some(Game {
        universe: universe.to_string(),
        // a teleport lands in another of the game's places; Play starts it from the start
        place: info.root_place.or_else(|| before.map(|g| g.place.clone())).unwrap_or_else(|| joined.to_string()),
        name,
        account: account_id.to_string(),
        when: server.get("since").and_then(Value::as_f64).unwrap_or_else(servers::unix_time),
    })
}

fn merge(games: &mut Vec<Game>, game: Game) {
    games.retain(|g| g.universe != game.universe);
    games.push(game);
}

/// Note that the account joined this server's game.
pub fn record(account_id: &str, server: &Map<String, Value>) -> anyhow::Result<()> {
    let info = servers::game_info(server.get("universe").and_then(Value::as_str));
    let known = load();
    let Some(game) = entry(account_id, server, info, &known) else { return Ok(()) };
    change(|games| merge(games, game))
}

pub fn forget(universe: &str) -> anyhow::Result<()> {
    change(|games| games.retain(|g| g.universe != universe))
}

/// Icons asked for in this process, and when: one that couldn't be had isn't
/// asked for again right away (the window and the panel look every few seconds).
static ASKED: LazyLock<Mutex<HashMap<String, Instant>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// The game's icon, kept a week.
pub fn icon(universe: &str) -> Option<PathBuf> {
    let path = paths::cache().join("games").join(format!("{universe}.png"));
    let recently = ASKED.lock().unwrap().get(universe).is_some_and(|t| t.elapsed() < Duration::from_secs(600));
    if recently {
        return path.exists().then_some(path);
    }
    ASKED.lock().unwrap().insert(universe.to_string(), Instant::now());
    servers::cached_picture(&path, Duration::from_secs(7 * 86400), || servers::game_icon_url(universe, 150))
}

/// "just now", "5 min ago", "3 h ago", "yesterday", "4 days ago".
pub fn ago(when: f64, now: f64) -> String {
    let minutes = ((now - when).max(0.0) / 60.0) as u64;
    match minutes {
        0 => "just now".into(),
        1..60 => format!("{minutes} min ago"),
        60..1440 => format!("{} h ago", minutes / 60),
        1440..2880 => "yesterday".into(),
        _ => format!("{} days ago", minutes / 1440),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;
    use serde_json::json;

    fn server(universe: &str, place: &str, since: f64) -> Map<String, Value> {
        json!({ "universe": universe, "place": place, "job": "j", "since": since }).as_object().unwrap().clone()
    }

    fn info(name: &str, root: &str) -> Option<GameInfo> {
        Some(GameInfo { name: name.into(), creator: None, root_place: Some(root.into()) })
    }

    #[test]
    fn a_game_is_kept_once_newest_first_from_its_start() {
        let _home = TempHome::new();
        for (account, universe, place, since, name, root) in [
            ("main", "1", "10", 100.0, "Bee Swarm", "10"),
            ("alt", "2", "20", 200.0, "Flex Your FPS", "20"),
            // a teleport into another place of the first game, later and as another account
            ("alt", "1", "11", 300.0, "Bee Swarm", "10"),
        ] {
            let game = entry(account, &server(universe, place, since), info(name, root), &load()).unwrap();
            change(|games| merge(games, game)).unwrap();
        }
        let games = load();
        assert_eq!(games.iter().map(|g| (g.universe.as_str(), g.account.as_str())).collect::<Vec<_>>(),
                   [("1", "alt"), ("2", "alt")]);
        assert_eq!((games[0].place.as_str(), games[0].when), ("10", 300.0));
        forget("1").unwrap();
        assert_eq!(load().len(), 1);
    }

    #[test]
    fn without_roblox_the_name_comes_from_what_is_known() {
        let _home = TempHome::new();
        // no name anywhere: nothing to show
        assert!(entry("main", &server("5", "50", 1.0), None, &[]).is_none());
        let mut described = server("5", "51", 2.0);
        described.insert("name".into(), json!("Doors"));
        let first = entry("main", &described, None, &[]).unwrap();
        assert_eq!((first.name.as_str(), first.place.as_str()), ("Doors", "51"));
        // later, offline: the name and start place it had
        let again = entry("alt", &server("5", "52", 3.0), None, std::slice::from_ref(&first)).unwrap();
        assert_eq!((again.name.as_str(), again.place.as_str(), again.account.as_str()), ("Doors", "51", "alt"));
        // a server from a log without the game's id isn't a game
        assert!(entry("main", &json!({ "place": "9" }).as_object().unwrap().clone(), info("X", "9"), &[]).is_none());
    }

    #[test]
    fn only_the_newest_are_kept() {
        let _home = TempHome::new();
        for i in 0..(KEEP + 5) {
            let game = entry("main", &server(&i.to_string(), "1", i as f64), info("G", "1"), &[]).unwrap();
            change(|games| merge(games, game)).unwrap();
        }
        let games = load();
        assert_eq!(games.len(), KEEP);
        assert_eq!(games[0].universe, (KEEP + 4).to_string());
    }

    #[test]
    fn times_read_plainly() {
        assert_eq!(ago(100.0, 110.0), "just now");
        assert_eq!(ago(0.0, 5.0 * 60.0), "5 min ago");
        assert_eq!(ago(0.0, 3.0 * 3600.0 + 50.0), "3 h ago");
        assert_eq!(ago(0.0, 30.0 * 3600.0), "yesterday");
        assert_eq!(ago(0.0, 4.0 * 86400.0), "4 days ago");
        assert_eq!(ago(10.0, 0.0), "just now");
    }
}

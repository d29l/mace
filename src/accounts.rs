//! The accounts Mace runs. Each one is a Rosemary prefix (its login, settings
//! and logs live there) plus Mace's own copy of the Roblox build it is on.
//!
//! Every account is equal; "main" is only the id of the first one. Prefixes are
//! referenced by path, so an existing one (Nutmeg's, say) is used where it is.
//! Mace deletes only prefixes it made itself.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail};
use serde_json::{Map, Value, json};

use crate::{fsutil, paths, runner, settings};

pub const MAIN: &str = "main";

#[derive(Clone, Debug, PartialEq)]
pub struct Account {
    pub id: String,
    pub prefix: PathBuf,
    pub label: String,
    /// Mace made the prefix, so removing the account may delete it.
    pub owned: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Identity {
    pub id: String,
    pub username: String,
    pub display: String,
}

impl Identity {
    pub fn is_empty(&self) -> bool {
        self.id.is_empty()
    }
}

fn as_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

impl Account {
    pub fn new(id: &str, prefix: impl Into<PathBuf>, label: &str, owned: bool) -> Account {
        Account { id: id.into(), prefix: prefix.into(), label: label.into(), owned }
    }

    pub fn is_main(&self) -> bool {
        self.id == MAIN
    }

    /// Mace's folder for this account: its Roblox copy and build record.
    pub fn folder(&self) -> PathBuf {
        paths::accounts_dir().join(&self.id)
    }

    pub fn bundle(&self) -> PathBuf {
        self.folder().join("Roblox.app")
    }

    pub fn home(&self) -> PathBuf {
        self.prefix.join("root/Users").join(paths::user())
    }

    pub fn logs(&self) -> PathBuf {
        self.home().join("Library/Logs/Roblox")
    }

    pub fn app_storage(&self) -> PathBuf {
        self.home().join("Library/Roblox/LocalStorage/appStorage.json")
    }

    pub fn read_storage(&self) -> Map<String, Value> {
        match fsutil::read_json(&self.app_storage()) {
            Some(Value::Object(map)) => map,
            _ => Map::new(),
        }
    }

    /// UserId, Username and DisplayName of the signed-in user, from the client's
    /// own storage; empty when nobody has signed in. Cookies are never read.
    pub fn identity(&self) -> Identity {
        let data = self.read_storage();
        let id = as_text(data.get("UserId"));
        if id.is_empty() || id == "0" || !id.chars().all(|c| c.is_ascii_digit()) {
            return Identity::default();
        }
        Identity { id, username: as_text(data.get("Username")), display: as_text(data.get("DisplayName")) }
    }

    pub fn name_with(&self, who: &Identity) -> String {
        for candidate in [&self.label, &who.display, &who.username] {
            if !candidate.is_empty() {
                return candidate.clone();
            }
        }
        if self.is_main() { "Main account".into() } else { "New account".into() }
    }

    pub fn name(&self) -> String {
        self.name_with(&self.identity())
    }
}

fn load() -> Map<String, Value> {
    let mut data = match fsutil::read_json(&paths::accounts_file()) {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    };
    data.entry("accounts").or_insert_with(|| json!([]));
    data.entry("last_used").or_insert(Value::Null);
    data
}

fn save(data: &Map<String, Value>) -> anyhow::Result<()> {
    fsutil::write_json(&paths::accounts_file(), &Value::Object(data.clone()))
}

fn entry(account: &Account) -> Value {
    json!({"id": account.id, "prefix": account.prefix, "label": account.label, "owned": account.owned})
}

fn entries(data: &Map<String, Value>) -> Vec<Value> {
    data.get("accounts").and_then(Value::as_array).cloned().unwrap_or_default()
}

pub fn accounts() -> Vec<Account> {
    entries(&load())
        .iter()
        .filter_map(|e| {
            Some(Account::new(
                e.get("id")?.as_str()?,
                e.get("prefix")?.as_str()?,
                e.get("label").and_then(Value::as_str).unwrap_or(""),
                e.get("owned").and_then(Value::as_bool).unwrap_or(true),
            ))
        })
        .collect()
}

pub fn get(account_id: &str) -> anyhow::Result<Account> {
    accounts().into_iter().find(|a| a.id == account_id).ok_or_else(|| anyhow!("no account '{account_id}'"))
}

/// An account by id, label, username or display name, ignoring case.
pub fn find(query: &str) -> anyhow::Result<Account> {
    let q = query.to_lowercase();
    let q = q.trim_start_matches('@');
    for account in accounts() {
        let who = account.identity();
        let names = [&account.id, &account.label, &who.username, &who.display];
        if names.iter().any(|n| n.to_lowercase() == q) {
            return Ok(account);
        }
    }
    Err(anyhow!("no account called '{query}'"))
}

/// Add an account whose prefix already exists (moving over, or adopting one).
pub fn register(account: Account) -> anyhow::Result<Account> {
    let mut data = load();
    let mut list = entries(&data);
    if list.iter().any(|e| e.get("id").and_then(Value::as_str) == Some(&account.id)) {
        bail!("there already is an account '{}'", account.id);
    }
    list.push(entry(&account));
    data.insert("accounts".into(), Value::Array(list));
    if data.get("last_used").is_none_or(Value::is_null) {
        data.insert("last_used".into(), Value::String(account.id.clone()));
    }
    save(&data)?;
    Ok(account)
}

fn free_id(data: &Map<String, Value>) -> String {
    let taken: Vec<String> =
        entries(data).iter().filter_map(|e| e.get("id")?.as_str().map(String::from)).collect();
    if !taken.iter().any(|t| t == MAIN) {
        return MAIN.into();
    }
    (1..).map(|n| format!("alt{n}")).find(|id| !taken.contains(id)).unwrap()
}

/// A new account with a fresh prefix, set up from Rosemary's cached Darwin base.
pub fn add(label: &str) -> anyhow::Result<Account> {
    let mut data = load();
    let id = free_id(&data);
    let account = Account::new(&id, paths::accounts_dir().join(&id).join("prefix"), label.trim(), true);
    std::fs::create_dir_all(account.folder())?;
    let program = runner::find(&settings::load().rosemary)?;
    let output = Command::new(&program)
        .args(["prefix", "create"])
        .arg(&account.prefix)
        .args(["--arch", "x86_64"])
        .env("ROSEMARY_PREFIX_SETUP", "download")
        .output()?;
    if !output.status.success() {
        let _ = std::fs::remove_dir_all(account.folder());
        let err = String::from_utf8_lossy(&output.stderr);
        let tail: String = err.chars().rev().take(800).collect::<Vec<_>>().into_iter().rev().collect();
        bail!("Rosemary could not create the prefix:\n{tail}");
    }
    // start from the first account's graphics, volume and frame-rate settings
    if let Some(first) = accounts().first() {
        let source = first.home().join("Library/Roblox/GlobalBasicSettings_13.xml");
        if source.exists() {
            let target = account.home().join("Library/Roblox");
            std::fs::create_dir_all(&target)?;
            std::fs::copy(&source, target.join("GlobalBasicSettings_13.xml"))?;
        }
    }
    let mut list = entries(&data);
    list.push(entry(&account));
    data.insert("accounts".into(), Value::Array(list));
    if data.get("last_used").is_none_or(Value::is_null) {
        data.insert("last_used".into(), Value::String(id));
    }
    save(&data)?;
    Ok(account)
}

pub fn rename(account: &Account, label: &str) -> anyhow::Result<()> {
    let mut data = load();
    let list: Vec<Value> = entries(&data)
        .into_iter()
        .map(|mut e| {
            if e.get("id").and_then(Value::as_str) == Some(&account.id) {
                e["label"] = Value::String(label.trim().to_string());
            }
            e
        })
        .collect();
    data.insert("accounts".into(), Value::Array(list));
    save(&data)
}

fn inside(path: &Path, dir: &Path) -> bool {
    path.starts_with(dir) && path != dir
}

/// Drop the account from the list and remove Mace's folder for it (its Roblox
/// copy), and its prefix too when Mace made it. Callers stop it first.
pub fn forget(account: &Account) -> anyhow::Result<()> {
    let mut data = load();
    let list: Vec<Value> =
        entries(&data).into_iter().filter(|e| e.get("id").and_then(Value::as_str) != Some(&account.id)).collect();
    if data.get("last_used").and_then(Value::as_str) == Some(&account.id) {
        let first = list.first().and_then(|e| e.get("id")).cloned().unwrap_or(Value::Null);
        data.insert("last_used".into(), first);
    }
    data.insert("accounts".into(), Value::Array(list));
    save(&data)?;
    if account.owned && inside(&account.prefix, &account.folder()) {
        let _ = std::fs::remove_dir_all(&account.prefix);
    }
    if inside(&account.folder(), &paths::accounts_dir()) {
        let _ = std::fs::remove_dir_all(account.folder());
    }
    let mut s = settings::load();
    if s.accounts.remove(&account.id).is_some() {
        settings::save(&s)?;
    }
    Ok(())
}

pub fn last_used() -> Option<Account> {
    let data = load();
    let all = accounts();
    let last = data.get("last_used").and_then(Value::as_str).unwrap_or("");
    all.iter().find(|a| a.id == last).cloned().or_else(|| all.first().cloned())
}

pub fn set_last_used(account: &Account) -> anyhow::Result<()> {
    let mut data = load();
    if data.get("last_used").and_then(Value::as_str) != Some(&account.id) {
        data.insert("last_used".into(), Value::String(account.id.clone()));
        save(&data)?;
    }
    Ok(())
}

/// The account roblox.com's links belong to: the one whose user the browser was
/// last seen signed in as (learned from the joins of browser launches).
pub fn browser_account() -> Option<Account> {
    let user = as_text(load().get("browser_user"));
    if user.is_empty() {
        return None;
    }
    accounts().into_iter().find(|a| a.identity().id == user)
}

pub fn set_browser_user(user_id: &str) -> anyhow::Result<()> {
    let mut data = load();
    if as_text(data.get("browser_user")) != user_id {
        data.insert("browser_user".into(), Value::String(user_id.to_string()));
        save(&data)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::TempHome;

    #[test]
    fn register_find_forget() {
        let home = TempHome::new();
        let kept = home.make_prefix("kept", None);
        register(Account::new("main", &kept, "Me", false)).unwrap();
        register(Account::new("alt1", home.make_prefix("x", None), "Alt", false)).unwrap();
        assert_eq!(find("alt").unwrap().id, "alt1");
        assert_eq!(last_used().unwrap().id, "main");
        set_last_used(&get("alt1").unwrap()).unwrap();
        assert_eq!(last_used().unwrap().id, "alt1");
        forget(&get("main").unwrap()).unwrap();
        assert!(kept.exists()); // not Mace's prefix: never deleted
        assert_eq!(accounts().iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["alt1"]);
    }

    #[test]
    fn identity_and_browser_account() {
        let home = TempHome::new();
        let p = home.make_prefix("m", Some(json!({"UserId": 87, "Username": "someone", "DisplayName": "Some"})));
        register(Account::new("main", &p, "", false)).unwrap();
        let account = get("main").unwrap();
        assert_eq!(account.identity(), Identity { id: "87".into(), username: "someone".into(), display: "Some".into() });
        assert_eq!(account.name(), "Some");
        assert!(browser_account().is_none());
        set_browser_user("87").unwrap();
        assert_eq!(browser_account().unwrap().id, "main");
    }
}

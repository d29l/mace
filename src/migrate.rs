//! Moving over from Nutmeg: the accounts are used where they are, and Nutmeg's
//! settings become Mace's. Nothing is moved or deleted, so Nutmeg keeps working
//! as before.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::accounts::{self, Account};
use crate::{fsutil, paths, settings};

static RUNS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?m)^\s*R="?([^"\n]+)"?\s*$"#).unwrap());
static TASKSET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"taskset\s+-c\s+([0-9,-]+)").unwrap());

/// Nutmeg's settings and data folder, host install first, then the Flatpak's.
fn nutmeg() -> Option<(Value, PathBuf)> {
    let flatpak = paths::home().join(".var/app/io.github.frolleks.Nutmeg");
    [(paths::config_home().join("nutmeg/config.json"), paths::data_home().join("nutmeg")),
     (flatpak.join("config/nutmeg/config.json"), flatpak.join("data/nutmeg"))]
        .into_iter()
        .find_map(|(config, data)| Some((fsutil::read_json(&config)?, data)))
}

/// A Rosemary path that is a small shell wrapper (pinning cores, say) is
/// replaced by the Rosemary it runs, with its taskset cores kept.
fn unwrap_rosemary(path: &str) -> (String, String) {
    let program = match path.strip_prefix("~/") {
        Some(rest) => paths::home().join(rest),
        None => PathBuf::from(path),
    };
    let small = program.metadata().is_ok_and(|m| m.len() <= 64 << 10);
    let Some(text) = small.then(|| std::fs::read_to_string(&program).ok()).flatten() else {
        return (path.into(), String::new());
    };
    if !text.starts_with("#!") {
        return (path.into(), String::new());
    }
    let Some(target) = RUNS.captures(&text) else { return (path.into(), String::new()) };
    let real = target[1].replace("$HOME", &paths::home().to_string_lossy());
    let cores = TASKSET.captures(&text).map(|m| m[1].to_string()).unwrap_or_default();
    (if Path::new(&real).exists() { real } else { path.into() }, cores)
}

fn resolved(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Register the existing accounts and import settings, once. Returns what was done.
pub fn run() -> anyhow::Result<Vec<String>> {
    let mut done = Vec::new();
    let found = nutmeg();
    if !settings::exists() {
        match &found {
            Some((config, _)) => {
                let mut s = settings::from_nutmeg(config);
                if s.rosemary != "auto" {
                    let (rosemary, cores) = unwrap_rosemary(&s.rosemary);
                    s.rosemary = rosemary;
                    if !cores.is_empty() {
                        s.cpu_cores = cores;
                    }
                }
                settings::save(&s)?;
                done.push("imported Nutmeg's settings".into());
            }
            None => {
                settings::save(&settings::load())?;
                done.push("wrote default settings".into());
            }
        }
    }
    if !accounts::accounts().is_empty() {
        done.push("accounts already set up; left as they are".into());
        return Ok(done);
    }
    if let Some((config, data)) = &found {
        let chosen = config.get("prefix_path").or_else(|| config.get("prefix")).and_then(Value::as_str).unwrap_or("");
        let prefix = resolved(&if chosen.is_empty() { data.join("prefix") } else { PathBuf::from(chosen) });
        if prefix.join("root").is_dir() {
            accounts::register(Account::new(accounts::MAIN, &prefix, "", false))?;
            done.push(format!("main account: Nutmeg's prefix {}", prefix.display()));
        }
    }
    Ok(done)
}

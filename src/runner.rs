//! Finding Rosemary: the runner chosen in the settings, else the newest one
//! Butter installed, else one on PATH.

use std::path::PathBuf;

use anyhow::bail;

use crate::{fsutil, paths};

#[derive(Clone, Debug)]
pub struct Runner {
    pub name: String,
    pub path: PathBuf,
}

fn butter_dirs() -> [PathBuf; 2] {
    [paths::data_home().join("butter/runners"), paths::home().join(".var/app/io.github.microlift.butter/data/butter/runners")]
}

/// `key = "value"` lines of a runner.toml.
fn fields(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            if key.is_empty() || !key.chars().all(|c| c.is_alphanumeric() || c == '_') {
                return None;
            }
            Some((key.to_string(), value.trim().trim_matches('"').to_string()))
        })
        .collect()
}

/// Butter's installed runners, newest install first, named as Butter names them.
pub fn butter_runners() -> Vec<Runner> {
    let mut found = Vec::new();
    for base in butter_dirs() {
        let Ok(entries) = std::fs::read_dir(&base) else { continue };
        for entry in entries.flatten() {
            let manifest = entry.path().join("runner.toml");
            let Ok(text) = std::fs::read_to_string(&manifest) else { continue };
            let fields = fields(&text);
            let get = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
            let program = entry.path().join(get("executable").unwrap_or_else(|| "bin/rosemary".into()));
            if !fsutil::is_executable(&program) {
                continue;
            }
            let installed = get("installed").and_then(|v| v.parse::<u64>().ok()).unwrap_or_else(|| {
                manifest
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs())
            });
            let name = get("name").unwrap_or_else(|| entry.file_name().to_string_lossy().into_owned());
            found.push((installed, Runner { name, path: program }));
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().map(|(_, r)| r).collect()
}

pub fn find(choice: &str) -> anyhow::Result<PathBuf> {
    if !choice.is_empty() && choice != "auto" {
        let path = match choice.strip_prefix("~/") {
            Some(rest) => paths::home().join(rest),
            None => PathBuf::from(choice),
        };
        if fsutil::is_executable(&path) {
            return Ok(path);
        }
        bail!("the chosen Rosemary is missing: {}", path.display());
    }
    if let Some(runner) = butter_runners().into_iter().next() {
        return Ok(runner.path);
    }
    if let Some(path) = fsutil::which("rosemary") {
        return Ok(path);
    }
    bail!("no Rosemary found: install a runner in Butter")
}

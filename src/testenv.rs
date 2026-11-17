//! A throwaway XDG home for tests, so nothing touches real settings. Tests that
//! use it run one at a time: environment variables belong to the whole process.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde_json::Value;

static LOCK: Mutex<()> = Mutex::new(());

pub struct TempHome {
    pub root: tempfile::TempDir,
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    _guard: MutexGuard<'static, ()>,
}

impl TempHome {
    pub fn new() -> TempHome {
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        let mut saved = Vec::new();
        for (var, sub) in [("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"), ("XDG_CACHE_HOME", "cache"),
                           ("XDG_RUNTIME_DIR", "run")] {
            saved.push((var, std::env::var_os(var)));
            std::fs::create_dir_all(root.path().join(sub)).unwrap();
            // SAFETY: tests that touch the environment hold LOCK.
            unsafe { std::env::set_var(var, root.path().join(sub)) };
        }
        TempHome { root, saved, _guard: guard }
    }

    pub fn path(&self) -> &Path {
        self.root.path()
    }

    /// A prefix with the folders Roblox would make, and an appStorage.json.
    pub fn make_prefix(&self, name: &str, storage: Option<Value>) -> PathBuf {
        let prefix = self.path().join("prefixes").join(name);
        let home = prefix.join("root/Users").join(crate::paths::user());
        std::fs::create_dir_all(home.join("Library/Roblox/LocalStorage")).unwrap();
        std::fs::create_dir_all(home.join("Library/Logs/Roblox")).unwrap();
        if let Some(storage) = storage {
            std::fs::write(home.join("Library/Roblox/LocalStorage/appStorage.json"), storage.to_string()).unwrap();
        }
        prefix
    }

    pub fn fake_runner(&self, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let program = self.path().join("rosemary");
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        program
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        for (var, value) in &self.saved {
            // SAFETY: still holding LOCK.
            unsafe {
                match value {
                    Some(v) => std::env::set_var(var, v),
                    None => std::env::remove_var(var),
                }
            }
        }
    }
}

//! Where Mace keeps things. Every path follows the XDG variables, so tests can
//! point a whole run at a scratch folder by setting them.

use std::env;
use std::path::PathBuf;

/// The login name, as the prefixes' `root/Users/<name>` folders use it.
pub fn user() -> String {
    for var in ["LOGNAME", "USER", "LNAME", "USERNAME"] {
        if let Ok(name) = env::var(var) {
            if !name.is_empty() {
                return name;
            }
        }
    }
    // SAFETY: getpwuid returns a pointer into static storage, or null.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if !pw.is_null() {
            let name = std::ffi::CStr::from_ptr((*pw).pw_name);
            return name.to_string_lossy().into_owned();
        }
    }
    String::from("user")
}

pub fn home() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn xdg(variable: &str, default: &str) -> PathBuf {
    match env::var_os(variable).map(PathBuf::from) {
        Some(path) if path.is_absolute() => path,
        _ => home().join(default),
    }
}

pub fn config_home() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
}

/// `$XDG_DATA_HOME` itself: menu entries, icons, other apps' data.
pub fn data_home() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share")
}

pub fn config() -> PathBuf {
    config_home().join("mace")
}

pub fn data() -> PathBuf {
    data_home().join("mace")
}

pub fn cache() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache").join("mace")
}

/// $XDG_RUNTIME_DIR itself, where Rosemary keeps its process servers too.
pub fn runtime_root() -> PathBuf {
    match env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        Some(path) if path.is_absolute() => path,
        // SAFETY: getuid can't fail.
        _ => PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })),
    }
}

pub fn runtime() -> PathBuf {
    match env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        Some(path) if path.is_absolute() => path.join("mace"),
        _ => cache(),
    }
}

pub fn settings_file() -> PathBuf {
    config().join("settings.json")
}

pub fn accounts_file() -> PathBuf {
    data().join("accounts.json")
}

/// One unpacked Roblox.app per build, shared by every account on it.
pub fn builds() -> PathBuf {
    data().join("builds")
}

pub fn accounts_dir() -> PathBuf {
    data().join("accounts")
}

pub fn logs() -> PathBuf {
    data().join("logs")
}

pub fn downloads() -> PathBuf {
    cache().join("downloads")
}

/// Mace's own files (the icon, the panel's library): installed beside
/// its data, or the source tree for a development build.
pub fn app() -> PathBuf {
    if let Some(dir) = env::var_os("MACE_APP_DIR") {
        return PathBuf::from(dir);
    }
    let installed = data().join("app");
    if installed.join("data").is_dir() {
        return installed;
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub fn logo() -> PathBuf {
    app().join("data/mace.svg")
}

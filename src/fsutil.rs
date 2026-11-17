//! Small file helpers shared by every module.

use std::io::Write;
use std::path::Path;

use serde_json::Value;

/// Replace `path` whole: write a temporary file beside it, then rename. Each
/// write has its own temporary file, so writers on other threads or processes
/// never take each other's.
pub fn write_atomic(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut tmp = tempfile::Builder::new().prefix(&format!(".{name}.")).tempfile_in(dir)?;
    tmp.write_all(contents)?;
    tmp.persist(path)?;
    Ok(())
}

pub fn write_json(path: &Path, value: &Value) -> anyhow::Result<()> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    write_atomic(path, text.as_bytes())
}

/// A JSON file's value, or None when it's missing or broken.
pub fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// A program on PATH.
pub fn which(program: &str) -> Option<std::path::PathBuf> {
    if program.contains('/') {
        let path = std::path::PathBuf::from(program);
        return is_executable(&path).then_some(path);
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).map(|dir| dir.join(program)).find(|p| is_executable(p))
    })
}

pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

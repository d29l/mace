//! Turning what a person pastes or clicks into a URL the Roblox client joins.

use std::sync::LazyLock;

use anyhow::bail;
use regex::Regex;

static PLACE_AND_JOB: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d+)(?:[\s/:]+([0-9a-fA-F-]{36}))?$").unwrap());
static GAMES_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/games/(\d+)").unwrap());

/// Percent-encoding for a query value (everything but unreserved characters).
pub fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

pub fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&text[i + 1..i + 3], 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 3;
                    continue;
                }
                Err(_) => out.push(b'%'),
            },
            b'+' => out.push(b' '),
            other => out.push(other),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A URL's path and its query, the query's names lowercased.
fn split_url(link: &str) -> (String, Vec<(String, String)>) {
    let without_scheme = link.split_once("://").map_or(link, |(_, rest)| rest);
    let without_fragment = without_scheme.split('#').next().unwrap_or("");
    let (location, query) = without_fragment.split_once('?').unwrap_or((without_fragment, ""));
    let path = location.find('/').map_or(String::new(), |at| location[at..].to_string());
    let pairs = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(k).to_lowercase(), decode(v))
        })
        .collect();
    (path, pairs)
}

/// A roblox: URL the client joins from: a join link, a Roblox game, share or
/// start link, a roblox:// or roblox-player: link, or "PLACEID [JOBID]".
pub fn join_url(link: &str) -> anyhow::Result<String> {
    let link = link.trim();
    if link.starts_with("roblox:") || link.starts_with("roblox-player:") {
        return Ok(link.to_string());
    }
    if let Some(m) = PLACE_AND_JOB.captures(link) {
        return Ok(start_url(&m[1], m.get(2).map(|j| j.as_str())));
    }
    let (path, query) = split_url(link);
    let get = |key: &str| query.iter().find(|(k, v)| k == key && !v.is_empty()).map(|(_, v)| v.clone());
    if path.trim_end_matches('/').ends_with("/share") {
        if let Some(code) = get("code") {
            let kind = get("type").unwrap_or_else(|| "Server".into());
            return Ok(format!("roblox://navigation/share_links?code={}&type={}", encode(&code), encode(&kind)));
        }
    }
    if let Some(place) = get("placeid") {
        return Ok(start_url(&place, get("gameinstanceid").or_else(|| get("jobid")).as_deref()));
    }
    if let Some(m) = GAMES_PATH.captures(&path) {
        return Ok(start_url(&m[1], None));
    }
    bail!("not a Roblox game, server or share link")
}

pub fn start_url(place: &str, job: Option<&str>) -> String {
    match job {
        Some(job) => format!("roblox://experiences/start?placeId={}&gameInstanceId={}", encode(place), encode(job)),
        None => format!("roblox://experiences/start?placeId={}", encode(place)),
    }
}

/// A link anyone can open in a browser to join this server.
pub fn share_link(place: &str, job: &str) -> String {
    format!("https://www.roblox.com/games/start?placeId={place}&gameInstanceId={job}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms() {
        let job = "0a1b2c3d-0000-4000-8000-123456789abc";
        assert_eq!(join_url(&format!("123 {job}")).unwrap(),
                   format!("roblox://experiences/start?placeId=123&gameInstanceId={job}"));
        assert_eq!(join_url(&format!("https://www.roblox.com/games/start?placeId=5&gameInstanceId={job}")).unwrap(),
                   format!("roblox://experiences/start?placeId=5&gameInstanceId={job}"));
        assert_eq!(join_url("https://www.roblox.com/games/42/Some-Game").unwrap(),
                   "roblox://experiences/start?placeId=42");
        assert!(join_url("https://www.roblox.com/share?code=abc&type=Server").unwrap()
            .starts_with("roblox://navigation/share_links?code=abc"));
        assert_eq!(join_url("roblox-player:1+launchmode:play").unwrap(), "roblox-player:1+launchmode:play");
        assert!(join_url("hello").is_err());
    }
}

//! What the in-game panel and Mace say to each other: one JSON object per line
//! over the account's panel socket. The panel (drawn inside Roblox) only draws
//! and reports clicks; everything it shows comes from Mace, and everything it
//! does is a request to Mace.

use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;

/// From the panel to Mace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToMace {
    /// First thing on a new connection: who is drawing, and on what.
    Hello { v: u32, pid: u32, video: String, swapchain: String },
    /// Send the state again (the panel asks while it's open).
    State,
    /// A page of the game's public servers.
    Servers { place: String, sort: Sort, cursor: Option<String> },
    /// A `mace` command, answered with `Done`.
    Run { id: u64, args: Vec<String> },
    /// The panel opened or closed (for the log, and for `mace panel`).
    Event { what: String },
    /// From `mace panel`: open or close the panels on this socket.
    Toggle,
    /// From another `mace` process: show this in the game. Answered with
    /// `Done { id: 0, ok }`, ok when a panel was there to show it.
    Toast { title: String, body: String },
}

/// From Mace to the panel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToPanel {
    Config(Config),
    State(State),
    Servers(ServerPage),
    /// An account's picture, square, RGBA, base64.
    Avatar { user: String, size: u32, rgba: String },
    Done { id: u64, ok: bool, error: String },
    /// Open or close the panel (`mace panel toggle`).
    Toggle,
    /// A note shown in the game for a few seconds, panel open or not.
    Toast { title: String, body: String },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// The account this Roblox is.
    pub me: String,
    pub key: Key,
    pub theme: Theme,
    /// Whether "switch to this account's window" can work here.
    pub can_focus: bool,
    /// Blur the game behind the open panel (otherwise it's only dimmed).
    #[serde(default)]
    pub blur: bool,
    /// Frames per second while the window isn't focused; 0 = no limit (Mace
    /// sends 0 to the account you used last).
    #[serde(default)]
    pub background_fps: u32,
    /// Nudge the game when it has been idle a while, so Roblox doesn't kick it.
    #[serde(default)]
    pub anti_afk: bool,
}

/// A key and its modifiers, by physical position (SDL scancodes).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Key {
    pub scancode: u32,
    pub mods: u8,
}

pub const MOD_SHIFT: u8 = 1;
pub const MOD_CTRL: u8 = 2;
pub const MOD_ALT: u8 = 4;
pub const MOD_SUPER: u8 = 8;

/// Colours as `#rrggbb`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Theme {
    pub background: String,
    pub surface: String,
    pub surface_hover: String,
    pub border: String,
    pub text: String,
    pub subtext: String,
    pub accent: String,
    pub on_accent: String,
    pub danger: String,
}

impl Default for Theme {
    fn default() -> Self {
        let c = |s: &str| s.to_string();
        Theme {
            background: c("#242426"), surface: c("#2f2f32"), surface_hover: c("#3a3a3e"), border: c("#3f3f44"),
            text: c("#ededef"), subtext: c("#a1a1a8"), accent: c("#d8d8dc"), on_accent: c("#1c1c1e"),
            danger: c("#e66b6b"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// Mace's clock (unix seconds), for "12 min here".
    pub now: f64,
    pub accounts: Vec<Account>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub name: String,
    pub username: String,
    /// The Roblox user id, whose picture comes as an `Avatar` message.
    pub user: Option<String>,
    pub fps: Option<u32>,
    /// The supervisor's state while starting: preparing, downloading, starting, updating, failed.
    pub launch: Option<String>,
    pub running: bool,
    pub muted: Option<bool>,
    pub server: Option<Server>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Server {
    pub place: String,
    pub job: String,
    pub link: String,
    pub name: Option<String>,
    pub location: Option<String>,
    /// When this account joined (unix seconds).
    pub since: Option<f64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    /// Fullest first (Roblox's own order).
    #[default]
    Most,
    Fewest,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServerPage {
    pub place: String,
    pub sort: Sort,
    /// The cursor this page was asked for (None: the first page).
    pub cursor: Option<String>,
    pub next: Option<String>,
    pub list: Vec<PublicServer>,
    /// Why there's nothing (offline, Roblox said slow down, ...); empty when fine.
    pub error: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PublicServer {
    pub id: String,
    pub playing: u32,
    pub max: u32,
    pub fps: Option<f32>,
    /// The average ping of the players in it, not yours.
    pub ping: Option<u32>,
    /// Known only for servers one of your accounts has been in.
    pub location: Option<String>,
}

impl ToMace {
    pub fn line(&self) -> String {
        let mut text = serde_json::to_string(self).expect("plain data");
        text.push('\n');
        text
    }
}

impl ToPanel {
    pub fn line(&self) -> String {
        let mut text = serde_json::to_string(self).expect("plain data");
        text.push('\n');
        text
    }
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn unbase64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut n, mut bits) = (0u32, 0);
    for c in text.bytes() {
        if c == b'=' {
            break;
        }
        let value = ALPHABET.iter().position(|&a| a == c)? as u32;
        n = n << 6 | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((n >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        for data in [&b""[..], b"a", b"ab", b"abc", b"abcd", &[0, 255, 7, 128, 9]] {
            assert_eq!(unbase64(&base64(data)).unwrap(), data);
        }
        assert_eq!(base64(b"Mace"), "TWFjZQ==");
    }

    #[test]
    fn messages_are_tagged_lines() {
        let line = ToMace::Run { id: 3, args: vec!["rejoin".into(), "main".into()] }.line();
        assert_eq!(line, "{\"type\":\"run\",\"id\":3,\"args\":[\"rejoin\",\"main\"]}\n");
        let back: ToPanel = serde_json::from_str(&ToPanel::Config(Config::default()).line()).unwrap();
        assert_eq!(back, ToPanel::Config(Config::default()));
        assert_eq!(serde_json::from_str::<ToMace>("{\"type\":\"state\"}").unwrap(), ToMace::State);
    }
}

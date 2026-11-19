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
    /// Something done to a plugin's menu: `open` and `close` (the drop-down
    /// of the plugin's `button`), `click`, `change` (a toggle: `value` is the
    /// new bool) and `submit` (an input: `value` is the text), for the control
    /// whose `key` it is.
    Plugin {
        plugin: String,
        button: String,
        #[serde(default)]
        key: String,
        what: String,
        #[serde(default)]
        value: Option<serde_json::Value>,
    },
}

/// From Mace to the panel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToPanel {
    Config(Config),
    State(State),
    Servers(ServerPage),
    /// A picture, square, RGBA, base64: an account's (`user` is its Roblox user
    /// id), a game's icon (`user` is "game:" and the game's universe id) or a
    /// plugin's picture (`user` is "plugin:ID/FILE").
    Avatar { user: String, size: u32, rgba: String },
    Done { id: u64, ok: bool, error: String },
    /// Open or close the panel (`mace panel toggle`).
    Toggle,
    /// A note shown in the game for a few seconds, panel open or not.
    Toast { title: String, body: String },
    /// The plugins' bar buttons, all of them each time (none: an empty list).
    Plugins { buttons: Vec<PluginButton> },
    /// What a plugin's drop-down shows, sent when it opens and again whenever it changes.
    PluginMenu { plugin: String, button: String, menu: Menu },
    /// Put this text on the clipboard (a plugin asked to).
    Copy { text: String },
    /// Close the panel if it's open (a plugin asked to).
    Close,
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
    /// Games your accounts played lately, newest first.
    #[serde(default)]
    pub games: Vec<Game>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Game {
    /// The game's universe id; its icon comes as an `Avatar` for "game:{universe}".
    pub universe: String,
    /// The place it starts in.
    pub place: String,
    pub name: String,
    /// The account that played it last.
    pub account: String,
    /// When (unix seconds).
    pub when: f64,
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
    /// The game (its universe id).
    #[serde(default)]
    pub universe: Option<String>,
    /// Your ping there (milliseconds), once the client has connected.
    #[serde(default)]
    pub ping: Option<u32>,
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
    /// Your ping there (milliseconds), known only for servers one of your
    /// accounts has connected to.
    pub ping: Option<u32>,
    /// Known only for servers one of your accounts has been in.
    pub location: Option<String>,
}

/// The icons a plugin can name: Material Symbols' names, drawn rounded and
/// filled like the bar's own.
pub const ICONS: [&str; 48] = [
    "add", "bolt", "bookmark", "bug_report", "chat", "check", "close", "content_copy", "delete", "edit",
    "emoji_events", "explore", "extension", "favorite", "flag", "games-recent", "group", "history", "home", "info",
    "link", "lock", "map", "music_note", "notifications", "pause", "person", "play_arrow", "public", "refresh",
    "rocket_launch", "schedule", "search", "select_window", "settings", "shield", "shuffle", "skip_next",
    "sports_esports", "star", "sticky_note_2", "storage", "swap_horiz", "sync", "timer", "visibility", "volume_off",
    "volume_up",
];

/// A plugin's button on the bar.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginButton {
    pub plugin: String,
    pub id: String,
    /// A built-in icon's name (Material Symbols', e.g. "timer"), or
    /// "plugin:ID/FILE" for a picture of the plugin's, sent as an `Avatar` and
    /// drawn as a mask in the text's colour.
    pub icon: String,
    pub tip: String,
    #[serde(default)]
    pub badge: Option<u32>,
}

/// A plugin's drop-down.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Menu {
    pub title: String,
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(default)]
    pub children: Vec<Node>,
}

/// One building block of a plugin's menu. Styles are names, so a newer Mace's
/// styles fall back to the plain one in an older panel; a control's `key` is
/// what the panel sends back when it's used (none: it does nothing).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Node {
    /// `style`: normal, subtle or title.
    Text { text: String, #[serde(default)] style: String },
    /// `style`: primary, secondary or danger.
    Button {
        label: String,
        #[serde(default)]
        style: String,
        #[serde(default)]
        icon: Option<String>,
        #[serde(default)]
        key: Option<String>,
    },
    /// A list row: an icon or a picture, a title and a line under it, and maybe a button at its end.
    Row {
        #[serde(default)]
        icon: Option<String>,
        #[serde(default)]
        image: Option<String>,
        title: String,
        #[serde(default)]
        subtitle: Option<String>,
        #[serde(default)]
        key: Option<String>,
        #[serde(default)]
        trailing: Option<Box<Node>>,
    },
    Toggle {
        label: String,
        value: bool,
        #[serde(default)]
        key: Option<String>,
    },
    Input {
        #[serde(default)]
        placeholder: String,
        #[serde(default)]
        value: String,
        #[serde(default)]
        key: Option<String>,
    },
    /// `direction`: column (the default) or row; `gap`: small or medium.
    Stack {
        #[serde(default)]
        direction: String,
        #[serde(default)]
        gap: String,
        children: Vec<Node>,
    },
    /// `size`: small or medium.
    Space { #[serde(default)] size: String },
    /// What an older panel gets for a block a newer Mace has: nothing.
    #[serde(other)]
    Unknown,
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

    #[test]
    fn a_newer_menu_still_reads() {
        let line = r#"{"type":"plugin_menu","plugin":"notes","button":"b","menu":{"title":"Notes","children":[
            {"type":"text","text":"hi"},{"type":"chart","points":[1,2]},
            {"type":"button","label":"Go","style":"glowing","key":"cb:1"}]}}"#;
        let ToPanel::PluginMenu { menu, .. } = serde_json::from_str(line).unwrap() else { panic!() };
        assert_eq!(menu.children[1], Node::Unknown);
        assert!(matches!(&menu.children[2], Node::Button { style, key: Some(k), .. } if style == "glowing" && k == "cb:1"));
    }
}

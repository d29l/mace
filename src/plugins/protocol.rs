//! What an account's supervisor and its plugin host say to each other: one
//! JSON object per line over the host's stdin and stdout. Private to Mace (the
//! plugins' API is the Luau one), so it can change with Mace.

use mace_proto::{Menu, PluginButton, State};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// From the supervisor to the host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToHost {
    /// What the panel knows: every account, their servers, recent games.
    State { state: State },
    /// Something happened: `joined` (data: the server), `left`, `opened` and
    /// `closed` (the bar), `focused`.
    Event {
        name: String,
        #[serde(default)]
        data: Value,
    },
    /// The panel used a plugin's menu (`mace_proto::ToMace::Plugin`).
    Ui {
        plugin: String,
        button: String,
        key: String,
        what: String,
        #[serde(default)]
        value: Option<Value>,
    },
}

/// From the host to the supervisor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromHost {
    /// Every plugin button, each time any changes.
    Buttons { buttons: Vec<PluginButton> },
    Menu { plugin: String, button: String, menu: Menu },
    /// A plugin's picture, square RGBA in base64, for `Avatar` key `key`.
    Image { key: String, size: u32, rgba: String },
    Toast { title: String, body: String },
    Copy { text: String },
    /// Close the bar.
    Close,
    /// A `mace` command for this account (start, stop, rejoin, gather), its
    /// permission already checked.
    Act { args: Vec<String> },
    /// A line for the account's log.
    Log { text: String },
}

impl ToHost {
    pub fn line(&self) -> String {
        serde_json::to_string(self).expect("plain data") + "\n"
    }
}

impl FromHost {
    pub fn line(&self) -> String {
        serde_json::to_string(self).expect("plain data") + "\n"
    }
}

//! Mace: run the macOS Roblox client on Linux through Rosemary, on as many
//! accounts as you like, each on the Roblox build its own channel wants.

pub mod accounts;
pub mod builds;
pub mod cli;
pub mod discord;
pub mod desktop;
pub mod doctor;
pub mod fsutil;
pub mod launch;
pub mod links;
pub mod migrate;
pub mod mods;
pub mod panel;
pub mod recent;
pub mod paths;
pub mod processes;
pub mod runner;
pub mod servers;
pub mod settings;
pub mod supervisor;
pub mod ui;
pub mod upgrade;

#[cfg(test)]
pub mod testenv;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The Mace app, which starts Roblox (`mace play`).
pub const APP_ID: &str = "io.github.Mace";
/// Mace Settings: the window with the accounts and settings.
pub const SETTINGS_ID: &str = "io.github.Mace.Settings";

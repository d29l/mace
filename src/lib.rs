//! Mace: run the macOS Roblox client on Linux through Rosemary, on as many
//! accounts as you like, each on the Roblox build its own channel wants.

pub mod accounts;
pub mod builds;
pub mod cli;
pub mod desktop;
pub mod doctor;
pub mod fsutil;
pub mod launch;
pub mod links;
pub mod migrate;
pub mod paths;
pub mod processes;
pub mod runner;
pub mod servers;
pub mod settings;
pub mod supervisor;
pub mod ui;

#[cfg(test)]
pub mod testenv;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const APP_ID: &str = "io.github.Mace";

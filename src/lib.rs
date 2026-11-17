//! Mace: run the macOS Roblox client on Linux through Rosemary, on as many
//! accounts as you like, each on the Roblox build its own channel wants.

pub mod accounts;
pub mod builds;
pub mod fsutil;
pub mod paths;
pub mod runner;
pub mod settings;

#[cfg(test)]
pub mod testenv;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const APP_ID: &str = "io.github.Mace";

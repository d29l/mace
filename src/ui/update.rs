//! The bar under the window's title when a newer Mace is out: Update, then
//! Restart into it. Only for the copy that updates itself (see `upgrade`).

use std::cell::RefCell;
use std::os::unix::process::CommandExt;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use adw::prelude::*;
use gtk::{gio, glib};

use super::widgets::background;
use crate::upgrade;

/// The window closed to start the new version, which `run_window` then does.
static RESTART: AtomicBool = AtomicBool::new(false);

enum State {
    /// Nothing to offer.
    Quiet,
    /// This version is out.
    Out(String),
    /// Being installed.
    Busy,
    /// Installed: the button restarts Mace.
    Installed,
}

/// The bar, hidden until GitHub has a newer version: asked when the window
/// opens and every hour while it's open.
pub fn bar(window: &adw::ApplicationWindow, toast: impl Fn(&str) + 'static) -> adw::Banner {
    let banner = adw::Banner::new("");
    let state = Rc::new(RefCell::new(State::Quiet));
    let toast = Rc::new(toast);
    let window = window.downgrade();
    banner.connect_button_clicked({
        let state = state.clone();
        move |banner| match state.replace(State::Busy) {
            State::Out(version) => {
                banner.set_title(&format!("Updating to Mace {version}…"));
                banner.set_button_label(None);
                let (banner, state, toast) = (banner.clone(), state.clone(), toast.clone());
                background(|| upgrade::update_now().map_err(|e| format!("{e:#}")), move |result| match result {
                    Ok(Some(installed)) => {
                        banner.set_title(&format!("Mace {installed} is installed"));
                        banner.set_button_label(Some("Restart"));
                        state.replace(State::Installed);
                    }
                    Ok(None) => {
                        banner.set_revealed(false);
                        state.replace(State::Quiet);
                    }
                    Err(error) => {
                        toast(&format!("Couldn't update: {error}"));
                        offer(&banner, &state, version);
                    }
                });
            }
            State::Installed => {
                state.replace(State::Installed);
                RESTART.store(true, Ordering::Relaxed);
                if let Some(window) = window.upgrade() {
                    window.close();
                }
            }
            other => {
                state.replace(other);
            }
        }
    });
    check(&banner, &state);
    let weak = banner.downgrade();
    glib::timeout_add_seconds_local(upgrade::EVERY.as_secs() as u32, move || match weak.upgrade() {
        Some(banner) => {
            check(&banner, &state);
            glib::ControlFlow::Continue
        }
        None => glib::ControlFlow::Break,
    });
    banner
}

/// Ask GitHub, off the main loop, and offer what it has; not while an update
/// is being installed or waits for a restart.
fn check(banner: &adw::Banner, state: &Rc<RefCell<State>>) {
    if !matches!(*state.borrow(), State::Quiet | State::Out(_)) {
        return;
    }
    let (banner, state) = (banner.clone(), state.clone());
    background(upgrade::available, move |newer| {
        if let Some(version) = newer
            && matches!(*state.borrow(), State::Quiet | State::Out(_))
        {
            offer(&banner, &state, version);
        }
    });
}

fn offer(banner: &adw::Banner, state: &RefCell<State>, version: String) {
    banner.set_title(&format!("Mace {version} is out"));
    banner.set_button_label(Some("Update"));
    banner.set_revealed(true);
    state.replace(State::Out(version));
}

/// After the window closed to restart: the new version in this process's place.
pub fn restart_if_asked() {
    if !RESTART.load(Ordering::Relaxed) {
        return;
    }
    let Some(program) = upgrade::installed_copy() else { return };
    // let go of the session bus, so the new window can have Mace's name on it
    if let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
        let _ = bus.close_sync(gio::Cancellable::NONE);
    }
    let error = std::process::Command::new(&program).exec();
    eprintln!("mace: couldn't restart: {error}");
}

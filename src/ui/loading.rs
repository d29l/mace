//! The loading window for one account's start: a small window you can drag
//! anywhere, opened by the launch supervisor. It follows the supervisor's
//! status file and shows the logo, the account's headshot, the game, what is
//! happening and a progress bar. It closes once Roblox's window is up; a failed
//! start stays a few seconds with the reason.

use std::cell::{Cell, RefCell};
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gdk, glib};
use serde_json::Value;

use crate::accounts::Account;
use crate::{APP_ID, fsutil, paths, supervisor};

/// No outline or shadow around the card, and grey initials.
pub const STYLE: &str = "
window.mace-loading, window.mace-loading.csd { box-shadow: none; outline: none; border: none; }
window.mace-loading avatar { background-image: none; background-color: #3a3a3e; color: #ededef; }
";
const ACTIVE: [&str; 4] = ["preparing", "downloading", "starting", "updating"];
const FAILED_FOR: Duration = Duration::from_secs(6);
/// No status at all by then: the start never got going.
const GIVE_UP: Duration = Duration::from_secs(15);

/// On Hyprland: no compositor border either, and floating in the middle.
fn hyprland_rules() {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() || fsutil::which("hyprctl").is_none() {
        return;
    }
    let rule = format!("hl.window_rule({{ match = {{ class = \"^{}\\\\.Loading$\" }}, float = true, center = true, \
                        border_size = 0, no_shadow = true }})", APP_ID.replace('.', "\\\\."));
    let _ = Command::new("hyprctl").args(["eval", &rule]).output();
}

pub fn open(app: &adw::Application, account: &Account) {
    hyprland_rules();
    let name = account.name();
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(format!("Starting Roblox · {name}"))
        .resizable(false)
        .default_width(380)
        .default_height(1)
        .build();
    window.add_css_class("mace-loading");

    let avatar = adw::Avatar::new(56, Some(&name), true);
    let game = gtk::Label::builder().label("Roblox").xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["title-3"]).build();
    let who = gtk::Label::builder().label(format!("as {name}")).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["dim-label"]).build();
    let heading = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).valign(gtk::Align::Center)
        .hexpand(true).build();
    heading.append(&game);
    heading.append(&who);
    let top = gtk::Box::builder().spacing(14).build();
    top.append(&avatar);
    top.append(&heading);
    let message = gtk::Label::builder().label("Starting…").xalign(0.0).wrap(true).max_width_chars(40).build();
    let bar = gtk::ProgressBar::builder().pulse_step(0.08).build();
    let hint = gtk::Label::builder().xalign(0.0).wrap(true).visible(false).css_classes(["dim-label", "caption"]).build();

    // libadwaita windows are at least 200 px tall: the content sits in the middle of that
    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(14)
        .margin_top(22).margin_bottom(22).margin_start(22).margin_end(22)
        .valign(gtk::Align::Center).vexpand(true).build();
    if paths::logo().exists() {
        content.append(&gtk::Image::builder().file(paths::logo().to_string_lossy()).pixel_size(64)
            .halign(gtk::Align::Center).margin_bottom(4).build());
    }
    for widget in [top.upcast_ref::<gtk::Widget>(), message.upcast_ref(), bar.upcast_ref(), hint.upcast_ref()] {
        content.append(widget);
    }
    // drag the window by any part of it
    window.set_content(Some(&gtk::WindowHandle::builder().child(&content).build()));

    let keys = gtk::EventControllerKey::new();
    let closing = window.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape {
            closing.close();
        }
        glib::Propagation::Proceed
    });
    window.add_controller(keys);

    let opened = Instant::now();
    let seen = Cell::new(false);
    let failed_at: Cell<Option<Instant>> = Cell::new(None);
    let avatar_path = RefCell::new(String::new());
    let id = account.id.clone();
    let win = window.clone();
    let tick = Rc::new(move || -> glib::ControlFlow {
        let status = supervisor::read_status(&id);
        let state = status.get("state").and_then(Value::as_str).unwrap_or("");
        if status.is_empty() {
            // gone after it was there: the supervisor finished; never there: it didn't start
            if seen.get() || opened.elapsed() > GIVE_UP {
                win.close();
                return glib::ControlFlow::Break;
            }
            bar.pulse();
            return glib::ControlFlow::Continue;
        }
        seen.set(true);
        if let Some(name) = status.get("game").and_then(Value::as_str) {
            game.set_label(name);
        }
        if let Some(path) = status.get("avatar").and_then(Value::as_str) {
            if *avatar_path.borrow() != path {
                *avatar_path.borrow_mut() = path.to_string();
                if let Ok(texture) = gdk::Texture::from_filename(path) {
                    avatar.set_custom_image(Some(&texture));
                }
            }
        }
        if state == "running" || state == "done" {
            win.close();
            return glib::ControlFlow::Break;
        }
        if state == "failed" {
            match failed_at.get() {
                None => {
                    failed_at.set(Some(Instant::now()));
                    let text = status.get("message").and_then(Value::as_str).unwrap_or("Roblox didn't start");
                    message.set_label(text);
                    message.add_css_class("error");
                    bar.set_visible(false);
                    hint.set_label(&format!("`mace why {}` has the details", status.get("account").and_then(Value::as_str).unwrap_or("")));
                    hint.set_visible(true);
                }
                Some(at) if at.elapsed() > FAILED_FOR => {
                    win.close();
                    return glib::ControlFlow::Break;
                }
                _ => {}
            }
            return glib::ControlFlow::Continue;
        }
        if ACTIVE.contains(&state) {
            let text = status.get("message").and_then(Value::as_str).filter(|m| !m.is_empty()).unwrap_or("Starting");
            message.set_label(&format!("{}…", text.trim_end_matches('.')));
            match status.get("progress").and_then(Value::as_f64) {
                Some(share) => bar.set_fraction(share),
                None => bar.pulse(),
            }
        }
        glib::ControlFlow::Continue
    });
    let tick_again = tick.clone();
    glib::timeout_add_local(Duration::from_millis(100), move || tick_again());
    window.present();
}

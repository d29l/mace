//! The loading window for one account's start, opened by the launch
//! supervisor: a small card you can drag anywhere. The mark turns while Mace
//! works; under it the game, who it's starting as, what's happening, how far
//! along it is, and a way to call it off. It closes once Roblox's window is
//! up; a failed start stays with the reason until it's closed (or a while).

use std::cell::{Cell, RefCell};
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gdk, glib};
use serde_json::Value;

use super::widgets::background;
use crate::accounts::Account;
use crate::{APP_ID, fsutil, paths, supervisor};

/// The card: no outline or shadow (the compositor's own is enough), the
/// turning mark, a slim progress bar, and quiet secondary text.
pub const STYLE: &str = "
window.mace-loading, window.mace-loading.csd { box-shadow: none; outline: none; border: none; }
window.mace-loading avatar { background-image: none; background-color: #3a3a3e; color: #ededef; }
@keyframes mace-turn { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }
window.mace-loading .mace-mark.working { animation: mace-turn 3s linear infinite; }
window.mace-loading .mace-game { font-size: 20px; font-weight: 800; }
window.mace-loading .mace-who { color: alpha(@window_fg_color, 0.62); }
window.mace-loading .mace-status { font-weight: 600; }
window.mace-loading .mace-quiet { font-size: 12.5px; color: alpha(@window_fg_color, 0.55); }
window.mace-loading .mace-failed { color: @error_color; }
window.mace-loading progressbar trough { min-height: 6px; border-radius: 999px; border: none;
                                         background-color: alpha(@window_fg_color, 0.1); }
window.mace-loading progressbar progress { min-height: 6px; border-radius: 999px; border: none;
                                           background-color: @window_fg_color; }
window.mace-loading button.mace-cancel { padding: 8px 26px; }
";
const ACTIVE: [&str; 4] = ["preparing", "downloading", "starting", "updating"];
/// A failed start stays this long unless closed first.
const FAILED_FOR: Duration = Duration::from_secs(20);
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

fn megabytes(bytes: f64) -> String {
    format!("{:.0} MB", bytes / 1_000_000.0)
}

pub fn open(app: &adw::Application, account: &Account) {
    hyprland_rules();
    let name = account.name();
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(format!("Starting Roblox · {name}"))
        .resizable(false)
        .default_width(400)
        .default_height(1)
        .build();
    window.add_css_class("mace-loading");

    // who and what: the mark, the game, the account
    let mark = gtk::Image::builder().pixel_size(72).halign(gtk::Align::Center).css_classes(["mace-mark", "working"]).build();
    if paths::logo().exists() {
        mark.set_from_file(Some(paths::logo()));
    }
    let game = gtk::Label::builder().label("Roblox").justify(gtk::Justification::Center).halign(gtk::Align::Center)
        .ellipsize(gtk::pango::EllipsizeMode::End).max_width_chars(30).css_classes(["mace-game"]).margin_top(18).build();
    let avatar = adw::Avatar::new(22, Some(&name), true);
    let who = gtk::Box::builder().spacing(8).halign(gtk::Align::Center).margin_top(6).build();
    who.append(&avatar);
    who.append(&gtk::Label::builder().label(format!("as {name}")).ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["mace-who"]).build());

    // what's happening, and how far along
    let message = gtk::Label::builder().label("Starting…").xalign(0.0).hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End).css_classes(["mace-status"]).build();
    let percent = gtk::Label::builder().xalign(1.0).visible(false).css_classes(["numeric", "mace-quiet"]).build();
    let status = gtk::Box::builder().spacing(12).margin_top(28).build();
    status.append(&message);
    status.append(&percent);
    let bar = gtk::ProgressBar::builder().pulse_step(0.06).margin_top(10).build();
    let detail = gtk::Label::builder().xalign(0.0).wrap(true).visible(false).css_classes(["mace-quiet"])
        .margin_top(8).build();
    let cancel = gtk::Button::builder().label("Cancel").halign(gtk::Align::Center).margin_top(24)
        .css_classes(["pill", "mace-cancel"]).build();

    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical)
        .margin_top(36).margin_bottom(28).margin_start(32).margin_end(32)
        .valign(gtk::Align::Center).vexpand(true).build();
    for widget in [mark.upcast_ref::<gtk::Widget>(), game.upcast_ref(), who.upcast_ref(), status.upcast_ref(),
                   bar.upcast_ref(), detail.upcast_ref(), cancel.upcast_ref()] {
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

    // Cancel stops the start (its supervisor and any Roblox it began); once it
    // failed, the same button just closes the window
    let failed = Rc::new(Cell::new(false));
    {
        let (failed, id, window, message) = (failed.clone(), account.id.clone(), window.clone(), message.clone());
        cancel.connect_clicked(move |button| {
            if failed.get() {
                window.close();
                return;
            }
            button.set_sensitive(false);
            message.set_label("Cancelling…");
            let (id, window) = (id.clone(), window.clone());
            background(move || supervisor::cancel(&id), move |_| window.close());
        });
    }

    let opened = Instant::now();
    let seen = Cell::new(false);
    let failed_at: Cell<Option<Instant>> = Cell::new(None);
    let avatar_path = RefCell::new(String::new());
    let id = account.id.clone();
    let win = window.clone();
    let tick = Rc::new(move || -> glib::ControlFlow {
        let state_of = supervisor::read_status(&id);
        let state = state_of.get("state").and_then(Value::as_str).unwrap_or("");
        if state_of.is_empty() {
            // gone after it was there: the supervisor finished; never there: it didn't start
            if seen.get() || opened.elapsed() > GIVE_UP {
                win.close();
                return glib::ControlFlow::Break;
            }
            bar.pulse();
            return glib::ControlFlow::Continue;
        }
        seen.set(true);
        if let Some(name) = state_of.get("game").and_then(Value::as_str) {
            game.set_label(name);
        }
        if let Some(path) = state_of.get("avatar").and_then(Value::as_str)
            && *avatar_path.borrow() != path
        {
            *avatar_path.borrow_mut() = path.to_string();
            if let Ok(texture) = gdk::Texture::from_filename(path) {
                avatar.set_custom_image(Some(&texture));
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
                    failed.set(true);
                    mark.remove_css_class("working");
                    let text = state_of.get("message").and_then(Value::as_str).unwrap_or("Roblox didn't start");
                    message.set_label(text);
                    message.set_ellipsize(gtk::pango::EllipsizeMode::None);
                    message.set_wrap(true);
                    message.add_css_class("mace-failed");
                    percent.set_visible(false);
                    bar.set_visible(false);
                    detail.set_label(&format!("Details: mace why {id}"));
                    detail.set_visible(true);
                    cancel.set_label("Close");
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
            let text = state_of.get("message").and_then(Value::as_str).filter(|m| !m.is_empty()).unwrap_or("Starting");
            message.set_label(&format!("{}…", text.trim_end_matches('.')));
            match state_of.get("progress").and_then(Value::as_f64) {
                Some(share) => {
                    bar.set_fraction(share);
                    percent.set_label(&format!("{:.0}%", share * 100.0));
                    percent.set_visible(true);
                }
                None => {
                    bar.pulse();
                    percent.set_visible(false);
                }
            }
            // how much of a download is done, while one runs
            let size = state_of.get("size").and_then(Value::as_array)
                .and_then(|s| Some((s.first()?.as_f64()?, s.get(1)?.as_f64()?)));
            match size.filter(|_| state == "downloading") {
                Some((done, total)) if total > 0.0 => {
                    detail.set_label(&format!("{} of {}", megabytes(done), megabytes(total)));
                    detail.set_visible(true);
                }
                _ => detail.set_visible(false),
            }
        }
        glib::ControlFlow::Continue
    });
    let tick_again = tick.clone();
    glib::timeout_add_local(Duration::from_millis(100), move || tick_again());
    window.present();
}

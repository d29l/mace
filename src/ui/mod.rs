//! Mace's windows: the main window (accounts and settings) and the loading
//! window. GTK is only started here, so the command line stays quick.

mod account_settings;
mod loading;
mod mods_tab;
mod plugins_tab;
mod settings_tabs;
mod update;
mod widgets;
mod window;

use adw::prelude::*;
use gtk::{gdk, glib};

use crate::{APP_ID, SETTINGS_ID};

/// Mace looks the same everywhere: dark, plain greys. Set above the user's own
/// GTK colours (a desktop theme's gtk.css), which would otherwise recolour it.
const STYLE: &str = "
@define-color accent_color #deddda;
@define-color accent_bg_color #4a4a4f;
@define-color accent_fg_color #ffffff;
@define-color destructive_color #ff7b63;
@define-color destructive_bg_color #c01c28;
@define-color destructive_fg_color #ffffff;
@define-color error_color #ff7b63;
@define-color error_bg_color #c01c28;
@define-color error_fg_color #ffffff;
@define-color success_bg_color #26a269;
@define-color window_bg_color #222224;
@define-color window_fg_color #ededef;
@define-color view_bg_color #1d1d1f;
@define-color view_fg_color #ededef;
@define-color headerbar_bg_color #2a2a2c;
@define-color headerbar_fg_color #ededef;
@define-color headerbar_backdrop_color #222224;
@define-color sidebar_bg_color #2a2a2c;
@define-color sidebar_fg_color #ededef;
@define-color sidebar_backdrop_color #252527;
@define-color card_bg_color #2e2e31;
@define-color card_fg_color #ededef;
@define-color popover_bg_color #333336;
@define-color popover_fg_color #ededef;
@define-color dialog_bg_color #2a2a2c;
@define-color dialog_fg_color #ededef;
.mace-game-icon { border-radius: 8px; }
";

/// Dark, grey and Adwaita's icons, whatever the desktop's theme.
fn use_mace_look(extra: &str) {
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_icon_theme_name(Some("Adwaita"));
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&format!("{STYLE}{extra}"));
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_USER + 1);
    }
}

/// Mace Settings: the accounts and settings window.
pub fn run_window() -> i32 {
    let app = adw::Application::builder().application_id(SETTINGS_ID).build();
    app.connect_startup(|_| use_mace_look(""));
    app.connect_activate(|app| {
        match app.active_window() {
            Some(window) => window.present(),
            None => window::Window::new(app).present(),
        }
    });
    let code = exit_code(app.run_with_args::<&str>(&[]));
    update::restart_if_asked();
    code
}

/// The loading window for one account's start.
pub fn run_loading(account_id: &str) -> i32 {
    let Ok(account) = crate::accounts::get(account_id) else { return 1 };
    let app = adw::Application::builder()
        .application_id(format!("{APP_ID}.Loading"))
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_startup(|_| use_mace_look(loading::STYLE));
    app.connect_activate(move |app| loading::open(app, &account));
    exit_code(app.run_with_args::<&str>(&[]))
}

fn exit_code(code: glib::ExitCode) -> i32 {
    i32::from(code.get())
}

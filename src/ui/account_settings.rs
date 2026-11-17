//! One account's own settings, over the ones every account shares.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use serde_json::{Map, Value};

use super::widgets::{PRESENTATION_CHOICES, chosen, combo, pairs, pairs_editor, read_pairs, spin};
use crate::accounts::Account;
use crate::settings::{self, Overrides, valid_cores};

type Saver = Box<dyn Fn(&mut Overrides)>;

/// Shows the dialog; `saved` runs with a message once it closes.
pub fn present(parent: &impl IsA<gtk::Widget>, account: &Account, saved: impl Fn(String) + 'static) {
    let config = settings::load();
    let own = config.overrides(&account.id);
    let everyone = config.for_account("");
    let dialog = adw::PreferencesDialog::builder().title(format!("{}'s settings", account.name())).search_enabled(false).build();
    let page = adw::PreferencesPage::builder().title("This account").build();
    dialog.add(&page);
    let group = adw::PreferencesGroup::builder()
        .title("Own settings")
        .description("Switch one on to give this account its own value; off uses the one every account has.")
        .build();
    page.add(&group);
    let savers: Rc<RefCell<Vec<Saver>>> = Rc::new(RefCell::new(Vec::new()));

    let expander = |title: &str, on: bool, child: &gtk::Widget| {
        let row = adw::ExpanderRow::builder().title(title).show_enable_switch(true).enable_expansion(on).expanded(on).build();
        row.add_row(child);
        group.add(&row);
        row
    };

    let modes = pairs(&PRESENTATION_CHOICES);
    let mode = combo("Presentation mode", &modes, own.presentation_mode.as_deref().unwrap_or(&everyone.presentation_mode), "");
    let mode_on = expander("Presentation mode", own.presentation_mode.is_some(), mode.upcast_ref());
    savers.borrow_mut().push(Box::new(move |o| {
        o.presentation_mode = mode_on.enables_expansion().then(|| chosen(&mode, &modes));
    }));

    let refresh = spin("Hz", own.refresh_rate.unwrap_or(everyone.refresh_rate), 100000.0, "0 = the screen's real one");
    let refresh_on = expander("Refresh rate Roblox sees", own.refresh_rate.is_some(), refresh.upcast_ref());
    savers.borrow_mut().push(Box::new(move |o| o.refresh_rate = refresh_on.enables_expansion().then(|| refresh.value() as u64)));

    let cap = spin("FPS", own.fps_cap.unwrap_or(everyone.fps_cap), 100000.0, "Roblox's own setting");
    let cap_on = expander("Frame-rate cap", own.fps_cap.is_some(), cap.upcast_ref());
    savers.borrow_mut().push(Box::new(move |o| o.fps_cap = cap_on.enables_expansion().then(|| cap.value() as u64)));

    let cores = adw::EntryRow::builder().title("e.g. 6,7 (empty = all)")
        .text(own.cpu_cores.as_deref().unwrap_or(&everyone.cpu_cores)).build();
    let cores_on = expander("CPU cores", own.cpu_cores.is_some(), cores.upcast_ref());
    let kept = own.cpu_cores.clone();
    savers.borrow_mut().push(Box::new(move |o| {
        let text = cores.text().replace(' ', "");
        o.cpu_cores = cores_on.enables_expansion().then(|| if valid_cores(&text) { text } else { kept.clone().unwrap_or_default() });
    }));

    for (key, title) in [("gamemode", "GameMode"), ("mangohud", "MangoHud overlay")] {
        let current = if key == "gamemode" { own.gamemode } else { own.mangohud };
        let fallback = if key == "gamemode" { everyone.gamemode } else { everyone.mangohud };
        let switch = adw::SwitchRow::builder().title("On").active(current.unwrap_or(fallback)).build();
        let on = expander(title, current.is_some(), switch.upcast_ref());
        savers.borrow_mut().push(Box::new(move |o| {
            let value = on.enables_expansion().then(|| switch.is_active());
            if key == "gamemode" { o.gamemode = value } else { o.mangohud = value }
        }));
    }

    let problems: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    for (key, title, description) in [
        ("fast_flags", "Extra FastFlags", "Added to (or replacing) every account's. One per line: Name=value"),
        ("extra_env", "Extra environment variables", "Added to every account's. One per line: NAME=value"),
    ] {
        let group = adw::PreferencesGroup::builder().title(title).description(description).build();
        page.add(&group);
        let current = if key == "fast_flags" { own.fast_flags.clone() } else { own.extra_env.clone() };
        let (editor, buffer) = pairs_editor(&current.unwrap_or_default());
        group.add(&editor);
        let problems = problems.clone();
        savers.borrow_mut().push(Box::new(move |o| {
            let (pairs, bad) = read_pairs(&buffer);
            problems.borrow_mut().extend(bad);
            let mut map = Map::new();
            for (name, value) in pairs {
                let value = if key == "fast_flags" { settings::parse_flag_value(&value) } else { Value::String(value) };
                map.insert(name, value);
            }
            if key == "fast_flags" {
                o.fast_flags = (!map.is_empty()).then(|| settings::clean_flags(Some(&Value::Object(map))));
            } else {
                o.extra_env = (!map.is_empty()).then(|| settings::clean_env(Some(&Value::Object(map))));
            }
        }));
    }

    let (id, name) = (account.id.clone(), account.name());
    dialog.connect_closed(move |_| {
        let mut overrides = Overrides::default();
        for saver in savers.borrow().iter() {
            saver(&mut overrides);
        }
        // only this account's part goes back: the rest may have changed in the window meanwhile
        let mut current = settings::load();
        current.set_overrides(&id, &overrides);
        let _ = settings::save(&current);
        let bad = problems.borrow();
        if bad.is_empty() {
            saved(format!("{name}'s settings saved; they apply at its next start"));
        } else {
            saved(format!("Skipped lines without '=': {}", bad.join(", ").chars().take(80).collect::<String>()));
        }
    });
    dialog.present(Some(parent));
}

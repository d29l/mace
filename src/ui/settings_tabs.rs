//! The settings every account shares, as the window's General and Advanced
//! tabs. Changes save themselves shortly after the last edit; each save reads
//! the file again first, so account settings made meanwhile are kept.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use serde_json::{Map, Value};

use super::widgets::{PRESENTATION_CHOICES, chosen, combo, pairs, pairs_editor, read_pairs, spin};
use crate::settings::{self, Settings};
use crate::{runner, settings::valid_cores};

type Saver = Box<dyn Fn(&mut Settings)>;

pub struct GlobalSettings {
    pub general: adw::PreferencesPage,
    pub advanced: adw::PreferencesPage,
    savers: RefCell<Vec<Saver>>,
    pending: RefCell<Option<glib::SourceId>>,
}

impl GlobalSettings {
    pub fn new() -> Rc<GlobalSettings> {
        let me = Rc::new(GlobalSettings {
            general: adw::PreferencesPage::builder().title("General").icon_name("emblem-system-symbolic").build(),
            advanced: adw::PreferencesPage::builder().title("Advanced").icon_name("preferences-other-symbolic").build(),
            savers: RefCell::new(Vec::new()),
            pending: RefCell::new(None),
        });
        let s = settings::load();
        me.build_general(&s);
        me.build_advanced(&s);
        me
    }

    fn saver(&self, saver: impl Fn(&mut Settings) + 'static) {
        self.savers.borrow_mut().push(Box::new(saver));
    }

    /// A function that schedules a save, for the widgets' change signals.
    fn on_change(self: &Rc<Self>) -> impl Fn() + Clone + 'static {
        let me = Rc::downgrade(self);
        move || {
            if let Some(me) = me.upgrade() {
                me.changed();
            }
        }
    }

    fn changed(self: &Rc<Self>) {
        if let Some(id) = self.pending.borrow_mut().take() {
            id.remove();
        }
        let me = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(Duration::from_millis(500), move || {
            if let Some(me) = me.upgrade() {
                me.pending.borrow_mut().take();
                me.save();
            }
        });
        *self.pending.borrow_mut() = Some(id);
    }

    pub fn save(&self) {
        let mut current = settings::load();
        for saver in self.savers.borrow().iter() {
            saver(&mut current);
        }
        let _ = settings::save(&current);
    }

    /// A change made just before the window closes.
    pub fn flush(&self) {
        if let Some(id) = self.pending.borrow_mut().take() {
            id.remove();
            self.save();
        }
    }

    fn build_general(self: &Rc<Self>, s: &Settings) {
        let page = &self.general;
        let changed = self.on_change();

        let group = adw::PreferencesGroup::builder()
            .title("Rosemary")
            .description("Changes are saved as you make them and apply at the next start.")
            .build();
        page.add(&group);
        let runners = runner::butter_runners();
        let mut choices = vec![("auto".to_string(), "Newest Butter runner".to_string())];
        choices.extend(runners.iter().map(|r| (r.path.to_string_lossy().into_owned(), r.name.clone())));
        if !choices.iter().any(|(v, _)| *v == s.rosemary) {
            choices.push((s.rosemary.clone(), s.rosemary.clone()));
        }
        let row = combo("Runner", &choices, &s.rosemary, if runners.is_empty() { "Butter has no runners installed" } else { "" });
        group.add(&row);
        row.connect_selected_notify({ let c = changed.clone(); move |_| c() });
        self.saver(move |t| t.rosemary = chosen(&row, &choices));

        let group = adw::PreferencesGroup::builder()
            .title("Frame rate")
            .description("Roblox caps its FPS at the refresh rate it's told and at its own frame-rate setting, \
                          whose menu stops at 240.")
            .build();
        page.add(&group);
        let modes = pairs(&PRESENTATION_CHOICES);
        let mode = combo("Presentation mode", &modes, &s.presentation_mode, "");
        let refresh = spin("Refresh rate Roblox sees", s.refresh_rate, 100000.0, "0 = the screen's real one");
        let cap = spin("Frame-rate cap", s.fps_cap, 100000.0, "Roblox's own setting; 0 = leave it as it is");
        group.add(&mode);
        group.add(&refresh);
        group.add(&cap);
        mode.connect_selected_notify({ let c = changed.clone(); move |_| c() });
        refresh.connect_value_notify({ let c = changed.clone(); move |_| c() });
        cap.connect_value_notify({ let c = changed.clone(); move |_| c() });
        self.saver(move |t| t.presentation_mode = chosen(&mode, &modes));
        self.saver(move |t| t.refresh_rate = refresh.value() as u64);
        self.saver(move |t| t.fps_cap = cap.value() as u64);

        let group = adw::PreferencesGroup::builder().title("Window and input").build();
        page.add(&group);
        let decorations = pairs(&[("server", "Your desktop's"), ("client", "Mac style")]);
        let menus = pairs(&[("none", "Hidden"), ("strip", "Strip under the title bar"), ("titlebar", "In the title bar")]);
        let deco = combo("Title bar", &decorations, &s.decorations, "");
        let menu = combo("Mac menu bar", &menus, &s.menu, "");
        let edit = adw::SwitchRow::builder().title("Ctrl for copy and paste")
            .subtitle("Ctrl+C, V, X, A, Z act as Cmd in text fields").active(s.control_edit_keys).build();
        let confine = adw::SwitchRow::builder().title("Keep the pointer in the window").active(s.confine_pointer).build();
        group.add(&deco);
        group.add(&menu);
        group.add(&edit);
        group.add(&confine);
        deco.connect_selected_notify({ let c = changed.clone(); move |_| c() });
        menu.connect_selected_notify({ let c = changed.clone(); move |_| c() });
        edit.connect_active_notify({ let c = changed.clone(); move |_| c() });
        confine.connect_active_notify({ let c = changed.clone(); move |_| c() });
        self.saver(move |t| t.decorations = chosen(&deco, &decorations));
        self.saver(move |t| t.menu = chosen(&menu, &menus));
        self.saver(move |t| t.control_edit_keys = edit.is_active());
        self.saver(move |t| t.confine_pointer = confine.is_active());

        let group = adw::PreferencesGroup::builder().title("Performance").build();
        page.add(&group);
        let gamemode = adw::SwitchRow::builder().title("GameMode").active(s.gamemode).build();
        let mangohud = adw::SwitchRow::builder().title("MangoHud overlay").active(s.mangohud).build();
        let cores = adw::EntryRow::builder().title("CPU cores (e.g. 1-5,7-11; empty = all)").text(&s.cpu_cores).build();
        let wrapper = adw::EntryRow::builder().title("Wrapper command (%command% = Roblox)").text(&s.wrapper).build();
        group.add(&gamemode);
        group.add(&mangohud);
        group.add(&cores);
        group.add(&wrapper);
        gamemode.connect_active_notify({ let c = changed.clone(); move |_| c() });
        mangohud.connect_active_notify({ let c = changed.clone(); move |_| c() });
        cores.connect_changed({ let c = changed.clone(); move |_| c() });
        wrapper.connect_changed({ let c = changed.clone(); move |_| c() });
        self.saver(move |t| t.gamemode = gamemode.is_active());
        self.saver(move |t| t.mangohud = mangohud.is_active());
        self.saver(move |t| {
            let text = cores.text().replace(' ', "");
            if valid_cores(&text) {
                cores.remove_css_class("error");
                t.cpu_cores = text;
            } else {
                cores.add_css_class("error");
            }
        });
        self.saver(move |t| t.wrapper = wrapper.text().trim().to_string());

        let group = adw::PreferencesGroup::builder()
            .title("In-game panel and loading window")
            .description("On Hyprland the panel's key only exists while a Roblox started by Mace has a window. \
                          Empty: no key.")
            .build();
        page.add(&group);
        let key = adw::EntryRow::builder().title("Panel key (e.g. SUPER + TAB)").text(&s.overlay_key).build();
        let fps = adw::SwitchRow::builder().title("Show FPS")
            .subtitle("Rosemary counts the frames; it costs a little speed").active(s.panel_fps).build();
        let loading = adw::SwitchRow::builder().title("Loading window")
            .subtitle("Shows progress while an account starts").active(s.loading_screen).build();
        group.add(&key);
        group.add(&fps);
        group.add(&loading);
        key.connect_changed({ let c = changed.clone(); move |_| c() });
        fps.connect_active_notify({ let c = changed.clone(); move |_| c() });
        loading.connect_active_notify({ let c = changed.clone(); move |_| c() });
        self.saver(move |t| t.overlay_key = key.text().trim().to_string());
        self.saver(move |t| t.panel_fps = fps.is_active());
        self.saver(move |t| t.loading_screen = loading.is_active());
    }

    fn build_advanced(self: &Rc<Self>, s: &Settings) {
        let changed = self.on_change();
        for (key, title, description) in [("fast_flags", "FastFlags", "One per line: Name=value"),
                                          ("extra_env", "Extra environment variables", "One per line: NAME=value")] {
            let group = adw::PreferencesGroup::builder().title(title).description(description).build();
            self.advanced.add(&group);
            let values = if key == "fast_flags" { &s.fast_flags } else { &s.extra_env };
            let (editor, buffer) = pairs_editor(values);
            group.add(&editor);
            buffer.connect_changed({ let c = changed.clone(); move |_| c() });
            self.saver(move |t| {
                // half-typed lines wait until they have an '='
                let (pairs, _bad) = read_pairs(&buffer);
                let mut map = Map::new();
                for (name, value) in pairs {
                    let value = if key == "fast_flags" { settings::parse_flag_value(&value) } else { Value::String(value) };
                    map.insert(name, value);
                }
                if key == "fast_flags" {
                    t.fast_flags = settings::clean_flags(Some(&Value::Object(map)));
                } else {
                    t.extra_env = settings::clean_env(Some(&Value::Object(map)));
                }
            });
        }
    }
}

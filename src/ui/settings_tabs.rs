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

    /// The everyday settings, most used first.
    fn build_general(self: &Rc<Self>, s: &Settings) {
        let page = &self.general;
        let changed = self.on_change();

        let group = adw::PreferencesGroup::builder()
            .title("In-game panel")
            .description("Applies to games started afterwards.")
            .build();
        page.add(&group);
        let panel = adw::SwitchRow::builder().title("Show the panel").active(s.panel).build();
        let key = adw::EntryRow::builder().title("Key (e.g. INSERT, SHIFT + F1)").text(&s.overlay_key).build();
        let blur = adw::SwitchRow::builder().title("Blur the game behind it").active(s.panel_blur).build();
        group.add(&panel);
        group.add(&key);
        group.add(&blur);
        panel.connect_active_notify({ let c = changed.clone(); move |_| c() });
        key.connect_changed({ let c = changed.clone(); move |_| c() });
        blur.connect_active_notify({ let c = changed.clone(); move |_| c() });
        self.saver(move |t| t.panel = panel.is_active());
        self.saver(move |t| t.overlay_key = key.text().trim().to_string());
        self.saver(move |t| t.panel_blur = blur.is_active());

        let group = adw::PreferencesGroup::builder().title("Frame rate").build();
        page.add(&group);
        let cap = spin("Frame-rate cap", s.fps_cap, 100000.0, "0 = unchanged");
        let refresh = spin("Refresh rate Roblox sees", s.refresh_rate, 100000.0, "0 = the real rate");
        let modes = pairs(&PRESENTATION_CHOICES);
        let mode = combo("Presentation mode", &modes, &s.presentation_mode, "");
        group.add(&cap);
        group.add(&refresh);
        group.add(&mode);
        cap.connect_value_notify({ let c = changed.clone(); move |_| c() });
        refresh.connect_value_notify({ let c = changed.clone(); move |_| c() });
        mode.connect_selected_notify({ let c = changed.clone(); move |_| c() });
        self.saver(move |t| t.fps_cap = cap.value() as u64);
        self.saver(move |t| t.refresh_rate = refresh.value() as u64);
        self.saver(move |t| t.presentation_mode = chosen(&mode, &modes));

        let group = adw::PreferencesGroup::builder().title("Background").build();
        page.add(&group);
        let background = spin("Background frame rate", s.background_fps, 1000.0, "0 = no limit");
        let afk = adw::SwitchRow::builder().title("Anti-AFK")
            .subtitle("Sends input to idle games. May break Roblox's rules and get accounts banned.")
            .active(s.anti_afk).build();
        group.add(&background);
        let tab_out = adw::SwitchRow::builder().title("Hide tabbing out").active(s.hide_tab_out).build();
        group.add(&afk);
        group.add(&tab_out);
        background.connect_value_notify({ let c = changed.clone(); move |_| c() });
        afk.connect_active_notify({ let c = changed.clone(); move |_| c() });
        tab_out.connect_active_notify({ let c = changed.clone(); move |_| c() });
        self.saver(move |t| t.background_fps = background.value() as u64);
        self.saver(move |t| t.anti_afk = afk.is_active());
        self.saver(move |t| t.hide_tab_out = tab_out.is_active());

        let group = adw::PreferencesGroup::builder().title("Controls").build();
        page.add(&group);
        let edit = adw::SwitchRow::builder().title("Ctrl for copy and paste")
            .subtitle("In text fields").active(s.control_edit_keys).build();
        let confine = adw::SwitchRow::builder().title("Keep the pointer in the window").active(s.confine_pointer).build();
        group.add(&edit);
        group.add(&confine);
        edit.connect_active_notify({ let c = changed.clone(); move |_| c() });
        confine.connect_active_notify({ let c = changed.clone(); move |_| c() });
        self.saver(move |t| t.control_edit_keys = edit.is_active());
        self.saver(move |t| t.confine_pointer = confine.is_active());

        let group = adw::PreferencesGroup::builder().title("Privacy").build();
        page.add(&group);
        let telemetry = adw::SwitchRow::builder().title("Block Roblox telemetry").active(s.block_telemetry).build();
        group.add(&telemetry);
        telemetry.connect_active_notify({ let c = changed.clone(); move |_| c() });
        self.saver(move |t| t.block_telemetry = telemetry.is_active());

        let group = adw::PreferencesGroup::builder().title("Starting").build();
        page.add(&group);
        let loading = adw::SwitchRow::builder().title("Loading window").active(s.loading_screen).build();
        group.add(&loading);
        loading.connect_active_notify({ let c = changed.clone(); move |_| c() });
        self.saver(move |t| t.loading_screen = loading.is_active());

        let group = adw::PreferencesGroup::builder().title("Discord").build();
        page.add(&group);
        let discord = adw::SwitchRow::builder().title("Show what you're playing").active(s.discord).build();
        let join = adw::SwitchRow::builder().title("Join button")
            .subtitle("Friends can join your server from your status").active(s.discord_join).sensitive(s.discord).build();
        group.add(&discord);
        group.add(&join);
        discord.bind_property("active", &join, "sensitive").build();
        discord.connect_active_notify({ let c = changed.clone(); move |_| c() });
        join.connect_active_notify({ let c = changed.clone(); move |_| c() });
        self.saver(move |t| t.discord = discord.is_active());
        self.saver(move |t| t.discord_join = join.is_active());

        // only the copy install.sh makes updates itself; a source build is updated with git
        if crate::upgrade::installed_copy().is_some() {
            let group = adw::PreferencesGroup::builder().title("Updates").description(format!("Mace {}", crate::VERSION)).build();
            page.add(&group);
            let check = adw::SwitchRow::builder().title("Check for updates").active(s.check_updates).build();
            group.add(&check);
            check.connect_active_notify({ let c = changed.clone(); move |_| c() });
            self.saver(move |t| t.check_updates = check.is_active());
        }
    }

    /// What few people need: how Roblox is run, and raw FastFlags and environment.
    fn build_advanced(self: &Rc<Self>, s: &Settings) {
        let page = &self.advanced;
        let changed = self.on_change();

        let group = adw::PreferencesGroup::builder().title("Performance").build();
        page.add(&group);
        let gamemode = adw::SwitchRow::builder().title("GameMode").active(s.gamemode).build();
        let mangohud = adw::SwitchRow::builder().title("MangoHud")
            .active(s.mangohud).build();
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

        let group = adw::PreferencesGroup::builder().title("Rosemary").build();
        page.add(&group);
        let runners = runner::butter_runners();
        let mut choices = vec![("auto".to_string(), "Newest".to_string())];
        choices.extend(runners.iter().map(|r| (r.path.to_string_lossy().into_owned(), r.name.clone())));
        if !choices.iter().any(|(v, _)| *v == s.rosemary) {
            choices.push((s.rosemary.clone(), s.rosemary.clone()));
        }
        let runner = combo("Runner", &choices, &s.rosemary, if runners.is_empty() { "No Butter runners found" } else { "" });
        let decorations = pairs(&[("server", "Your desktop's"), ("client", "Mac style")]);
        let menus = pairs(&[("none", "Hidden"), ("strip", "Under the title bar"), ("titlebar", "In the title bar")]);
        let deco = combo("Title bar", &decorations, &s.decorations, "");
        let menu = combo("Mac menu bar", &menus, &s.menu, "");
        group.add(&runner);
        group.add(&deco);
        group.add(&menu);
        runner.connect_selected_notify({ let c = changed.clone(); move |_| c() });
        deco.connect_selected_notify({ let c = changed.clone(); move |_| c() });
        menu.connect_selected_notify({ let c = changed.clone(); move |_| c() });
        self.saver(move |t| t.rosemary = chosen(&runner, &choices));
        self.saver(move |t| t.decorations = chosen(&deco, &decorations));
        self.saver(move |t| t.menu = chosen(&menu, &menus));

        for (key, title, description) in [("fast_flags", "FastFlags", "One per line: Name=value"),
                                          ("extra_env", "Environment variables", "One per line: NAME=value")] {
            let group = adw::PreferencesGroup::builder().title(title).description(description).build();
            page.add(&group);
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

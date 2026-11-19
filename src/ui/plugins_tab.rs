//! The Plugins tab: the plugins in the plugins folder, each turned on here
//! (after saying what it may do), and a start for making your own. Games
//! already running pick up changes within a second.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use crate::plugins::{self, Found};

const GUIDE: &str = "https://github.com/d29l/mace/blob/main/docs/plugins.md";

pub struct PluginsTab {
    pub page: adw::PreferencesPage,
    group: adw::PreferencesGroup,
    rows: RefCell<Vec<gtk::Widget>>,
    /// A switch is being set by Mace, not clicked.
    quiet: Rc<Cell<bool>>,
    toast: Box<dyn Fn(&str)>,
}

/// "My Cool Plugin" → "my-cool-plugin".
fn id_from(name: &str) -> String {
    let mut id = String::new();
    for c in name.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            id.push(c);
        } else if !id.ends_with('-') && !id.is_empty() {
            id.push('-');
        }
    }
    id.trim_end_matches('-').chars().take(48).collect()
}

fn open_folder(widget: &impl IsA<gtk::Widget>) {
    let _ = std::fs::create_dir_all(plugins::dir());
    let window = widget.root().and_downcast::<gtk::Window>();
    gtk::FileLauncher::new(Some(&gio::File::for_path(plugins::dir())))
        .launch(window.as_ref(), gio::Cancellable::NONE, |_| {});
}

impl PluginsTab {
    pub fn new(toast: impl Fn(&str) + 'static) -> Rc<PluginsTab> {
        let page = adw::PreferencesPage::builder().title("Plugins").icon_name("application-x-addon-symbolic").build();
        let new = gtk::Button::builder()
            .child(&adw::ButtonContent::builder().icon_name("list-add-symbolic").label("New").build())
            .css_classes(["flat"]).tooltip_text("Make a plugin").build();
        let open = gtk::Button::builder().icon_name("folder-open-symbolic").css_classes(["flat"])
            .tooltip_text("Open the plugins folder").build();
        let suffix = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        suffix.append(&open);
        suffix.append(&new);
        let group = adw::PreferencesGroup::builder().title("Plugins")
            .description("Buttons and menus in the in-game bar.").header_suffix(&suffix).build();
        page.add(&group);
        let help = adw::PreferencesGroup::new();
        let guide = adw::ActionRow::builder().title("Making plugins").subtitle("In Luau, like Roblox scripts")
            .activatable(true).build();
        guide.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
        help.add(&guide);
        page.add(&help);

        let me = Rc::new(PluginsTab { page, group, rows: RefCell::new(Vec::new()), quiet: Rc::new(Cell::new(false)),
                                      toast: Box::new(toast) });
        open.connect_clicked(open_folder);
        guide.connect_activated(|row| {
            let window = row.root().and_downcast::<gtk::Window>();
            gtk::UriLauncher::new(GUIDE).launch(window.as_ref(), gio::Cancellable::NONE, |_| {});
        });
        let weak = Rc::downgrade(&me);
        new.connect_clicked(move |_| {
            if let Some(me) = weak.upgrade() {
                me.ask_new();
            }
        });
        // plugins may have been added in the file manager meanwhile
        let weak = Rc::downgrade(&me);
        me.page.connect_map(move |_| {
            if let Some(me) = weak.upgrade() {
                me.refresh();
            }
        });
        me.refresh();
        me
    }

    fn refresh(self: &Rc<Self>) {
        for row in self.rows.borrow_mut().drain(..) {
            self.group.remove(&row);
        }
        let found = plugins::list();
        if found.is_empty() {
            let row = adw::ActionRow::builder().title("No plugins yet")
                .subtitle("Put one in the plugins folder, or make one").build();
            self.group.add(&row);
            self.rows.borrow_mut().push(row.upcast());
        }
        for plugin in found {
            let row = self.row(plugin);
            self.group.add(&row);
            self.rows.borrow_mut().push(row.upcast());
        }
    }

    fn row(self: &Rc<Self>, plugin: Found) -> adw::ActionRow {
        let row = adw::ActionRow::builder().title(plugin.name()).subtitle_lines(3).build();
        let switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
        row.add_suffix(&switch);
        row.set_activatable_widget(Some(&switch));
        match &plugin.manifest {
            Ok(manifest) => {
                let about = [manifest.version.clone(), manifest.author.clone()].into_iter()
                    .filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                // turned on before it named these: they wait until it's turned on again
                let entry = crate::settings::load().plugins.get(&plugin.id).cloned();
                let agreed = plugins::agreed_sites(entry.as_ref());
                let waiting: Vec<&str> = manifest.http.iter().filter(|s| !agreed.contains(s)).map(String::as_str).collect();
                let waiting = if plugins::entry_is_on(entry.as_ref()) && !waiting.is_empty() {
                    format!("Turn it off and on to let it reach {}", waiting.join(", "))
                } else {
                    String::new()
                };
                let lines = [manifest.description.clone(), about, waiting].into_iter().filter(|s| !s.is_empty())
                    .collect::<Vec<_>>().join("\n");
                row.set_subtitle(&glib_escape(&lines));
                self.quiet.set(true);
                switch.set_active(plugins::is_on(&plugin.id));
                self.quiet.set(false);
            }
            Err(why) => {
                row.set_subtitle(&glib_escape(why));
                row.add_css_class("error");
                switch.set_sensitive(false);
            }
        }
        let weak = Rc::downgrade(self);
        switch.connect_active_notify(move |switch| {
            let Some(me) = weak.upgrade() else { return };
            if me.quiet.get() {
                return;
            }
            let Ok(manifest) = &plugin.manifest else { return };
            if !switch.is_active() {
                if let Err(error) = plugins::set_on(&plugin.id, false) {
                    (me.toast)(&format!("Couldn't turn it off: {error:#}"));
                }
                return;
            }
            // on only once agreed to
            me.quiet.set(true);
            switch.set_active(false);
            me.quiet.set(false);
            me.ask_on(&plugin.id, manifest, switch);
        });
        row
    }

    fn ask_on(self: &Rc<Self>, id: &str, manifest: &plugins::Manifest, switch: &gtk::Switch) {
        let may = plugins::describe_permissions(manifest);
        let mut body = if may.is_empty() {
            "It can add a button and a menu to the in-game bar, and show notes.".to_string()
        } else {
            format!("It may:\n{}", may.iter().map(|m| format!("• {m}")).collect::<Vec<_>>().join("\n"))
        };
        if !manifest.http.is_empty() {
            body.push_str(&format!("\n\nIt can reach the internet, at:\n{}\nThose sites see your IP address and \
                                    whatever the plugin sends them.", plugins::describe_sites(manifest).join("\n")));
        }
        body.push_str("\n\nPlugins are programs. Only turn on ones you trust.");
        let dialog = adw::AlertDialog::new(Some(&format!("Turn on {}?", manifest.name)), Some(&body));
        dialog.add_responses(&[("cancel", "Cancel"), ("on", "Turn On")]);
        dialog.set_response_appearance("on", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("on"));
        dialog.set_close_response("cancel");
        let (id, weak, switch) = (id.to_string(), Rc::downgrade(self), switch.clone());
        dialog.connect_response(None, move |_, answer| {
            let Some(me) = weak.upgrade() else { return };
            if answer != "on" {
                return;
            }
            match plugins::set_on(&id, true) {
                Ok(()) => {
                    me.quiet.set(true);
                    switch.set_active(true);
                    me.quiet.set(false);
                }
                Err(error) => (me.toast)(&format!("Couldn't turn it on: {error:#}")),
            }
        });
        dialog.present(self.page.root().as_ref());
    }

    fn ask_new(self: &Rc<Self>) {
        let dialog = adw::AlertDialog::new(Some("New plugin"), Some("It starts as a button with a menu."));
        let entry = gtk::Entry::builder().placeholder_text("Name").activates_default(true).build();
        dialog.set_extra_child(Some(&entry));
        dialog.add_responses(&[("cancel", "Cancel"), ("make", "Make")]);
        dialog.set_response_appearance("make", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("make"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, answer| {
            let Some(me) = weak.upgrade() else { return };
            if answer != "make" {
                return;
            }
            let id = id_from(&entry.text());
            if id.is_empty() {
                (me.toast)("Give it a name");
                return;
            }
            match plugins::create(&id) {
                Ok(_) => {
                    me.refresh();
                    (me.toast)(&format!("Made {id}: edit its init.luau, then turn it on"));
                    open_folder(&me.page);
                }
                Err(error) => (me.toast)(&format!("{error:#}")),
            }
        });
        dialog.present(self.page.root().as_ref());
    }
}

/// Text as Pango markup reads it (row subtitles are markup).
fn glib_escape(text: &str) -> String {
    gtk::glib::markup_escape_text(text).to_string()
}

#[cfg(test)]
mod tests {
    use super::id_from;

    #[test]
    fn names_become_ids() {
        assert_eq!(id_from("My Cool Plugin"), "my-cool-plugin");
        assert_eq!(id_from("  Server hop!! 2 "), "server-hop-2");
        assert_eq!(id_from("../x"), "x");
        assert_eq!(id_from("!!!"), "");
    }
}

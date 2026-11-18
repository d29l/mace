//! The main window: the accounts, what each is playing and buttons to start,
//! stop and join servers, beside the settings tabs.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, glib};
use serde_json::{Map, Value};

use super::mods_tab::ModsTab;
use super::settings_tabs::GlobalSettings;
use super::widgets::{background, markup};
use super::account_settings;
use crate::accounts::{self, Account, Identity};
use crate::servers::{self, LogFollower, unix_time};
use crate::{APP_ID, builds, doctor, launch, processes, supervisor};

const REFRESH_SECONDS: u32 = 2;

/// What the worker thread finds out each refresh.
struct Snapshot {
    live: HashMap<String, Vec<i32>>,
    servers: HashMap<String, Map<String, Value>>,
    launching: HashMap<String, Map<String, Value>>,
    identities: HashMap<String, Identity>,
    builds: HashMap<String, String>,
}

pub struct Window {
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    group: adw::PreferencesGroup,
    stack: gtk::Stack,
    settings: Rc<GlobalSettings>,
    _mods: Rc<ModsTab>,
    rows: RefCell<Vec<Rc<AccountRow>>>,
    followers: Arc<Mutex<HashMap<String, LogFollower>>>,
    busy: Cell<bool>,
}

impl Window {
    pub fn new(app: &adw::Application) -> adw::ApplicationWindow {
        let window = adw::ApplicationWindow::builder().application(app).title("Mace").default_width(620).default_height(640).build();
        // tabs: the accounts, then the settings every account shares
        let tabs = adw::ViewStack::new();
        let switcher = adw::ViewSwitcher::builder().stack(&tabs).policy(adw::ViewSwitcherPolicy::Wide).build();
        let header = adw::HeaderBar::builder().title_widget(&switcher).build();

        let add = gtk::Button::builder()
            .child(&adw::ButtonContent::builder().icon_name("list-add-symbolic").label("Add").build())
            .css_classes(["flat"])
            .tooltip_text("Add an account")
            .build();
        let group = adw::PreferencesGroup::builder()
            .title("Accounts")
            .header_suffix(&add)
            .build();
        let empty_add = gtk::Button::builder().label("Add an account").halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"]).build();
        let empty = adw::StatusPage::builder().icon_name(APP_ID).title("No accounts yet")
            .description("Add one, then sign in to Roblox.").child(&empty_add).build();
        let page = adw::PreferencesPage::new();
        page.add(&group);
        let stack = gtk::Stack::new();
        stack.add_named(&page, Some("accounts"));
        stack.add_named(&empty, Some("empty"));
        tabs.add_titled_with_icon(&stack, Some("accounts"), "Accounts", "system-users-symbolic");
        let settings = GlobalSettings::new();
        let toasts = adw::ToastOverlay::new();
        tabs.add_titled_with_icon(&settings.general, Some("general"), "General", "emblem-system-symbolic");
        let mods = ModsTab::new({
            let toasts = toasts.clone();
            move |text| toasts.add_toast(adw::Toast::new(text))
        });
        tabs.add_titled_with_icon(&mods.page, Some("mods"), "Mods", "application-x-addon-symbolic");
        tabs.add_titled_with_icon(&settings.advanced, Some("advanced"), "Advanced", "preferences-other-symbolic");

        toasts.set_child(Some(&tabs));
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&toasts));
        window.set_content(Some(&view));

        let me = Rc::new(Window { window: window.clone(), toasts, group, stack, settings, _mods: mods, rows: RefCell::new(Vec::new()),
                                  followers: Arc::new(Mutex::new(HashMap::new())), busy: Cell::new(false) });
        for button in [&add, &empty_add] {
            let weak = Rc::downgrade(&me);
            button.connect_clicked(move |_| {
                if let Some(me) = weak.upgrade() {
                    me.ask_add();
                }
            });
        }
        let weak = Rc::downgrade(&me);
        window.connect_close_request(move |_| {
            if let Some(me) = weak.upgrade() {
                me.settings.flush();
            }
            glib::Propagation::Proceed
        });
        me.reload();
        let weak = Rc::downgrade(&me);
        glib::timeout_add_seconds_local(REFRESH_SECONDS, move || match weak.upgrade() {
            Some(me) => {
                me.refresh();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        // the window keeps `me` alive through its handlers' strong reference here
        unsafe { window.set_data("mace-window", me) };
        window
    }

    pub fn toast(&self, text: &str) {
        let toast = adw::Toast::new(&markup(text));
        toast.set_timeout(3);
        self.toasts.add_toast(toast);
    }

    pub fn error(self: &Rc<Self>, error: &anyhow::Error) {
        let text = format!("{error:#}");
        self.toast(text.trim().lines().last().unwrap_or("Something went wrong"));
        self.refresh();
    }

    fn reload(self: &Rc<Self>) {
        for row in self.rows.borrow().iter() {
            self.group.remove(&row.row);
        }
        let rows: Vec<Rc<AccountRow>> = accounts::accounts().into_iter().map(|a| AccountRow::new(self, a)).collect();
        for row in &rows {
            self.group.add(&row.row);
        }
        self.stack.set_visible_child_name(if rows.is_empty() { "empty" } else { "accounts" });
        *self.rows.borrow_mut() = rows;
        self.refresh();
    }

    fn refresh_soon(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(1500), move || {
            if let Some(me) = weak.upgrade() {
                me.refresh();
            }
        });
    }

    fn refresh(self: &Rc<Self>) {
        if self.busy.replace(true) {
            return;
        }
        let ids: Vec<Account> = self.rows.borrow().iter().map(|r| r.account.clone()).collect();
        let followers = self.followers.clone();
        let weak = Rc::downgrade(self);
        // off the main loop: what runs, the servers from the logs, the launches
        background(move || collect(&ids, &followers), move |snapshot| {
            if let Some(me) = weak.upgrade() {
                me.busy.set(false);
                me.apply(&snapshot);
            }
        });
    }

    fn apply(&self, snapshot: &Snapshot) {
        let rows = self.rows.borrow();
        for row in rows.iter() {
            *row.who.borrow_mut() = snapshot.identities.get(&row.account.id).cloned().unwrap_or_default();
        }
        for row in rows.iter() {
            let who = row.who.borrow().clone();
            let twin = rows.iter().find(|r| !Rc::ptr_eq(r, row) && !who.is_empty() && r.who.borrow().id == who.id)
                .map(|r| r.account.name_with(&r.who.borrow()));
            row.refresh(snapshot, twin);
        }
    }

    fn ask_add(self: &Rc<Self>) {
        let entry = gtk::Entry::builder().placeholder_text("Name (optional)").activates_default(true).build();
        let dialog = adw::AlertDialog::new(Some("Add an account"),
            Some("Roblox opens so you can sign in."));
        dialog.set_extra_child(Some(&entry));
        dialog.add_responses(&[("cancel", "Cancel"), ("add", "Add and sign in")]);
        dialog.set_response_appearance("add", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("add"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, answer| {
            let Some(me) = weak.upgrade() else { return };
            if answer != "add" {
                return;
            }
            me.toast("Setting up…");
            let label = entry.text().to_string();
            let weak = Rc::downgrade(&me);
            background(move || accounts::add(&label).map_err(|e| e.to_string()), move |result| {
                let Some(me) = weak.upgrade() else { return };
                match result {
                    Ok(account) => {
                        me.reload();
                        me.toast(&format!("Sign in to {} in Roblox", account.name()));
                        if let Some(row) = me.rows.borrow().iter().find(|r| r.account.id == account.id) {
                            row.run_start(None);
                        }
                    }
                    Err(error) => me.error(&anyhow::anyhow!(error)),
                }
            });
        });
        dialog.present(Some(&self.window));
    }
}

fn collect(accounts: &[Account], followers: &Arc<Mutex<HashMap<String, LogFollower>>>) -> Snapshot {
    let live = processes::running(accounts, None);
    let mut found = HashMap::new();
    let mut followers = followers.lock().unwrap();
    followers.retain(|id, _| live.contains_key(id));
    for account in accounts {
        if !live.contains_key(&account.id) {
            continue;
        }
        let follower = followers.entry(account.id.clone()).or_insert_with(|| LogFollower::new(&account.logs()));
        follower.poll();
        if follower.server.contains_key("ip") {
            found.insert(account.id.clone(), servers::describe(&follower.server));
        }
    }
    Snapshot {
        live,
        servers: found,
        launching: accounts.iter().map(|a| (a.id.clone(), supervisor::read_status(&a.id))).collect(),
        identities: accounts.iter().map(|a| (a.id.clone(), a.identity())).collect(),
        builds: accounts.iter()
            .filter_map(|a| builds::installed(a).map(|b| (a.id.clone(), format!("Roblox {} · channel {}", b.version, b.channel))))
            .collect(),
    }
}

// ---------------------------------------------------------------- an account

struct AccountRow {
    row: adw::ActionRow,
    account: Account,
    window: Weak<Window>,
    avatar: adw::Avatar,
    start: adw::SplitButton,
    join_box: gtk::Box,
    who: RefCell<Identity>,
    server: RefCell<Map<String, Value>>,
    running: Cell<bool>,
    avatar_for: RefCell<String>,
    starting: Cell<bool>,
}

fn str_of<'a>(map: &'a Map<String, Value>, key: &str) -> &'a str {
    map.get(key).and_then(Value::as_str).unwrap_or("")
}

impl AccountRow {
    fn new(window: &Rc<Window>, account: Account) -> Rc<AccountRow> {
        let row = adw::ActionRow::new();
        let avatar = adw::Avatar::new(40, None, true);
        row.add_prefix(&avatar);
        // click: start or stop; the arrow: start in (or move to) another account's server
        let join_box = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2)
            .margin_top(4).margin_bottom(4).margin_start(4).margin_end(4).build();
        let popover = gtk::Popover::builder().child(&join_box).build();
        let start = adw::SplitButton::builder().valign(gtk::Align::Center).popover(&popover)
            .dropdown_tooltip("Join another account's server").build();
        let more = gtk::MenuButton::builder().icon_name("view-more-symbolic").valign(gtk::Align::Center)
            .css_classes(["flat"]).tooltip_text("More").build();
        row.add_suffix(&start);
        row.add_suffix(&more);
        let me = Rc::new(AccountRow {
            row, account, window: Rc::downgrade(window), avatar, start: start.clone(), join_box,
            who: RefCell::new(Identity::default()), server: RefCell::new(Map::new()), running: Cell::new(false),
            avatar_for: RefCell::new(String::new()), starting: Cell::new(false),
        });
        let weak = Rc::downgrade(&me);
        popover.connect_show(move |_| {
            if let Some(me) = weak.upgrade() {
                me.fill_join_menu();
            }
        });
        let weak = Rc::downgrade(&me);
        start.connect_clicked(move |_| {
            if let Some(me) = weak.upgrade() {
                me.on_start();
            }
        });
        let weak = Rc::downgrade(&me);
        more.set_create_popup_func(move |button| {
            if let Some(me) = weak.upgrade() {
                me.fill_more_menu(button);
            }
        });
        me
    }

    fn name(&self) -> String {
        self.account.name_with(&self.who.borrow())
    }

    fn refresh(self: &Rc<Self>, snapshot: &Snapshot, twin: Option<String>) {
        let running = snapshot.live.contains_key(&self.account.id);
        self.running.set(running);
        *self.server.borrow_mut() = if running { snapshot.servers.get(&self.account.id).cloned().unwrap_or_default() } else { Map::new() };
        let who = self.who.borrow().clone();
        let name = self.name();
        self.row.set_title(&markup(&name));
        self.avatar.set_text(Some(&name));
        let mut bits: Vec<String> = Vec::new();
        match (&twin, who.username.is_empty()) {
            (Some(other), false) => bits.push(format!("signed in as @{}, same as {other}", who.username)),
            (None, false) => bits.push(format!("@{}", who.username)),
            (_, true) if self.account.prefix.exists() => bits.push("not signed in".into()),
            _ => {}
        }
        let launching = snapshot.launching.get(&self.account.id).cloned().unwrap_or_default();
        let state = str_of(&launching, "state");
        let mut progress = String::new();
        if ["preparing", "downloading", "starting", "updating"].contains(&state) {
            let message = str_of(&launching, "message");
            let message = if message.is_empty() { state } else { message }.trim_end_matches('.');
            progress = match launching.get("progress").and_then(Value::as_f64) {
                Some(share) => format!("{message}… {}%", (share * 100.0) as u32),
                None => format!("{message}…"),
            };
        } else if state == "failed" && unix_time() - launching.get("started").and_then(Value::as_f64).unwrap_or(0.0) < 120.0 {
            let message = str_of(&launching, "message");
            progress = format!("couldn't start: {}", if message.is_empty() { "see Why didn't it start?" } else { message });
        }
        let server = self.server.borrow();
        if !progress.is_empty() {
            bits.push(progress.clone());
        } else if !running {
            bits.push("not running".into());
        } else if server.contains_key("ip") {
            let game = server.get("name").and_then(Value::as_str).unwrap_or("a game");
            let place = server.get("location").and_then(Value::as_str);
            bits.push(match place { Some(p) => format!("in {game} · {p}"), None => format!("in {game}") });
        } else {
            bits.push("running".into());
        }
        self.row.set_subtitle(&markup(&bits.join(" · ")));
        self.row.set_tooltip_text(snapshot.builds.get(&self.account.id).map(String::as_str));
        self.start.set_icon_name(if running { "media-playback-stop-symbolic" } else { "media-playback-start-symbolic" });
        self.start.set_tooltip_text(Some(if running { "Stop" } else { "Start" }));
        if progress.is_empty() && !self.starting.get() {
            self.start.set_sensitive(true);
        }
        if !who.id.is_empty() && *self.avatar_for.borrow() != who.id {
            *self.avatar_for.borrow_mut() = who.id.clone();
            let weak = Rc::downgrade(self);
            let id = who.id.clone();
            background(move || servers::avatar(&id), move |path| {
                if let (Some(me), Some(path)) = (weak.upgrade(), path) {
                    if let Ok(texture) = gdk::Texture::from_filename(path) {
                        me.avatar.set_custom_image(Some(&texture));
                    }
                }
            });
        }
    }

    fn window(&self) -> Option<Rc<Window>> {
        self.window.upgrade()
    }

    fn run_start(self: &Rc<Self>, link: Option<String>) {
        self.start.set_sensitive(false);
        self.starting.set(true);
        let account = self.account.clone();
        let weak = Rc::downgrade(self);
        background(move || launch::start(&account, link.as_deref(), false, true).map_err(|e| e.to_string()), move |result| {
            let Some(me) = weak.upgrade() else { return };
            me.starting.set(false);
            let Some(window) = me.window() else { return };
            match result {
                Ok(_) => window.refresh_soon(),
                Err(error) => window.error(&anyhow::anyhow!(error)),
            }
        });
    }

    fn on_start(self: &Rc<Self>) {
        let Some(window) = self.window() else { return };
        let name = self.name();
        if self.running.get() {
            self.start.set_sensitive(false);
            window.toast(&format!("Stopping {name}…"));
            let account = self.account.clone();
            let weak = Rc::downgrade(&window);
            background(move || processes::stop(&account, &accounts::accounts(), Duration::from_secs(2)), move |_| {
                if let Some(window) = weak.upgrade() {
                    window.refresh();
                }
            });
        } else {
            window.toast(&if self.who.borrow().is_empty() { format!("Starting {name}: sign in to Roblox") }
                          else { format!("Starting {name}…") });
            self.run_start(None);
        }
    }

    fn join(self: &Rc<Self>, link: String, label: &str) {
        if let Some(window) = self.window() {
            window.toast(&format!("{}: joining {label}…", self.name()));
        }
        self.run_start(Some(link));
    }

    fn gather(self: &Rc<Self>) {
        let Some(window) = self.window() else { return };
        window.toast(&format!("Bringing everyone to {}'s server…", self.name()));
        let account = self.account.clone();
        let weak = Rc::downgrade(&window);
        background(move || launch::gather(&account).map_err(|e| e.to_string()), move |result| {
            if let Some(window) = weak.upgrade() {
                match result {
                    Ok(_) => window.refresh_soon(),
                    Err(error) => window.error(&anyhow::anyhow!(error)),
                }
            }
        });
    }

    fn item(container: &gtk::Box, text: &str, subtitle: &str, destructive: bool, callback: impl Fn() + 'static) {
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        content.append(&gtk::Label::builder().label(text).xalign(0.0).build());
        if !subtitle.is_empty() {
            content.append(&gtk::Label::builder().label(subtitle).xalign(0.0).css_classes(["dim-label", "caption"]).build());
        }
        let button = gtk::Button::builder().child(&content).css_classes(["flat"]).build();
        if destructive {
            button.add_css_class("error");
        }
        let at = container.clone();
        button.connect_clicked(move |_| {
            if let Some(popover) = at.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>() {
                popover.popdown();
            }
            callback();
        });
        container.append(&button);
    }

    fn fill_join_menu(self: &Rc<Self>) {
        let container = &self.join_box;
        while let Some(child) = container.first_child() {
            container.remove(&child);
        }
        let Some(window) = self.window() else { return };
        let verb = if self.running.get() { "Join" } else { "Start in" };
        let rows = window.rows.borrow();
        if self.server.borrow().contains_key("ip") && rows.len() > 1 {
            let weak = Rc::downgrade(self);
            Self::item(container, "Bring everyone here", "All your accounts", false, move || {
                if let Some(me) = weak.upgrade() {
                    me.gather();
                }
            });
            container.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        }
        let mut any = false;
        for row in rows.iter() {
            if Rc::ptr_eq(row, self) || !row.server.borrow().contains_key("ip") {
                continue;
            }
            any = true;
            let server = row.server.borrow().clone();
            let who = row.name();
            let game = server.get("name").and_then(Value::as_str).unwrap_or("their game").to_string();
            let subtitle = match server.get("location").and_then(Value::as_str) {
                Some(place) => format!("{game} · {place}"),
                None => game,
            };
            let link = str_of(&server, "link").to_string();
            let weak = Rc::downgrade(self);
            Self::item(container, &format!("{verb} {who}'s server"), &subtitle, false, move || {
                if let Some(me) = weak.upgrade() {
                    me.join(link.clone(), &format!("{who}'s server"));
                }
            });
        }
        if !any {
            container.append(&gtk::Label::builder().label("No other account is in a server").css_classes(["dim-label"])
                .margin_top(6).margin_bottom(6).margin_start(8).margin_end(8).build());
        }
        container.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let weak = Rc::downgrade(self);
        Self::item(container, "Join by link or job ID…", "", false, move || {
            if let Some(me) = weak.upgrade() {
                me.ask_link();
            }
        });
    }

    fn fill_more_menu(self: &Rc<Self>, button: &gtk::MenuButton) {
        let container = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2)
            .margin_top(4).margin_bottom(4).margin_start(4).margin_end(4).build();
        button.set_popover(Some(&gtk::Popover::builder().child(&container).build()));
        let server = self.server.borrow().clone();
        if server.contains_key("link") {
            for (label, key, what) in [("Copy join link", "link", "join link"), ("Copy job ID", "job", "job ID")] {
                let (text, weak) = (str_of(&server, key).to_string(), Rc::downgrade(self));
                Self::item(&container, label, "", false, move || {
                    if let Some(me) = weak.upgrade() {
                        me.row.clipboard().set_text(&text);
                        if let Some(window) = me.window() {
                            window.toast(&format!("Copied the {what}"));
                        }
                    }
                });
            }
        }
        let weak = Rc::downgrade(self);
        Self::item(&container, "Settings…", "", false, move || {
            let Some(me) = weak.upgrade() else { return };
            let Some(window) = me.window() else { return };
            let toast_to = Rc::downgrade(&window);
            account_settings::present(&window.window, &me.account, move |message| {
                if let Some(window) = toast_to.upgrade() {
                    window.toast(&message);
                }
            });
        });
        let weak = Rc::downgrade(self);
        Self::item(&container, "Why didn't it start?", "", false, move || {
            if let Some(me) = weak.upgrade() {
                me.show_why();
            }
        });
        let weak = Rc::downgrade(self);
        Self::item(&container, "Rename…", "", false, move || {
            if let Some(me) = weak.upgrade() {
                me.ask_rename();
            }
        });
        let weak = Rc::downgrade(self);
        Self::item(&container, "Remove account…", "", true, move || {
            if let Some(me) = weak.upgrade() {
                me.ask_remove();
            }
        });
    }

    fn show_why(&self) {
        let Some(window) = self.window() else { return };
        let found = doctor::diagnose(&self.account, self.running.get());
        let dialog = adw::AlertDialog::new(Some(&format!("{}'s last start", self.name())), Some(&found.join("\n\n")));
        dialog.add_response("ok", "OK");
        dialog.present(Some(&window.window));
    }

    fn ask_link(self: &Rc<Self>) {
        let Some(window) = self.window() else { return };
        let entry = gtk::Entry::builder().placeholder_text("Link, or PLACEID JOBID").activates_default(true).build();
        let dialog = adw::AlertDialog::new(Some(&format!("Join with {}", self.name())),
                                           None);
        dialog.set_extra_child(Some(&entry));
        dialog.add_responses(&[("cancel", "Cancel"), ("join", "Join")]);
        dialog.set_response_appearance("join", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("join"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, answer| {
            let Some(me) = weak.upgrade() else { return };
            if answer != "join" {
                return;
            }
            let text = entry.text().to_string();
            match crate::links::join_url(&text) {
                Ok(_) => me.join(text, "the server"),
                Err(error) => {
                    if let Some(window) = me.window() {
                        window.error(&error);
                    }
                }
            }
        });
        dialog.present(Some(&window.window));
    }

    fn ask_rename(self: &Rc<Self>) {
        let Some(window) = self.window() else { return };
        let who = self.who.borrow().clone();
        let entry = gtk::Entry::builder().text(&self.account.label).activates_default(true)
            .placeholder_text(if who.display.is_empty() { "Name" } else { &who.display }).build();
        let dialog = adw::AlertDialog::new(Some("Rename account"), Some("Empty uses the Roblox display name."));
        dialog.set_extra_child(Some(&entry));
        dialog.add_responses(&[("cancel", "Cancel"), ("save", "Save")]);
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");
        let (account, weak) = (self.account.clone(), Rc::downgrade(&window));
        dialog.connect_response(None, move |_, answer| {
            if answer == "save" {
                let _ = accounts::rename(&account, &entry.text());
                if let Some(window) = weak.upgrade() {
                    window.reload();
                }
            }
        });
        dialog.present(Some(&window.window));
    }

    fn ask_remove(self: &Rc<Self>) {
        let Some(window) = self.window() else { return };
        let name = self.name();
        let body = if self.account.owned {
            "Deletes its prefix, login, settings and logs. The Roblox account isn't affected.".to_string()
        } else {
            format!("Removes it from Mace. Its prefix ({}) wasn't made by Mace and is kept.",
                    self.account.prefix.display())
        };
        let dialog = adw::AlertDialog::new(Some(&format!("Remove {name}?")), Some(&body));
        dialog.add_responses(&[("cancel", "Cancel"), ("remove", "Remove")]);
        dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        dialog.set_close_response("cancel");
        let (account, weak) = (self.account.clone(), Rc::downgrade(&window));
        dialog.connect_response(None, move |_, answer| {
            if answer != "remove" {
                return;
            }
            let (account, name, weak) = (account.clone(), name.clone(), weak.clone());
            background(move || {
                if processes::running(&accounts::accounts(), None).contains_key(&account.id) {
                    return Err(format!("{name} is running; stop it first"));
                }
                accounts::forget(&account).map_err(|e| e.to_string())
            }, move |result| {
                if let Some(window) = weak.upgrade() {
                    match result {
                        Ok(()) => window.reload(),
                        Err(error) => window.error(&anyhow::anyhow!(error)),
                    }
                }
            });
        });
        dialog.present(Some(&window.window));
    }
}

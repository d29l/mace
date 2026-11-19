//! The panel: a bar floating at the top of the game (who you're playing as,
//! this server, then icon buttons: rejoin and copy the link; servers, recent
//! games and alts; Mace Settings) with drop-downs under it, and toasts.
//! Everything shown comes from Mace; every button is a request to Mace (or a
//! copy to the clipboard, done right here).
//!
//! Layout follows the usual rules for toolbars: an 8-point grid, icon buttons
//! with tooltips, evenly spaced, settings at the trailing end, 40-point
//! targets, inner corners following the outer ones, and no borders (a soft
//! shadow lifts things off the game).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use egui::{Align2, Color32, CornerRadius, FontFamily, FontId, Rect, Response, Sense, Ui as EUi, pos2, vec2};
use mace_proto::{Account, Config, Game, Menu, Node, Pin, PluginButton, PublicServer, ServerPage, Sort, State, Theme, ToMace};

use crate::client::{self, Avatar};

/// Where the bar sits: its top edge and height (points).
pub const BAR_TOP: f32 = 16.0;
pub const BAR_HEIGHT: f32 = 64.0;
const BAR_WIDEST: f32 = 920.0;
const BAR_RADIUS: u8 = 24;
/// Space between the bar's edge and what's in it; with 40-point controls in a
/// 64-point bar it's the same all round.
const INSET: f32 = 12.0;
const CONTROL: f32 = 40.0;
/// Space between the bar's buttons, the same everywhere.
const BUTTON_GAP: f32 = 4.0;
/// How long a toast stays, in seconds.
const TOAST_SECONDS: f32 = 4.5;

pub enum Action {
    Close,
    Copy(String),
    Send(ToMace),
}

/// What Mace has told the panel, as of this frame.
pub struct Snapshot {
    pub connected: bool,
    pub config: Option<Config>,
    pub state: Option<Arc<State>>,
    pub avatars: HashMap<String, Avatar>,
    pub pages: Vec<ServerPage>,
    pub done: Vec<(u64, bool, String)>,
    pub toasts: Vec<(String, String)>,
    pub plugins: Vec<PluginButton>,
    pub menus: HashMap<(String, String), (Arc<Menu>, std::time::Instant)>,
    pub copies: Vec<String>,
    pub close: bool,
}

impl Snapshot {
    pub fn take() -> Snapshot {
        client::with(|d| Snapshot {
            connected: d.connected,
            config: d.config.clone(),
            state: d.state.clone(),
            avatars: d.avatars.clone(),
            pages: std::mem::take(&mut d.pages),
            done: std::mem::take(&mut d.done),
            toasts: std::mem::take(&mut d.toasts),
            plugins: d.plugins.clone(),
            menus: d.menus.clone(),
            copies: std::mem::take(&mut d.copies),
            close: std::mem::take(&mut d.close),
        })
    }

    fn me(&self) -> Option<Account> {
        let me = &self.config.as_ref()?.me;
        self.state.as_ref()?.accounts.iter().find(|a| &a.id == me).cloned()
    }
}

pub fn setup(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::empty();
    fonts.font_data.insert("regular".into(),
        Arc::new(egui::FontData::from_static(include_bytes!("../fonts/NotoSans-Regular.ttf"))));
    fonts.font_data.insert("bold".into(),
        Arc::new(egui::FontData::from_static(include_bytes!("../fonts/NotoSans-Bold.ttf"))));
    fonts.families.insert(FontFamily::Proportional, vec!["regular".into()]);
    fonts.families.insert(FontFamily::Monospace, vec!["regular".into()]);
    fonts.families.insert(FontFamily::Name("bold".into()), vec!["bold".into(), "regular".into()]);
    ctx.set_fonts(fonts);
    ctx.all_styles_mut(|style| {
        style.visuals = egui::Visuals::dark();
        style.visuals.window_shadow = egui::Shadow::NONE;
        style.visuals.popup_shadow = egui::Shadow::NONE;
        style.spacing.item_spacing = vec2(8.0, 8.0);
        style.spacing.scroll.bar_width = 4.0;
        style.spacing.scroll.floating = true;
        // tooltips come up quickly: the buttons are icons, and they say what each does
        style.interaction.tooltip_delay = 0.15;
    });
}

fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

fn color(hex: &str) -> Color32 {
    let hex = hex.trim_start_matches('#');
    let v = u32::from_str_radix(hex.get(..6).unwrap_or("808080"), 16).unwrap_or(0x808080);
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

fn ease(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

/// What lifts the bar, menus and toasts off the game, instead of a border.
fn shadow() -> egui::Shadow {
    egui::Shadow { offset: [0, 6], blur: 24, spread: 0, color: Color32::from_black_alpha(110) }
}

/// One line of text, cut with "…" to fit `width`.
fn line(painter: &egui::Painter, text: &str, font: FontId, color: Color32, width: f32) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.into(), font, color);
    job.wrap = egui::text::TextWrapping { max_width: width.max(1.0), max_rows: 1, break_anywhere: true,
                                          overflow_character: Some('…') };
    painter.layout_job(job)
}

/// A title over a quieter line, centred on `mid`, from `x`, at most `width` wide.
fn two_lines(painter: &egui::Painter, c: Colors, x: f32, mid: f32, width: f32, title: &str, below: &str) {
    two_lines_at(painter, c, x, mid, width, title, below, 15.0);
}

/// The same, with the title at `size`. Drawn, not laid out as widgets: nothing
/// in it takes the pointer from the row or bar it's on.
#[allow(clippy::too_many_arguments)]
fn two_lines_at(painter: &egui::Painter, c: Colors, x: f32, mid: f32, width: f32, title: &str, below: &str, size: f32) {
    let top = line(painter, title, bold(size), c.text, width);
    let sub = line(painter, below, regular(12.5), c.subtext, width);
    let total = top.size().y + 2.0 + sub.size().y;
    let y = mid - total / 2.0;
    let height = top.size().y;
    painter.galley(pos2(x, y), top, c.text);
    painter.galley(pos2(x, y + height + 2.0), sub, c.subtext);
}

/// The theme as colours.
#[derive(Clone, Copy)]
struct Colors {
    background: Color32,
    surface: Color32,
    hover: Color32,
    text: Color32,
    subtext: Color32,
    accent: Color32,
    on_accent: Color32,
    danger: Color32,
}

impl Colors {
    fn of(t: &Theme) -> Colors {
        Colors {
            background: color(&t.background), surface: color(&t.surface), hover: color(&t.surface_hover),
            text: color(&t.text), subtext: color(&t.subtext), accent: color(&t.accent),
            on_accent: color(&t.on_accent), danger: color(&t.danger),
        }
    }
}

/// Material Symbols, rounded and filled, at the weight of the text (Noto Sans):
/// `dev/icons.sh` makes them, and layer/icons/README.md says whose they are.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Icon {
    Refresh,
    Link,
    Servers,
    /// A controller with a history clock: recent games.
    Games,
    /// A controller: a game without its own picture.
    Game,
    Alts,
    Settings,
    Close,
    Focus,
    Volume,
    Muted,
    /// One of the icons a plugin can name (`mace_proto::ICONS`).
    Named(&'static str),
    /// A plugin's own picture, drawn as a mask in the ink's colour.
    Picture(egui::TextureId),
}


/// The icons a plugin can name, by that name.
macro_rules! named_icons {
    ($($name:literal,)*) => {
        fn named_coverage(name: &str) -> Option<&'static [u8]> {
            match name {
                $($name => Some(include_bytes!(concat!("../icons/", $name, ".a8"))),)*
                _ => None,
            }
        }
        #[cfg(test)]
        const NAMED: &[&str] = &[$($name,)*];
    };
}
named_icons! {
    "add",
    "album",
    "bolt",
    "bookmark",
    "bug_report",
    "chat",
    "check",
    "close",
    "content_copy",
    "delete",
    "edit",
    "emoji_events",
    "explore",
    "extension",
    "favorite",
    "flag",
    "games-recent",
    "group",
    "history",
    "home",
    "info",
    "link",
    "lock",
    "lyrics",
    "map",
    "music_note",
    "notifications",
    "pause",
    "person",
    "play_arrow",
    "public",
    "push_pin",
    "refresh",
    "rocket_launch",
    "schedule",
    "search",
    "select_window",
    "settings",
    "shield",
    "shuffle",
    "skip_next",
    "skip_previous",
    "sports_esports",
    "star",
    "sticky_note_2",
    "storage",
    "swap_horiz",
    "sync",
    "timer",
    "visibility",
    "volume_off",
    "volume_up",
}

/// A plugin's icon: a built-in one by name, else none.
fn named_icon(name: &str) -> Option<Icon> {
    mace_proto::ICONS.iter().find(|n| **n == name).map(|n| Icon::Named(n))
}

/// The pixel sizes each icon comes in, in the order its file has them.
const ICON_SIZES: [u32; 6] = [16, 20, 24, 32, 40, 48];

impl Icon {
    fn coverage(self) -> &'static [u8] {
        match self {
            Icon::Refresh => include_bytes!("../icons/refresh.a8"),
            Icon::Link => include_bytes!("../icons/link.a8"),
            Icon::Servers => include_bytes!("../icons/storage.a8"),
            Icon::Games => include_bytes!("../icons/games-recent.a8"),
            Icon::Game => include_bytes!("../icons/sports_esports.a8"),
            Icon::Alts => include_bytes!("../icons/group.a8"),
            Icon::Settings => include_bytes!("../icons/settings.a8"),
            Icon::Close => include_bytes!("../icons/close.a8"),
            Icon::Focus => include_bytes!("../icons/select_window.a8"),
            Icon::Volume => include_bytes!("../icons/volume_up.a8"),
            Icon::Muted => include_bytes!("../icons/volume_off.a8"),
            Icon::Named(name) => named_coverage(name).unwrap_or(&[]),
            Icon::Picture(_) => &[],
        }
    }

    /// The icon `size` pixels wide, one byte of coverage a pixel.
    fn pixels(self, size: u32) -> &'static [u8] {
        let mut at = 0;
        for s in ICON_SIZES {
            let n = (s * s) as usize;
            if s == size {
                return &self.coverage()[at..at + n];
            }
            at += n;
        }
        &[]
    }
}

/// The icons made into textures so far, per context.
#[derive(Clone, Default)]
struct IconTextures(HashMap<(Icon, u32), egui::TextureHandle>);

/// Draw an icon in `color`, as big as `rect` is (in the size drawn for that
/// many pixels, on whole pixels, so it stays sharp).
fn paint_icon(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    if let Icon::Picture(texture) = icon {
        painter.image(texture, rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), color);
        return;
    }
    let ctx = painter.ctx();
    let ppp = ctx.pixels_per_point();
    let wanted = rect.width().min(rect.height()) * ppp;
    let size = ICON_SIZES.iter().copied().find(|s| *s as f32 >= wanted - 0.5).unwrap_or(48);
    let key = egui::Id::unique("mace-icons");
    let made = ctx.data_mut(|d| d.get_temp_mut_or_default::<IconTextures>(key).0.get(&(icon, size)).map(|t| t.id()));
    let texture = match made {
        Some(id) => id,
        None => {
            let rgba: Vec<u8> = icon.pixels(size).iter().flat_map(|a| [255, 255, 255, *a]).collect();
            let image = egui::ColorImage::from_rgba_unmultiplied([size as usize; 2], &rgba);
            let handle = ctx.load_texture(format!("icon-{size}"), image, egui::TextureOptions::LINEAR);
            let id = handle.id();
            ctx.data_mut(|d| d.get_temp_mut_or_default::<IconTextures>(key).0.insert((icon, size), handle));
            id
        }
    };
    let side = size as f32 / ppp;
    let min = rect.center() - vec2(side, side) / 2.0;
    let min = pos2((min.x * ppp).round() / ppp, (min.y * ppp).round() / ppp);
    painter.image(texture, Rect::from_min_size(min, vec2(side, side)), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                  color);
}

/// A round icon button (32 points, a 20-point icon) for rows and headers.
fn icon_button(ui: &mut EUi, c: Colors, icon: Icon, tip: &str, tint: Option<Color32>) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::click());
    if response.hovered() {
        ui.painter().circle_filled(rect.center(), 16.0, c.hover);
    }
    paint_icon(ui.painter(), Rect::from_center_size(rect.center(), vec2(20.0, 20.0)), icon, tint.unwrap_or(c.text));
    response.on_hover_text(tip)
}

/// A button in the bar.
struct BarButton {
    salt: String,
    icon: Icon,
    tip: String,
    /// Its drop-down is open.
    selected: bool,
    enabled: bool,
    /// Counts something, in a small pill at its top right.
    badge: Option<usize>,
}

/// A bar button: a 24-point icon in a round 40-point target, flat until
/// hovered and filled while its drop-down is open.
fn bar_button(ui: &mut EUi, c: Colors, rect: Rect, b: &BarButton) -> Response {
    let BarButton { icon, selected, enabled, badge, .. } = *b;
    let response = ui.interact(rect, ui.scope_id().with(&b.salt), if enabled { Sense::click() } else { Sense::hover() });
    let painter = ui.painter();
    let fill = if selected {
        Some(c.hover.lerp_to_gamma(c.text, 0.18))
    } else if enabled && response.is_pointer_button_down_on() {
        Some(c.hover.lerp_to_gamma(c.text, 0.1))
    } else if enabled && response.hovered() {
        Some(c.hover)
    } else {
        None
    };
    if let Some(fill) = fill {
        painter.circle_filled(rect.center(), rect.width() / 2.0, fill);
    }
    let ink = if enabled { c.text } else { c.subtext.gamma_multiply(0.45) };
    paint_icon(painter, Rect::from_center_size(rect.center(), vec2(24.0, 24.0)), icon, ink);
    if let Some(n) = badge.filter(|n| *n > 0) {
        let digits = painter.layout_no_wrap(n.to_string(), bold(10.5), c.on_accent);
        let pill = Rect::from_min_size(pos2(rect.center().x + 3.0, rect.min.y + 3.0),
                                       vec2(16.0f32.max(digits.size().x + 8.0), 16.0));
        // a ring of the bar's colour keeps it off the icon
        painter.rect_filled(pill.expand(2.0), CornerRadius::same(10), c.background);
        painter.rect_filled(pill, CornerRadius::same(8), if enabled { c.accent } else { c.subtext });
        painter.galley(pill.center() - digits.size() / 2.0, digits, c.on_accent);
    }
    response.on_hover_text(&b.tip)
}

/// A small text button (chips in the server list).
fn text_button(ui: &mut EUi, c: Colors, label: &str, selected: bool) -> Response {
    let galley = ui.painter().layout_no_wrap(label.into(), regular(13.0), c.text);
    let size = galley.size() + vec2(28.0, 16.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let fill = if selected { c.accent } else if response.hovered() { c.hover.lerp_to_gamma(c.text, 0.08) } else { c.hover };
    let ink = if selected { c.on_accent } else { c.text };
    ui.painter().rect_filled(rect, CornerRadius::same(10), fill);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, label, regular(13.0), ink);
    response
}

/// Which plugin menu a control is in, to say so when it's used.
struct PluginAt<'a> {
    plugin: &'a str,
    button: &'a str,
    /// When the menu came, for blocks that move on by themselves.
    arrived: Instant,
}

impl PluginAt<'_> {
    fn send(&self, actions: &mut Vec<Action>, key: &Option<String>, what: &str, value: Option<serde_json::Value>) {
        if let Some(key) = key {
            actions.push(Action::Send(ToMace::Plugin {
                plugin: self.plugin.into(), button: self.button.into(), key: key.clone(), what: what.into(), value,
            }));
        }
    }
}

/// A plugin's button: primary (filled with the accent), danger (red text) or
/// plain, with an icon before the label if it has one.
fn plugin_button(ui: &mut EUi, c: Colors, label: &str, style: &str, icon: Option<Icon>, enabled: bool) -> Response {
    let font = regular(13.5);
    let galley = ui.painter().layout_no_wrap(label.into(), font.clone(), c.text);
    let icon_room = if icon.is_some() { 18.0 + 6.0 } else { 0.0 };
    let size = vec2(galley.size().x + icon_room + 32.0, 36.0);
    let (rect, response) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    let hovered = enabled && response.hovered();
    let (fill, ink) = match style {
        "primary" => (if hovered { c.accent.lerp_to_gamma(c.text, 0.25) } else { c.accent }, c.on_accent),
        "danger" => (if hovered { c.hover.lerp_to_gamma(c.danger, 0.15) } else { c.hover }, c.danger),
        _ => (if hovered { c.hover.lerp_to_gamma(c.text, 0.08) } else { c.hover }, c.text),
    };
    let ink = if enabled { ink } else { ink.gamma_multiply(0.5) };
    ui.painter().rect_filled(rect, CornerRadius::same(10), fill);
    let mut x = rect.min.x + 16.0;
    if let Some(icon) = icon {
        paint_icon(ui.painter(), Rect::from_center_size(pos2(x + 9.0, rect.center().y), vec2(18.0, 18.0)), icon, ink);
        x += icon_room;
    }
    ui.painter().text(pos2(x, rect.center().y), Align2::LEFT_CENTER, label, font, ink);
    response
}

fn small(ui: &mut EUi, c: Colors, text: &str) {
    ui.add(egui::Label::new(egui::RichText::new(text).font(regular(12.5)).color(c.subtext)).truncate());
}

fn title(ui: &mut EUi, c: Colors, text: &str, size: f32) {
    ui.add(egui::Label::new(egui::RichText::new(text).font(bold(size)).color(c.text)).truncate());
}

fn heading(ui: &mut EUi, c: Colors, text: &str) {
    ui.add_space(2.0);
    ui.label(egui::RichText::new(text).font(bold(12.0)).color(c.subtext));
}

/// Roblox gives places with a flag emoji in front; the panel's font has no flags.
fn plain_location(text: &str) -> String {
    let kept: String = text.chars().filter(|ch| !('\u{1F1E6}'..='\u{1F1FF}').contains(ch)).collect();
    kept.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn since(seconds: f64) -> String {
    let minutes = ((now() - seconds) / 60.0).max(0.0) as u64;
    match minutes {
        0 => "just joined".into(),
        1..=59 => format!("{minutes} min"),
        _ => format!("{} h {} min", minutes / 60, minutes % 60),
    }
}

/// "just now", "5 min ago", "3 h ago", "yesterday", "4 days ago".
fn ago(when: f64, now: f64) -> String {
    let minutes = ((now - when).max(0.0) / 60.0) as u64;
    match minutes {
        0 => "just now".into(),
        1..60 => format!("{minutes} min ago"),
        60..1440 => format!("{} h ago", minutes / 60),
        1440..2880 => "yesterday".into(),
        _ => format!("{} days ago", minutes / 1440),
    }
}

fn status(a: &Account, here: Option<&str>) -> String {
    if matches!(a.launch.as_deref(), Some("preparing" | "downloading" | "starting" | "updating")) {
        return "starting…".into();
    }
    if !a.running {
        return "not running".into();
    }
    let Some(server) = &a.server else { return "not in a game".into() };
    let mut text = if Some(server.job.as_str()) == here {
        "here".to_string()
    } else {
        format!("in {}", server.name.as_deref().unwrap_or("another game"))
    };
    if let Some(fps) = a.fps {
        text.push_str(&format!(" · {fps} FPS"));
    }
    text
}

/// Which drop-down hangs under the bar.
#[derive(Clone, Copy, Default, PartialEq)]
enum Drop {
    #[default]
    None,
    Servers,
    Alts,
    Games,
    /// A plugin's menu (which one: `Ui::plugin_drop`).
    Plugin,
}

/// What a drop-down shows. Pinned, it stays on screen while the panel is closed.
#[derive(Clone, PartialEq)]
enum Card {
    Servers,
    Alts,
    Games,
    Plugin(String, String),
}

impl Card {
    fn id(&self) -> String {
        match self {
            Card::Servers => "servers".into(),
            Card::Alts => "alts".into(),
            Card::Games => "games".into(),
            Card::Plugin(plugin, button) => format!("plugin:{plugin}/{button}"),
        }
    }

    fn from_id(id: &str) -> Option<Card> {
        Some(match id {
            "servers" => Card::Servers,
            "alts" => Card::Alts,
            "games" => Card::Games,
            _ => {
                let (plugin, button) = id.strip_prefix("plugin:")?.split_once('/')?;
                Card::Plugin(plugin.into(), button.into())
            }
        })
    }

    fn width(&self) -> f32 {
        if *self == Card::Servers { 520.0 } else { 440.0 }
    }
}

/// The bar's own pin.
const BAR_PIN: &str = "bar";

#[derive(Default)]
struct Browser {
    place: String,
    sort: Sort,
    list: Vec<PublicServer>,
    next: Option<String>,
    /// The cursor of the page being waited for (Some(None): the first page).
    waiting: Option<Option<String>>,
    error: String,
}

struct Toast {
    title: String,
    body: String,
    at: Instant,
}

#[derive(Default)]
pub struct Ui {
    drop: Drop,
    next_id: u64,
    /// Mute states shown before Mace confirms them.
    muted: HashMap<String, (bool, Instant)>,
    avatars: HashMap<String, (Avatar, egui::TextureHandle)>,
    browser: Browser,
    toasts: Vec<Toast>,
    /// The plugin and button whose menu `Drop::Plugin` is.
    plugin_drop: Option<(String, String)>,
    /// The plugin menus Mace was last told are showing (open, or pinned).
    told_open: Vec<(String, String)>,
    /// What's pinned, and where: Mace's (from the config), changed here first.
    pins: Vec<Pin>,
    /// The pins the config had when last looked at.
    seen_pins: Option<Vec<Pin>>,
    /// A pinned card is being dragged: the config's pins wait.
    dragging: bool,
    /// Plugin pictures made into white masks, to draw as icons.
    masks: HashMap<String, (Avatar, egui::TextureHandle)>,
    /// What's typed in plugin menus' text fields: (the value Mace gave, the text now), by place in the menu.
    inputs: HashMap<String, (String, String)>,
}

impl Ui {
    pub fn opened(&mut self) {
        self.drop = Drop::None;
    }

    /// Toasts still on screen (or new ones to show).
    pub fn has_toasts(&self) -> bool {
        self.toasts.iter().any(|t| t.at.elapsed().as_secs_f32() < TOAST_SECONDS)
    }

    fn toast(&mut self, title: impl Into<String>, body: impl Into<String>) {
        self.toasts.push(Toast { title: title.into(), body: body.into(), at: Instant::now() });
        if self.toasts.len() > 8 {
            self.toasts.remove(0);
        }
    }

    fn say(&mut self, text: impl Into<String>) {
        self.toast(text, "");
    }

    fn run(&mut self, actions: &mut Vec<Action>, args: &[&str]) {
        self.next_id += 1;
        actions.push(Action::Send(ToMace::Run { id: self.next_id, args: args.iter().map(|s| s.to_string()).collect() }));
    }

    fn texture(&mut self, ctx: &egui::Context, user: &str, avatars: &HashMap<String, Avatar>) -> Option<egui::TextureId> {
        let avatar = avatars.get(user)?;
        let fresh = self.avatars.get(user).is_none_or(|(a, _)| !Arc::ptr_eq(a, avatar));
        if fresh {
            let size = avatar.0 as usize;
            let image = egui::ColorImage::from_rgba_unmultiplied([size, size], &avatar.1);
            let handle = ctx.load_texture(format!("avatar-{user}"), image, egui::TextureOptions::LINEAR);
            self.avatars.insert(user.to_string(), (avatar.clone(), handle));
        }
        self.avatars.get(user).map(|(_, h)| h.id())
    }

    fn paint_avatar(&mut self, ui: &EUi, c: Colors, a: &Account, rect: Rect, avatars: &HashMap<String, Avatar>) {
        match a.user.as_deref().and_then(|u| self.texture(ui.ctx(), u, avatars)) {
            Some(id) => {
                egui::Image::new((id, rect.size())).corner_radius(CornerRadius::same((rect.width() / 2.0) as u8))
                    .paint_at(ui, rect);
            }
            None => {
                ui.painter().circle_filled(rect.center(), rect.width() / 2.0, c.hover);
                let initial = a.name.chars().next().unwrap_or('?').to_uppercase().to_string();
                ui.painter().text(rect.center(), Align2::CENTER_CENTER, initial, bold(rect.width() * 0.42), c.text);
            }
        }
    }

    fn is_muted(&self, a: &Account) -> Option<bool> {
        match self.muted.get(&a.id) {
            Some((m, at)) if at.elapsed().as_secs_f32() < 3.0 => Some(*m),
            _ => a.muted,
        }
    }

    fn toggle_mute(&mut self, actions: &mut Vec<Action>, a: &Account) {
        let now_muted = !self.is_muted(a).unwrap_or(false);
        self.muted.insert(a.id.clone(), (now_muted, Instant::now()));
        self.run(actions, &["mute", &a.id, if now_muted { "on" } else { "off" }]);
    }

    fn open_drop(&mut self, drop: Drop) {
        // a pinned one is out already
        let pinned = match drop {
            Drop::Servers => self.is_pinned("servers"),
            Drop::Alts => self.is_pinned("alts"),
            Drop::Games => self.is_pinned("games"),
            _ => false,
        };
        self.drop = if self.drop == drop || pinned { Drop::None } else { drop };
    }

    fn open_plugin(&mut self, plugin: &str, button: &str) {
        if self.is_pinned(&Card::Plugin(plugin.into(), button.into()).id()) {
            return;
        }
        let this = Some((plugin.to_string(), button.to_string()));
        if self.drop == Drop::Plugin && self.plugin_drop == this {
            self.drop = Drop::None;
        } else {
            self.drop = Drop::Plugin;
            self.plugin_drop = this;
        }
    }

    /// A plugin's picture as a white mask, to be tinted like the built-in icons.
    fn mask(&mut self, ctx: &egui::Context, key: &str, avatars: &HashMap<String, Avatar>) -> Option<egui::TextureId> {
        let picture = avatars.get(key)?;
        let fresh = self.masks.get(key).is_none_or(|(a, _)| !Arc::ptr_eq(a, picture));
        if fresh {
            let size = picture.0 as usize;
            let rgba: Vec<u8> = picture.1.as_chunks::<4>().0.iter().flat_map(|p| [255, 255, 255, p[3]]).collect();
            let image = egui::ColorImage::from_rgba_unmultiplied([size, size], &rgba);
            let handle = ctx.load_texture(format!("mask-{key}"), image, egui::TextureOptions::LINEAR);
            self.masks.insert(key.to_string(), (picture.clone(), handle));
        }
        self.masks.get(key).map(|(_, h)| h.id())
    }

    /// The icon a plugin named: built in, or its picture (a puzzle piece until that arrives).
    fn plugin_icon(&mut self, ctx: &egui::Context, name: &str, avatars: &HashMap<String, Avatar>) -> Icon {
        if name.starts_with("plugin:") {
            if let Some(texture) = self.mask(ctx, name, avatars) {
                return Icon::Picture(texture);
            }
        } else if let Some(icon) = named_icon(name) {
            return icon;
        }
        Icon::Named("extension")
    }

    /// The drop-down open now, as a card.
    fn current_card(&self) -> Option<Card> {
        match self.drop {
            Drop::None => None,
            Drop::Servers => Some(Card::Servers),
            Drop::Alts => Some(Card::Alts),
            Drop::Games => Some(Card::Games),
            Drop::Plugin => self.plugin_drop.clone().map(|(p, b)| Card::Plugin(p, b)),
        }
    }

    fn is_pinned(&self, id: &str) -> bool {
        self.pins.iter().any(|p| p.id == id)
    }

    /// Take the config's pins, unless one is being dragged.
    fn sync_pins(&mut self, d: &Snapshot) {
        let Some(config) = &d.config else { return };
        if self.seen_pins.as_ref() != Some(&config.pins) {
            self.seen_pins = Some(config.pins.clone());
            if !self.dragging {
                self.pins = config.pins.clone();
            }
        }
    }

    /// Pin (at `at`) or unpin, and tell Mace, which keeps it for every game.
    fn set_pin(&mut self, id: &str, at: Option<egui::Pos2>, actions: &mut Vec<Action>) {
        self.pins.retain(|p| p.id != id);
        if let Some(at) = at {
            self.pins.push(Pin { id: id.into(), x: at.x, y: at.y });
        }
        actions.push(Action::Send(ToMace::Pins { pins: self.pins.clone() }));
    }

    /// Whether a plugin's button is still there (its card goes with it).
    fn card_exists(card: &Card, d: &Snapshot) -> bool {
        match card {
            Card::Plugin(p, b) => d.plugins.iter().any(|x| x.plugin == *p && x.id == *b),
            _ => true,
        }
    }

    /// Tell Mace which plugin menus are showing (open or pinned), when that changes.
    fn tell_plugin_menu(&mut self, open: bool, d: &Snapshot, actions: &mut Vec<Action>) {
        // a plugin whose button went away closes its menu
        if let Some(card) = self.current_card()
            && !Self::card_exists(&card, d)
        {
            self.drop = Drop::None;
        }
        let mut now: Vec<(String, String)> = self.pins.iter().filter_map(|p| match Card::from_id(&p.id) {
            Some(Card::Plugin(plugin, button)) if Self::card_exists(&Card::Plugin(plugin.clone(), button.clone()), d) => {
                Some((plugin, button))
            }
            _ => None,
        }).collect();
        if open && self.drop == Drop::Plugin
            && let Some(shown) = self.plugin_drop.clone()
            && !now.contains(&shown)
        {
            now.push(shown);
        }
        let message = |(plugin, button): (String, String), what: &str| Action::Send(ToMace::Plugin {
            plugin, button, key: String::new(), what: what.into(), value: None });
        for was in self.told_open.clone() {
            if !now.contains(&was) {
                actions.push(message(was, "close"));
            }
        }
        for shown in &now {
            if !self.told_open.contains(shown) {
                actions.push(message(shown.clone(), "open"));
            }
        }
        self.told_open = now;
    }

    /// Draw the panel; `progress` (0..1, eased) is how far the bar has come in
    /// (0: closed, when only toasts are drawn).
    pub fn show(&mut self, root: &mut EUi, progress: f32, d: &Snapshot, escape: bool) -> Vec<Action> {
        let mut actions = Vec::new();
        for (title, body) in &d.toasts {
            self.toast(title.clone(), body.clone());
        }
        for (_, ok, error) in &d.done {
            if !ok && !error.is_empty() {
                self.say(error.clone());
            }
        }
        for page in &d.pages {
            self.take_page(page);
        }
        for text in &d.copies {
            actions.push(Action::Copy(text.clone()));
        }
        if d.close {
            actions.push(Action::Close);
        }
        if escape {
            if self.drop != Drop::None {
                self.drop = Drop::None;
            } else {
                actions.push(Action::Close);
            }
        }
        let theme = d.config.as_ref().map(|c| c.theme.clone()).unwrap_or_default();
        let c = Colors::of(&theme);
        let ctx = root.ctx().clone();
        let screen = ctx.content_rect();

        self.sync_pins(d);
        // as at the start of the frame: one pinned in it shows from the next (or its pin would take the same click)
        let pinned_before = self.pins.clone();
        let open = progress > 0.0;
        let bar_pinned = self.is_pinned(BAR_PIN);
        if open {
            // the game behind: dimmed, and a click on it closes the panel
            egui::Area::new(egui::Id::unique("mace-backdrop")).order(egui::Order::Middle).fade_in(false).constrain(false)
                .fixed_pos(screen.min)
                .show(&ctx, |ui| {
                    let response = ui.allocate_rect(screen, Sense::click());
                    ui.painter().rect_filled(screen, 0.0, Color32::from_black_alpha((110.0 * progress) as u8));
                    if response.clicked() {
                        actions.push(Action::Close);
                    }
                });
        }
        if open || bar_pinned {
            // comes down from above the top edge, fading in on the way (pinned, it's there already)
            let slide = if bar_pinned { 1.0 } else { progress };
            let opacity = (slide * 1.6).min(1.0);
            let width = BAR_WIDEST.min(screen.width() - 48.0).max(300.0);
            let top = screen.min.y + BAR_TOP - (1.0 - slide) * (BAR_HEIGHT + BAR_TOP + 16.0);
            let bar = Rect::from_min_size(pos2(screen.center().x - width / 2.0, top), vec2(width, BAR_HEIGHT));
            self.bar(&ctx, c, d, bar, opacity, &mut actions);
            if open && let Some(card) = self.current_card().filter(|card| !self.is_pinned(&card.id())) {
                // hangs from the bar's trailing end, under the buttons that open it
                let w = f32::min(card.width(), bar.width());
                let at = pos2(bar.max.x - w, bar.max.y + 8.0);
                // never down to the bottom edge, where toasts show while the bar is open
                let room = (screen.max.y - at.y - 120.0).clamp(160.0, 600.0);
                self.card(&ctx, c, d, &card, Rect::from_min_size(at, vec2(w, room)), opacity, &mut actions);
            }
        }
        // what's pinned stays where it was put (inside the screen), the panel open or not
        for pin in pinned_before {
            let Some(card) = Card::from_id(&pin.id).filter(|card| Self::card_exists(card, d)) else { continue };
            let w = card.width().min(screen.width() - 32.0);
            let x = pin.x.clamp(screen.min.x + 8.0, (screen.max.x - w - 8.0).max(screen.min.x + 8.0));
            let y = pin.y.clamp(screen.min.y + 8.0, (screen.max.y - 120.0).max(screen.min.y + 8.0));
            let room = (screen.max.y - y - 16.0).clamp(120.0, 600.0);
            self.card(&ctx, c, d, &card, Rect::from_min_size(pos2(x, y), vec2(w, room)), 1.0, &mut actions);
        }
        self.tell_plugin_menu(progress > 0.0, d, &mut actions);
        self.draw_toasts(&ctx, c, screen, progress > 0.0);
        actions
    }

    fn bar(&mut self, ctx: &egui::Context, c: Colors, d: &Snapshot, bar: Rect, opacity: f32, actions: &mut Vec<Action>) {
        egui::Area::new(egui::Id::unique("mace-bar")).order(egui::Order::Foreground).fade_in(false).constrain(false)
            .fixed_pos(bar.min)
            .show(ctx, |ui| {
                ui.set_opacity(opacity);
                let painter = ui.painter().clone();
                painter.add(shadow().as_shape(bar, CornerRadius::same(BAR_RADIUS)));
                painter.rect_filled(bar, CornerRadius::same(BAR_RADIUS), c.background);
                // clicks on the bar's empty parts aren't clicks on the game
                let _ = ui.allocate_rect(bar, Sense::click());
                let mid = bar.center().y;
                let avatar = Rect::from_center_size(pos2(bar.min.x + INSET + CONTROL / 2.0, mid), vec2(CONTROL, CONTROL));
                let text_left = avatar.max.x + 12.0;

                let Some(me) = d.me() else {
                    painter.circle_filled(avatar.center(), CONTROL / 2.0, c.hover);
                    painter.text(avatar.center(), Align2::CENTER_CENTER, "M", bold(16.0), c.text);
                    two_lines(&painter, c, text_left, mid, bar.max.x - INSET - text_left, "Mace",
                              if d.connected { "Loading…" } else { "Connecting to Mace…" });
                    return;
                };
                self.paint_avatar(ui, c, &me, avatar, &d.avatars);

                // the actions, evenly spaced from the trailing end: Mace Settings, alts,
                // recent games, servers, copy this server's link, rejoin
                let in_game = me.server.is_some();
                let alts = d.state.as_ref().map_or(0, |s| s.accounts.iter().filter(|a| a.id != me.id && a.running).count());
                let bar_pinned = self.is_pinned(BAR_PIN);
                // a pinned card's button shows it's out
                let open = |drop: Drop| self.drop == drop || match drop {
                    Drop::Servers => self.is_pinned("servers"),
                    Drop::Alts => self.is_pinned("alts"),
                    Drop::Games => self.is_pinned("games"),
                    _ => false,
                };
                let button = |salt: &str, icon, tip: &str, selected, enabled, badge| BarButton {
                    salt: salt.into(), icon, tip: tip.into(), selected, enabled, badge };
                let mut buttons = vec![
                    button("settings", Icon::Settings, "Mace Settings", false, true, None),
                    button("pin", Icon::Named("push_pin"), if bar_pinned { "Unpin the bar" } else { "Keep the bar on screen" },
                           bar_pinned, true, None),
                    button("alts", Icon::Alts, "Alts", open(Drop::Alts), true, Some(alts)),
                    button("games", Icon::Games, "Recent games", open(Drop::Games), true, None),
                    button("servers", Icon::Servers, "Servers", open(Drop::Servers), in_game, None),
                    button("link", Icon::Link, "Copy join link", false, in_game, None),
                    button("rejoin", Icon::Refresh, "Rejoin", false, in_game, None),
                ];
                // the plugins' buttons, before the built-in ones
                for (i, p) in d.plugins.iter().enumerate() {
                    let icon = self.plugin_icon(ui.ctx(), &p.icon, &d.avatars);
                    let selected = (self.drop == Drop::Plugin
                        && self.plugin_drop.as_ref().is_some_and(|(plugin, id)| *plugin == p.plugin && *id == p.id))
                        || self.is_pinned(&Card::Plugin(p.plugin.clone(), p.id.clone()).id());
                    buttons.push(BarButton { salt: format!("plugin:{i}"), icon, tip: p.tip.clone(), selected, enabled: true,
                                             badge: p.badge.map(|b| b as usize) });
                }
                let mut right = bar.max.x - INSET;
                let mut clicked = Vec::new();
                for b in &buttons {
                    let rect = Rect::from_min_max(pos2(right - CONTROL, mid - CONTROL / 2.0), pos2(right, mid + CONTROL / 2.0));
                    right = rect.min.x - BUTTON_GAP;
                    if bar_button(ui, c, rect, b).clicked() {
                        clicked.push(b.salt.clone());
                    }
                }
                right += BUTTON_GAP;
                for salt in clicked {
                    match salt.as_str() {
                        "link" => {
                            if let Some(server) = &me.server {
                                actions.push(Action::Copy(server.link.clone()));
                                self.say("Link copied");
                            }
                        }
                        "alts" => self.open_drop(Drop::Alts),
                        "games" => self.open_drop(Drop::Games),
                        "servers" => {
                            if let Some(server) = &me.server {
                                self.open_browser(actions, &server.place);
                            }
                        }
                        "settings" => {
                            self.run(actions, &["settings"]);
                            self.say("Opening Mace Settings…");
                            actions.push(Action::Close);
                        }
                        "pin" => {
                            let at = (!bar_pinned).then_some(pos2(0.0, 0.0));
                            self.set_pin(BAR_PIN, at, actions);
                        }
                        "rejoin" => {
                            self.run(actions, &["rejoin", &me.id]);
                            self.say("Rejoining…");
                            actions.push(Action::Close);
                        }
                        plugin => {
                            let index = plugin.strip_prefix("plugin:").and_then(|i| i.parse::<usize>().ok());
                            if let Some(p) = index.and_then(|i| d.plugins.get(i)) {
                                self.open_plugin(&p.plugin, &p.id);
                            }
                        }
                    }
                }

                // you, then this server, in the space left (with room to breathe before the buttons)
                let room = right - 24.0 - text_left;
                let user = if me.username.is_empty() { String::new() } else { format!("@{}", me.username) };
                let wide = |text: &str, font: FontId| painter.layout_no_wrap(text.into(), font, c.text).size().x;
                let identity = wide(&me.name, bold(15.0)).max(wide(&user, regular(12.5))).min(160.0);
                let (server_left, server_room) = if room > identity + 32.0 + 160.0 {
                    two_lines(&painter, c, text_left, mid, identity, &me.name, &user);
                    (text_left + identity + 32.0, room - identity - 32.0)
                } else {
                    (text_left, room)
                };
                match &me.server {
                    Some(server) => {
                        let place = [server.location.as_deref().map(plain_location), server.since.map(since)]
                            .into_iter().flatten().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                        two_lines(&painter, c, server_left, mid, server_room, server.name.as_deref().unwrap_or("This game"),
                                  &place);
                    }
                    None => two_lines(&painter, c, server_left, mid, server_room,
                                      if me.running { "Not in a game" } else { "Starting…" },
                                      ""),
                }
            });
    }

    /// A drop-down's card: hanging from the bar, or pinned where it was put
    /// (then dragged by its top while the panel is open). Its pin is in its top right corner.
    #[allow(clippy::too_many_arguments)]
    fn card(&mut self, ctx: &egui::Context, c: Colors, d: &Snapshot, card: &Card, room: Rect, opacity: f32,
            actions: &mut Vec<Action>) {
        let id = card.id();
        let pinned = self.is_pinned(&id);
        egui::Area::new(egui::Id::unique(("mace-card", &id))).order(egui::Order::Foreground).fade_in(false).constrain(false)
            .fixed_pos(room.min)
            .show(ctx, |ui| {
                ui.set_opacity(opacity);
                let margin = 20.0;
                let frame = egui::Frame::new().fill(c.background).corner_radius(CornerRadius::same(20)).inner_margin(margin)
                    .shadow(shadow())
                    .show(ui, |ui| {
                        ui.set_width(room.width() - 2.0 * margin);
                        ui.set_max_height(room.height() - 2.0 * margin);
                        match card {
                            Card::Servers => self.servers(ui, c, d, actions),
                            Card::Alts => self.alts(ui, c, d, actions),
                            Card::Games => self.games(ui, c, d, actions),
                            Card::Plugin(plugin, button) => self.plugin_menu(ui, c, d, plugin, button, actions),
                        }
                    });
                let rect = frame.response.rect;
                // a pinned card moves by its top
                if pinned {
                    let handle = Rect::from_min_size(rect.min, vec2(rect.width() - 64.0, 52.0));
                    let drag = ui.interact(handle, ui.scope_id().with("drag"), Sense::drag());
                    if drag.dragged() {
                        self.dragging = true;
                        let delta = drag.drag_delta();
                        if let Some(pin) = self.pins.iter_mut().find(|p| p.id == id) {
                            pin.x += delta.x;
                            pin.y += delta.y;
                        }
                    }
                    if drag.drag_stopped() {
                        self.dragging = false;
                        actions.push(Action::Send(ToMace::Pins { pins: self.pins.clone() }));
                    }
                }
                let at = Rect::from_min_size(pos2(rect.max.x - 14.0 - 32.0, rect.min.y + 14.0), vec2(32.0, 32.0));
                let mut corner = ui.new_child(egui::UiBuilder::new().max_rect(at));
                let tint = if pinned { Some(c.text) } else { Some(c.subtext) };
                if icon_button(&mut corner, c, Icon::Named("push_pin"), if pinned { "Unpin" } else { "Keep on screen" }, tint)
                    .clicked()
                {
                    if pinned {
                        self.set_pin(&id, None, actions);
                    } else {
                        // it stays where it is; the drop-down it was is closed
                        self.set_pin(&id, Some(rect.min), actions);
                        self.drop = Drop::None;
                    }
                }
            });
    }

    fn alts(&mut self, ui: &mut EUi, c: Colors, d: &Snapshot, actions: &mut Vec<Action>) {
        let (Some(state), Some(config), Some(me)) = (&d.state, &d.config, d.me()) else { return };
        let here = me.server.as_ref().map(|s| s.job.clone());
        let others: Vec<&Account> = state.accounts.iter().filter(|a| a.id != me.id).collect();
        let running: Vec<&Account> = others.iter().copied().filter(|a| a.running).collect();
        let addable: Vec<&Account> = others.iter().copied()
            .filter(|a| !a.running || (here.is_some() && a.server.as_ref().map(|s| &s.job) != here.as_ref()))
            .collect();
        title(ui, c, "Alts", 16.0);
        if others.is_empty() {
            small(ui, c, "No other accounts");
            return;
        }
        let height = (ui.max_rect().bottom() - ui.cursor().top()).max(120.0);
        egui::ScrollArea::vertical().auto_shrink([false, true]).max_height(height).show(ui, |ui| {
            if !running.is_empty() {
                heading(ui, c, "Running");
                for alt in &running {
                    self.alt_row(ui, c, alt, here.as_deref(), config.can_focus, &d.avatars, actions);
                }
            }
            if !addable.is_empty() {
                ui.add_space(4.0);
                heading(ui, c, if here.is_some() { "Add to this server" } else { "Start" });
                if here.is_some() && addable.len() > 1
                    && self.pick_row(ui, c, None, "Everyone", "All your accounts", &d.avatars).clicked()
                {
                    self.run(actions, &["gather", &me.id]);
                    self.say("Bringing everyone…");
                    self.drop = Drop::None;
                }
                for alt in &addable {
                    let line = status(alt, here.as_deref());
                    if self.pick_row(ui, c, Some(alt), &alt.name, &line, &d.avatars).clicked() {
                        if here.is_some() {
                            self.run(actions, &["start", &alt.id, &me.id]);
                            self.say(format!("{} is joining…", alt.name));
                        } else {
                            self.run(actions, &["start", &alt.id]);
                            self.say(format!("Starting {}…", alt.name));
                        }
                        self.drop = Drop::None;
                    }
                }
            }
        });
    }

    fn games(&mut self, ui: &mut EUi, c: Colors, d: &Snapshot, actions: &mut Vec<Action>) {
        let (Some(state), Some(me)) = (&d.state, d.me()) else { return };
        title(ui, c, "Recent games", 16.0);
        if state.games.is_empty() {
            small(ui, c, "Games you play show up here");
            return;
        }
        let height = (ui.max_rect().bottom() - ui.cursor().top()).max(120.0);
        egui::ScrollArea::vertical().auto_shrink([false, true]).max_height(height).show(ui, |ui| {
            for game in &state.games {
                let who = state.accounts.iter().find(|a| a.id == game.account).map_or("", |a| a.name.as_str());
                let line = [who.to_string(), ago(game.when, state.now)].into_iter().filter(|s| !s.is_empty())
                    .collect::<Vec<_>>().join(" · ");
                if self.game_row(ui, c, game, &line, &d.avatars).clicked() {
                    let link = format!("roblox://experiences/start?placeId={}", game.place);
                    self.run(actions, &["start", &me.id, &link]);
                    self.say(format!("Joining {}…", game.name));
                    actions.push(Action::Close);
                }
            }
        });
    }

    fn game_row(&mut self, ui: &mut EUi, c: Colors, game: &Game, line: &str, avatars: &HashMap<String, Avatar>) -> Response {
        let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 56.0), Sense::click());
        if response.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(12), c.hover);
        }
        let icon = Rect::from_center_size(pos2(rect.min.x + 12.0 + 18.0, rect.center().y), vec2(36.0, 36.0));
        match self.texture(ui.ctx(), &format!("game:{}", game.universe), avatars) {
            Some(id) => egui::Image::new((id, icon.size())).corner_radius(CornerRadius::same(8)).paint_at(ui, icon),
            None => {
                ui.painter().rect_filled(icon, CornerRadius::same(8), c.surface);
                paint_icon(ui.painter(), icon.shrink(8.0), Icon::Game, c.subtext);
            }
        }
        let x = icon.max.x + 12.0;
        two_lines_at(ui.painter(), c, x, rect.center().y, rect.max.x - 12.0 - x, &game.name, line, 14.0);
        response.on_hover_text(format!("Play {}", game.name))
    }

    #[allow(clippy::too_many_arguments)]
    fn alt_row(&mut self, ui: &mut EUi, c: Colors, a: &Account, here: Option<&str>, can_focus: bool,
               avatars: &HashMap<String, Avatar>, actions: &mut Vec<Action>) {
        // the avatar and text are drawn; only the buttons are widgets
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 56.0), Sense::hover());
        let avatar = Rect::from_center_size(pos2(rect.min.x + 12.0 + 18.0, rect.center().y), vec2(36.0, 36.0));
        self.paint_avatar(ui, c, a, avatar, avatars);
        let mut row = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(8.0, 12.0)))
            .layout(egui::Layout::right_to_left(egui::Align::Center)));
        let x = avatar.max.x + 12.0;
        let buttons = if can_focus { 3.0 } else { 2.0 } * 36.0;
        two_lines_at(ui.painter(), c, x, rect.center().y, rect.max.x - 8.0 - buttons - 12.0 - x, &a.name,
                     &status(a, here), 14.0);
        row.scope(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if icon_button(ui, c, Icon::Close, &format!("Stop {}", a.name), Some(c.danger)).clicked() {
                self.run(actions, &["stop", &a.id]);
                self.say(format!("Stopping {}…", a.name));
            }
            if can_focus && icon_button(ui, c, Icon::Focus, &format!("Switch to {}", a.name), None).clicked() {
                self.run(actions, &["focus", &a.id]);
                actions.push(Action::Close);
            }
            if let Some(muted) = self.is_muted(a) {
                let icon = if muted { Icon::Muted } else { Icon::Volume };
                if icon_button(ui, c, icon, if muted { "Unmute" } else { "Mute" }, None).clicked() {
                    self.toggle_mute(actions, a);
                }
            }
        });
    }

    fn pick_row(&mut self, ui: &mut EUi, c: Colors, a: Option<&Account>, name: &str, line: &str,
                avatars: &HashMap<String, Avatar>) -> Response {
        // one target, the whole row: everything in it is drawn, so no part of it takes the pointer
        let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 56.0), Sense::click());
        if response.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(12), c.hover);
        }
        let avatar = Rect::from_center_size(pos2(rect.min.x + 12.0 + 18.0, rect.center().y), vec2(36.0, 36.0));
        match a {
            Some(a) => self.paint_avatar(ui, c, a, avatar, avatars),
            None => {
                ui.painter().circle_filled(avatar.center(), 18.0, c.surface);
                paint_icon(ui.painter(), avatar.shrink(8.0), Icon::Alts, c.text);
            }
        }
        let x = avatar.max.x + 12.0;
        two_lines_at(ui.painter(), c, x, rect.center().y, rect.max.x - 12.0 - x, name, line, 14.0);
        response
    }

    // -- the server browser

    /// A plugin's drop-down: its menu as Mace last sent it, drawn from the blocks in it.
    fn plugin_menu(&mut self, ui: &mut EUi, c: Colors, d: &Snapshot, plugin: &str, button: &str,
                   actions: &mut Vec<Action>) {
        let (plugin, button) = (plugin.to_string(), button.to_string());
        ui.spacing_mut().item_spacing.y = 2.0;
        // the title stops short of the card's pin
        let header = ui.available_width() - 40.0;
        let Some((menu, arrived)) = d.menus.get(&(plugin.clone(), button.clone())).cloned() else {
            let tip = d.plugins.iter().find(|p| p.plugin == plugin && p.id == button).map_or("", |p| p.tip.as_str());
            ui.scope(|ui| {
                ui.set_max_width(header);
                title(ui, c, tip, 16.0);
                small(ui, c, "Loading…");
            });
            return;
        };
        ui.scope(|ui| {
            ui.set_max_width(header);
            title(ui, c, &menu.title, 16.0);
            if let Some(subtitle) = menu.subtitle.as_deref().filter(|s| !s.is_empty()) {
                small(ui, c, subtitle);
            }
        });
        ui.add_space(8.0);
        let height = (ui.max_rect().bottom() - ui.cursor().top()).max(80.0);
        egui::ScrollArea::vertical().auto_shrink([false, true]).max_height(height).show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            let at = PluginAt { plugin: &plugin, button: &button, arrived };
            for (i, node) in menu.children.iter().enumerate() {
                self.plugin_node(ui, c, d, node, &at, &i.to_string(), actions);
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn plugin_node(&mut self, ui: &mut EUi, c: Colors, d: &Snapshot, node: &Node, at: &PluginAt, path: &str,
                   actions: &mut Vec<Action>) {
        match node {
            Node::Text { text, style } => {
                let (font, color) = match style.as_str() {
                    "title" => (bold(15.0), c.text),
                    "subtle" => (regular(12.5), c.subtext),
                    _ => (regular(13.5), c.text),
                };
                ui.add(egui::Label::new(egui::RichText::new(text).font(font).color(color)).wrap());
            }
            Node::Button { label, style, icon, key } => {
                let icon = icon.as_deref().map(|i| self.plugin_icon(ui.ctx(), i, &d.avatars));
                if plugin_button(ui, c, label, style, icon, key.is_some()).clicked() {
                    at.send(actions, key, "click", None);
                }
            }
            Node::Row { icon, image, title, subtitle, key, trailing } => {
                let height = if subtitle.as_deref().is_some_and(|s| !s.is_empty()) { 56.0 } else { 44.0 };
                let sense = if key.is_some() { Sense::click() } else { Sense::hover() };
                let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), sense);
                if key.is_some() && response.hovered() {
                    ui.painter().rect_filled(rect, CornerRadius::same(12), c.hover);
                }
                let picture = Rect::from_center_size(pos2(rect.min.x + 12.0 + 18.0, rect.center().y), vec2(36.0, 36.0));
                let mut x = rect.min.x + 12.0;
                if let Some(texture) = image.as_deref().and_then(|i| self.texture(ui.ctx(), i, &d.avatars)) {
                    egui::Image::new((texture, picture.size())).corner_radius(CornerRadius::same(8)).paint_at(ui, picture);
                    x = picture.max.x + 12.0;
                } else if let Some(icon) = icon.as_deref() {
                    let icon = self.plugin_icon(ui.ctx(), icon, &d.avatars);
                    ui.painter().circle_filled(picture.center(), 18.0, c.surface);
                    paint_icon(ui.painter(), picture.shrink(8.0), icon, c.text);
                    x = picture.max.x + 12.0;
                }
                let trailing_room = if trailing.is_some() { 120.0 } else { 0.0 };
                two_lines_at(ui.painter(), c, x, rect.center().y, rect.max.x - 12.0 - trailing_room - x, title,
                             subtitle.as_deref().unwrap_or(""), 14.0);
                if let Some(trailing) = trailing {
                    let mut end = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(8.0, 4.0)))
                        .layout(egui::Layout::right_to_left(egui::Align::Center)));
                    self.plugin_node(&mut end, c, d, trailing, at, &format!("{path}.t"), actions);
                }
                if response.clicked() {
                    at.send(actions, key, "click", None);
                }
            }
            Node::Toggle { label, value, key } => {
                let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
                if response.hovered() {
                    ui.painter().rect_filled(rect, CornerRadius::same(12), c.hover);
                }
                let switch = Rect::from_center_size(pos2(rect.max.x - 12.0 - 18.0, rect.center().y), vec2(36.0, 20.0));
                let label_room = switch.min.x - 12.0 - (rect.min.x + 12.0);
                let galley = line(ui.painter(), label, regular(13.5), c.text, label_room);
                ui.painter().galley(pos2(rect.min.x + 12.0, rect.center().y - galley.size().y / 2.0), galley, c.text);
                let track = if *value { c.accent } else { c.hover.lerp_to_gamma(c.text, 0.18) };
                ui.painter().rect_filled(switch, CornerRadius::same(10), track);
                let knob_x = if *value { switch.max.x - 10.0 } else { switch.min.x + 10.0 };
                ui.painter().circle_filled(pos2(knob_x, switch.center().y), 7.0, if *value { c.on_accent } else { c.text });
                if response.clicked() {
                    at.send(actions, key, "change", Some(serde_json::Value::Bool(!value)));
                }
            }
            Node::Input { placeholder, value, key } => {
                let entry = self.inputs.entry(format!("{}/{}/{path}", at.plugin, at.button))
                    .or_insert_with(|| (value.clone(), value.clone()));
                if entry.0 != *value {
                    *entry = (value.clone(), value.clone());
                }
                let response = ui.scope(|ui| {
                    // no outlines: a field is a surface, like the rest
                    let visuals = ui.visuals_mut();
                    for state in [&mut visuals.widgets.inactive, &mut visuals.widgets.hovered, &mut visuals.widgets.active] {
                        state.bg_stroke = egui::Stroke::NONE;
                        state.corner_radius = CornerRadius::same(10);
                    }
                    visuals.selection.stroke = egui::Stroke::NONE;
                    visuals.selection.bg_fill = c.accent.gamma_multiply(0.35);
                    visuals.text_cursor.stroke = egui::Stroke::new(1.5, c.text);
                    ui.add(egui::TextEdit::singleline(&mut entry.1)
                        .hint_text(egui::RichText::new(placeholder).font(regular(13.5)).color(c.subtext))
                        .font(regular(13.5)).text_color(c.text).background_color(c.surface)
                        .margin(vec2(12.0, 9.0)).desired_width(f32::INFINITY))
                }).inner;
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let text = entry.1.clone();
                    at.send(actions, key, "submit", Some(serde_json::Value::String(text)));
                }
            }
            Node::Stack { direction, gap, children } => {
                let gap = match gap.as_str() {
                    "small" => 4.0,
                    "medium" => 12.0,
                    _ => 8.0,
                };
                let mut draw = |ui: &mut EUi| {
                    ui.spacing_mut().item_spacing = vec2(gap, gap);
                    for (i, child) in children.iter().enumerate() {
                        self.plugin_node(ui, c, d, child, at, &format!("{path}.{i}"), actions);
                    }
                };
                if direction == "row" {
                    ui.horizontal_wrapped(|ui| draw(ui));
                } else {
                    ui.vertical(|ui| draw(ui));
                }
            }
            Node::Space { size } => ui.add_space(if size == "medium" { 16.0 } else { 8.0 }),
            Node::Progress { value, max, rate, times } => {
                // moves on by itself between menus (a song playing), so it's smooth
                let now = (value + rate * at.arrived.elapsed().as_secs_f64()).clamp(0.0, max.max(0.0));
                let filled = if *max > 0.0 { (now / max) as f32 } else { 0.0 };
                let height = if *times { 26.0 } else { 6.0 };
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
                let track = Rect::from_min_size(rect.min + vec2(0.0, 1.0), vec2(rect.width(), 4.0));
                ui.painter().rect_filled(track, CornerRadius::same(2), c.hover.lerp_to_gamma(c.text, 0.12));
                ui.painter().rect_filled(Rect::from_min_size(track.min, vec2(track.width() * filled, 4.0)),
                                         CornerRadius::same(2), c.accent);
                if *times {
                    let clock = |s: f64| format!("{}:{:02}", (s as u64) / 60, (s as u64) % 60);
                    let painter = ui.painter();
                    painter.text(pos2(rect.min.x, rect.max.y), Align2::LEFT_BOTTOM, clock(now), regular(11.5), c.subtext);
                    painter.text(rect.max, Align2::RIGHT_BOTTOM, clock(*max), regular(11.5), c.subtext);
                }
                // keep moving while shown
                if *rate != 0.0 {
                    ui.ctx().request_repaint();
                }
            }
            Node::Image { image, size } => {
                let side = match size.as_str() {
                    "small" => 48.0,
                    "large" => 160.0,
                    _ => 96.0,
                };
                let (rect, _) = ui.allocate_exact_size(vec2(side, side), Sense::hover());
                match self.texture(ui.ctx(), image, &d.avatars) {
                    Some(texture) => {
                        egui::Image::new((texture, rect.size())).corner_radius(CornerRadius::same(12)).paint_at(ui, rect);
                    }
                    // still on its way
                    None => {
                        ui.painter().rect_filled(rect, CornerRadius::same(12), c.surface);
                    }
                }
            }
            Node::Unknown => {}
        }
    }

    fn open_browser(&mut self, actions: &mut Vec<Action>, place: &str) {
        self.open_drop(Drop::Servers);
        if self.drop != Drop::Servers {
            return;
        }
        if self.browser.place != place {
            self.browser = Browser { place: place.into(), ..Default::default() };
        }
        self.ask_page(actions, None);
    }

    fn ask_page(&mut self, actions: &mut Vec<Action>, cursor: Option<String>) {
        if cursor.is_none() {
            self.browser.error.clear();
        }
        self.browser.waiting = Some(cursor.clone());
        actions.push(Action::Send(ToMace::Servers { place: self.browser.place.clone(), sort: self.browser.sort, cursor }));
    }

    fn take_page(&mut self, page: &ServerPage) {
        let b = &mut self.browser;
        if page.place != b.place || page.sort != b.sort || b.waiting.as_ref() != Some(&page.cursor) {
            return;
        }
        b.waiting = None;
        if page.cursor.is_none() {
            b.list.clear();
        }
        for s in &page.list {
            if !b.list.iter().any(|x| x.id == s.id) {
                b.list.push(s.clone());
            }
        }
        b.next = page.next.clone();
        b.error = page.error.clone();
    }

    fn join(&mut self, actions: &mut Vec<Action>, me: &str, server: &PublicServer) {
        let target = format!("{} {}", self.browser.place, server.id);
        self.run(actions, &["start", me, &target]);
        self.say("Joining server…");
        actions.push(Action::Close);
    }

    fn servers(&mut self, ui: &mut EUi, c: Colors, d: &Snapshot, actions: &mut Vec<Action>) {
        let Some(me) = d.me() else { return };
        let Some(game) = me.server.clone() else {
            if self.drop == Drop::Servers {
                self.drop = Drop::None;
            }
            return;
        };
        let here = game.job.clone();
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.set_width(ui.available_width() - 40.0);
                title(ui, c, "Servers", 16.0);
                small(ui, c, game.name.as_deref().unwrap_or("This game"));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // the card's pin is at the far end
                ui.add_space(36.0);
                if icon_button(ui, c, Icon::Refresh, "Refresh", None).clicked() {
                    self.ask_page(actions, None);
                }
            });
        });
        ui.add_space(4.0);

        // quick picks, from what's loaded; then the order
        let others: Vec<PublicServer> = self.browser.list.iter().filter(|s| s.id != here).cloned().collect();
        let emptiest = others.iter().filter(|s| s.playing > 0).min_by_key(|s| s.playing).cloned();
        let fullest = others.iter().filter(|s| s.playing < s.max).max_by_key(|s| s.playing).cloned();
        ui.horizontal_wrapped(|ui| {
            for (label, pick) in [("Emptiest", emptiest), ("Fullest", fullest)] {
                let response = ui.add_enabled_ui(pick.is_some(), |ui| text_button(ui, c, label, false)).inner;
                if response.clicked()
                    && let Some(server) = pick
                {
                    self.join(actions, &me.id, &server);
                }
            }
        });
        ui.horizontal(|ui| {
            for (label, sort) in [("Most players", Sort::Most), ("Fewest players", Sort::Fewest)] {
                if text_button(ui, c, label, self.browser.sort == sort).clicked() && self.browser.sort != sort {
                    self.browser.sort = sort;
                    self.browser.list.clear();
                    self.ask_page(actions, None);
                }
            }
        });
        ui.add_space(4.0);

        if self.browser.list.is_empty() {
            if !self.browser.error.is_empty() {
                small(ui, c, &self.browser.error.clone());
            } else if self.browser.waiting.is_some() {
                small(ui, c, "Loading servers…");
            } else {
                small(ui, c, "No open servers");
            }
            return;
        }
        let mut chosen = None;
        let mut more = false;
        let height = (ui.max_rect().bottom() - ui.cursor().top()).max(120.0);
        egui::ScrollArea::vertical().auto_shrink([false, true]).max_height(height).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            for s in &self.browser.list {
                if server_row(ui, c, s, s.id == here).clicked() && s.id != here {
                    chosen = Some(s.clone());
                }
            }
            if self.browser.next.is_some() {
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::hover());
                ui.painter().text(rect.center(), Align2::CENTER_CENTER,
                                  if self.browser.waiting.is_some() { "Loading more…" } else { "More below" },
                                  regular(12.5), c.subtext);
                // reaching the end loads the next page
                if ui.is_rect_visible(rect) && self.browser.waiting.is_none() {
                    more = true;
                }
            }
        });
        if more {
            let cursor = self.browser.next.clone();
            self.ask_page(actions, cursor);
        }
        if let Some(server) = chosen {
            self.join(actions, &me.id, &server);
        }
    }

    // -- toasts

    /// At the top of the game while the bar is closed (sliding down), above the
    /// bottom edge while it's open; newest first, three at most.
    fn draw_toasts(&mut self, ctx: &egui::Context, c: Colors, screen: Rect, open: bool) {
        self.toasts.retain(|t| t.at.elapsed().as_secs_f32() < TOAST_SECONDS);
        if self.toasts.is_empty() {
            return;
        }
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("mace-toasts")));
        let mut edge = if open { screen.max.y - 24.0 } else { screen.min.y + BAR_TOP };
        for t in self.toasts.iter().rev().take(3) {
            let age = t.at.elapsed().as_secs_f32();
            let fade = ((TOAST_SECONDS - age) / 0.4).clamp(0.0, 1.0) * ease(age / 0.25).min(1.0);
            let appear = ease(age / 0.25);
            let wrap = 440.0;
            let head = painter.layout(t.title.clone(), bold(14.0), c.text.gamma_multiply(fade), wrap);
            let body = (!t.body.is_empty())
                .then(|| painter.layout(plain_location(&t.body), regular(12.5), c.subtext.gamma_multiply(fade), wrap));
            let inner = vec2(head.size().x.max(body.as_ref().map_or(0.0, |b| b.size().x)),
                             head.size().y + body.as_ref().map_or(0.0, |b| b.size().y + 2.0));
            let size = inner + vec2(40.0, 28.0);
            let top = if open {
                edge - size.y + (1.0 - appear) * 12.0
            } else {
                edge - (1.0 - appear) * (size.y + BAR_TOP + 8.0)
            };
            let rect = Rect::from_min_size(pos2(screen.center().x - size.x / 2.0, top), size);
            let mut lift = shadow();
            lift.color = lift.color.gamma_multiply(fade);
            painter.add(lift.as_shape(rect, CornerRadius::same(18)));
            painter.rect_filled(rect, CornerRadius::same(18), c.background.gamma_multiply(fade));
            let mut at = rect.min + vec2(20.0, 14.0);
            let head_height = head.size().y;
            painter.galley(at, head, c.text);
            at.y += head_height + 2.0;
            if let Some(body) = body {
                painter.galley(at, body, c.subtext);
            }
            edge = if open { rect.min.y - 8.0 } else { rect.max.y + 8.0 };
        }
    }
}

fn server_row(ui: &mut EUi, c: Colors, s: &PublicServer, here: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 56.0), Sense::click());
    if here {
        ui.painter().rect_filled(rect, CornerRadius::same(12), c.surface);
    } else if response.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(12), c.hover);
    }
    let inner = rect.shrink2(vec2(14.0, 11.0));
    // fullness: a bar and the count
    let bar = Rect::from_min_size(inner.min + vec2(0.0, 4.0), vec2(72.0, 6.0));
    ui.painter().rect_filled(bar, CornerRadius::same(3), c.hover.lerp_to_gamma(c.text, 0.06));
    let filled = if s.max > 0 { (s.playing as f32 / s.max as f32).clamp(0.0, 1.0) } else { 0.0 };
    ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * filled, bar.height())), CornerRadius::same(3), c.accent);
    ui.painter().text(pos2(bar.min.x, inner.max.y), Align2::LEFT_BOTTOM, format!("{}/{}", s.playing, s.max), regular(12.5), c.subtext);
    // the server's id; under it your ping and its place, when one of your accounts has been there
    let mut facts: Vec<String> = s.ping.map(|ping| format!("~{ping} ms")).into_iter().collect();
    facts.extend(s.location.as_deref().map(plain_location).filter(|p| !p.is_empty()));
    let facts = facts.join(" · ");
    let middle = pos2(inner.min.x + 96.0, inner.center().y);
    // up to the row's end, or to "Current"
    let width = inner.max.x - middle.x - if here { 72.0 } else { 0.0 };
    let painter = ui.painter();
    let id_at = if facts.is_empty() { 0.0 } else { -8.0 };
    for (text, font, color, dy) in [(s.id.as_str(), regular(13.5), c.text, id_at), (facts.as_str(), regular(12.5), c.subtext, 9.0)] {
        if !text.is_empty() {
            let galley = line(painter, text, font, color, width);
            painter.galley(middle + vec2(0.0, dy - galley.size().y / 2.0), galley, color);
        }
    }
    if here {
        ui.painter().text(pos2(inner.max.x, inner.center().y), Align2::RIGHT_CENTER, "Current", bold(12.5), c.text);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_are_dropped_from_places() {
        assert_eq!(plain_location("🇩🇪 Frankfurt am Main, Hesse, DE"), "Frankfurt am Main, Hesse, DE");
        assert_eq!(plain_location("Paris"), "Paris");
        assert_eq!(plain_location("Server in 🇩🇪 Frankfurt"), "Server in Frankfurt");
    }

    #[test]
    fn statuses_read_like_the_old_panel() {
        let mut a = Account { running: true, fps: Some(144), ..Default::default() };
        assert_eq!(status(&a, None), "not in a game");
        a.server = Some(mace_proto::Server { job: "j".into(), name: Some("Obby".into()), ..Default::default() });
        assert_eq!(status(&a, Some("j")), "here · 144 FPS");
        assert_eq!(status(&a, Some("k")), "in Obby · 144 FPS");
        a.launch = Some("starting".into());
        assert_eq!(status(&a, Some("j")), "starting…");
    }
}

#[cfg(test)]
mod clicks {
    use super::*;
    use egui::Pos2;

    fn snapshot(toasts: Vec<(String, String)>) -> Snapshot {
        let me = Account { id: "main".into(), name: "Main".into(), running: true, ..Default::default() };
        Snapshot {
            connected: true, config: Some(Config { me: "main".into(), ..Default::default() }),
            state: Some(Arc::new(State { now: 7200.0, accounts: vec![me], games: vec![Game {
                universe: "1".into(), place: "1537690962".into(), name: "Bee Swarm Simulator".into(),
                account: "main".into(), when: 0.0 }] })),
            avatars: HashMap::new(),
            pages: Vec::new(), done: Vec::new(), toasts,
            plugins: Vec::new(), menus: HashMap::new(), copies: Vec::new(), close: false,
        }
    }

    fn frame(ctx: &egui::Context, ui: &mut Ui, progress: f32, events: Vec<egui::Event>, escape: bool,
             toasts: Vec<(String, String)>) -> Vec<Action> {
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1280.0, 800.0))),
            events,
            ..Default::default()
        };
        let mut actions = Vec::new();
        let data = snapshot(toasts);
        let output = ctx.run_ui(raw, |root| actions = ui.show(root, progress, &data, escape));
        let mut deltas = output.textures_delta;
        deltas.clear();
        actions
    }

    fn click(at: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }
    }

    fn clicked(ctx: &egui::Context, ui: &mut Ui, at: Pos2) -> Vec<Action> {
        frame(ctx, ui, 1.0, vec![egui::Event::PointerMoved(at)], false, vec![]);
        frame(ctx, ui, 1.0, vec![click(at, true)], false, vec![]);
        frame(ctx, ui, 1.0, vec![click(at, false)], false, vec![])
    }

    #[test]
    fn a_click_on_the_game_closes_and_one_on_the_bar_doesnt() {
        let ctx = egui::Context::default();
        setup(&ctx);
        let mut ui = Ui::default();
        ui.opened();
        frame(&ctx, &mut ui, 1.0, vec![], false, vec![]);
        frame(&ctx, &mut ui, 1.0, vec![], false, vec![]);
        // the bar's empty middle
        let after = clicked(&ctx, &mut ui, pos2(640.0, BAR_TOP + BAR_HEIGHT / 2.0));
        assert!(!after.iter().any(|a| matches!(a, Action::Close)), "the bar isn't the game");
        let after = clicked(&ctx, &mut ui, pos2(300.0, 500.0));
        assert!(after.iter().any(|a| matches!(a, Action::Close)));
    }

    #[test]
    fn escape_closes_a_drop_down_first() {
        let ctx = egui::Context::default();
        setup(&ctx);
        let mut ui = Ui::default();
        ui.drop = Drop::Alts;
        assert!(frame(&ctx, &mut ui, 1.0, vec![], true, vec![]).is_empty());
        assert!(ui.drop == Drop::None);
        assert!(frame(&ctx, &mut ui, 1.0, vec![], true, vec![]).iter().any(|a| matches!(a, Action::Close)));
    }

    #[test]
    fn a_recent_game_starts_here() {
        let ctx = egui::Context::default();
        setup(&ctx);
        let mut ui = Ui { drop: Drop::Games, ..Default::default() };
        frame(&ctx, &mut ui, 1.0, vec![], false, vec![]);
        frame(&ctx, &mut ui, 1.0, vec![], false, vec![]);
        // the first row, under the drop-down's title (it hangs from the bar's trailing end)
        let width = BAR_WIDEST.min(1280.0 - 48.0);
        let right = 640.0 + width / 2.0;
        let at = pos2(right - 220.0, BAR_TOP + BAR_HEIGHT + 8.0 + 20.0 + 40.0 + 28.0);
        let actions = clicked(&ctx, &mut ui, at);
        let ran: Vec<&Vec<String>> = actions.iter().filter_map(|a| match a {
            Action::Send(ToMace::Run { args, .. }) => Some(args),
            _ => None,
        }).collect();
        assert_eq!(ran, [&vec!["start".to_string(), "main".into(), "roblox://experiences/start?placeId=1537690962".into()]]);
        assert!(actions.iter().any(|a| matches!(a, Action::Close)));
        assert_eq!(ago(0.0, 7200.0), "2 h ago");
    }

    #[test]
    fn toasts_show_while_closed_and_go() {
        let ctx = egui::Context::default();
        setup(&ctx);
        let mut ui = Ui::default();
        assert!(!ui.has_toasts());
        frame(&ctx, &mut ui, 0.0, vec![], false, vec![("Joined Obby".into(), "Server in Paris".into())]);
        assert!(ui.has_toasts());
        ui.toasts[0].at -= std::time::Duration::from_secs_f32(TOAST_SECONDS + 0.1);
        frame(&ctx, &mut ui, 0.0, vec![], false, vec![]);
        assert!(!ui.has_toasts());
    }

    fn sent_plugin(actions: &[Action]) -> Vec<(String, String, Option<serde_json::Value>)> {
        actions.iter().filter_map(|a| match a {
            Action::Send(ToMace::Plugin { key, what, value, .. }) => Some((key.clone(), what.clone(), value.clone())),
            _ => None,
        }).collect()
    }

    #[test]
    fn a_plugin_button_opens_its_menu_and_mace_hears_of_it() {
        let ctx = egui::Context::default();
        setup(&ctx);
        let mut ui = Ui::default();
        let mut data = snapshot(vec![]);
        data.plugins = vec![PluginButton { plugin: "p".into(), id: "b".into(), icon: "star".into(), tip: "P".into(), badge: Some(2) }];
        let run = |ui: &mut Ui, events: Vec<egui::Event>, data: &Snapshot| {
            let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1280.0, 800.0))), events,
                                       ..Default::default() };
            let mut actions = Vec::new();
            let mut deltas = ctx.run_ui(raw, |root| actions = ui.show(root, 1.0, data, false)).textures_delta;
            deltas.clear();
            actions
        };
        run(&mut ui, vec![], &data);
        // left of the seven built-in buttons, which go from the bar's trailing end
        let width = BAR_WIDEST.min(1280.0 - 48.0);
        let x = 640.0 + width / 2.0 - INSET - 7.0 * (CONTROL + BUTTON_GAP) - CONTROL / 2.0;
        let at = pos2(x, BAR_TOP + BAR_HEIGHT / 2.0);
        run(&mut ui, vec![egui::Event::PointerMoved(at)], &data);
        run(&mut ui, vec![click(at, true)], &data);
        let actions = run(&mut ui, vec![click(at, false)], &data);
        assert!(ui.drop == Drop::Plugin);
        assert_eq!(sent_plugin(&actions), [(String::new(), "open".to_string(), None)]);
        // its button goes away: the menu closes, and Mace hears that too
        data.plugins.clear();
        let actions = run(&mut ui, vec![], &data);
        assert!(ui.drop == Drop::None);
        assert_eq!(sent_plugin(&actions), [(String::new(), "close".to_string(), None)]);
    }

    /// One block of a plugin's menu, drawn at the top left of a 300-point column.
    fn block(ctx: &egui::Context, ui: &mut Ui, node: &Node, events: Vec<egui::Event>) -> Vec<Action> {
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0))), events,
                                   ..Default::default() };
        let data = snapshot(vec![]);
        let mut actions = Vec::new();
        let at = PluginAt { plugin: "p", button: "b", arrived: Instant::now() };
        let mut deltas = ctx.run_ui(raw, |root| {
            egui::Area::new(egui::Id::unique("block")).order(egui::Order::Foreground).fade_in(false).constrain(false).fixed_pos(Pos2::ZERO).show(root.ctx(), |area| {
                area.set_width(300.0);
                ui.plugin_node(area, Colors::of(&Theme::default()), &data, node, &at, "0", &mut actions);
            });
        }).textures_delta;
        deltas.clear();
        actions
    }

    fn click_block(ctx: &egui::Context, ui: &mut Ui, node: &Node, at: Pos2) -> Vec<Action> {
        block(ctx, ui, node, vec![]);
        block(ctx, ui, node, vec![egui::Event::PointerMoved(at)]);
        block(ctx, ui, node, vec![click(at, true)]);
        block(ctx, ui, node, vec![click(at, false)])
    }

    #[test]
    fn plugin_controls_say_what_was_done() {
        let ctx = egui::Context::default();
        setup(&ctx);
        let mut ui = Ui::default();
        let button = Node::Button { label: "Go".into(), style: "primary".into(), icon: Some("bolt".into()), key: Some("k1".into()) };
        assert_eq!(sent_plugin(&click_block(&ctx, &mut ui, &button, pos2(12.0, 18.0))), [("k1".into(), "click".into(), None)]);
        let toggle = Node::Toggle { label: "Loud".into(), value: false, key: Some("k2".into()) };
        assert_eq!(sent_plugin(&click_block(&ctx, &mut ui, &toggle, pos2(150.0, 20.0))),
                   [("k2".into(), "change".into(), Some(serde_json::Value::Bool(true)))]);
        let row = Node::Row { icon: Some("star".into()), image: None, title: "A row".into(), subtitle: Some("under".into()),
                              key: Some("k3".into()), trailing: None };
        assert_eq!(sent_plugin(&click_block(&ctx, &mut ui, &row, pos2(150.0, 28.0))), [("k3".into(), "click".into(), None)]);
        // no key: nothing to say
        let inert = Node::Button { label: "Off".into(), style: String::new(), icon: None, key: None };
        assert!(sent_plugin(&click_block(&ctx, &mut ui, &inert, pos2(12.0, 18.0))).is_empty());
        // typing, then Enter
        let input = Node::Input { placeholder: "Say".into(), value: String::new(), key: Some("k4".into()) };
        click_block(&ctx, &mut ui, &input, pos2(150.0, 18.0));
        block(&ctx, &mut ui, &input, vec![egui::Event::Text("hello".into())]);
        let enter = egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false,
                                       modifiers: Default::default() };
        let actions = block(&ctx, &mut ui, &input, vec![enter]);
        assert_eq!(sent_plugin(&actions), [("k4".into(), "submit".into(), Some(serde_json::Value::String("hello".into())))]);
    }

    #[test]
    fn every_icon_a_plugin_can_name_is_here() {
        assert_eq!(NAMED, &mace_proto::ICONS[..]);
        for name in mace_proto::ICONS {
            let icon = named_icon(name).unwrap();
            assert_eq!(icon.pixels(48).len(), 48 * 48, "{name}");
        }
        assert!(named_icon("nope").is_none());
    }

    #[test]
    fn a_plugin_menu_draws() {
        let ctx = egui::Context::default();
        setup(&ctx);
        let mut ui = Ui { drop: Drop::Plugin, plugin_drop: Some(("notes".into(), "notes".into())), ..Default::default() };
        let mut data = snapshot(vec![]);
        data.plugins = vec![PluginButton { plugin: "notes".into(), id: "notes".into(), icon: "sticky_note_2".into(),
                                           tip: "Notes".into(), badge: None }];
        let menu: Menu = serde_json::from_str(r#"{"title":"Notes","subtitle":"Sample Obby","children":[
            {"type":"text","text":"The secret door is behind the waterfall.","style":"normal"},
            {"type":"input","placeholder":"Change the note","value":"x","key":"k1"},
            {"type":"button","label":"Clear","style":"danger","key":"k2"}]}"#).unwrap();
        data.menus.insert(("notes".into(), "notes".into()), (Arc::new(menu), Instant::now()));
        for _ in 0..3 {
            let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1280.0, 800.0))),
                                       ..Default::default() };
            let mut deltas = ctx.run_ui(raw, |root| { ui.show(root, 1.0, &data, false); }).textures_delta;
            deltas.clear();
        }
    }

    fn sent_pins(actions: &[Action]) -> Option<Vec<String>> {
        actions.iter().rev().find_map(|a| match a {
            Action::Send(ToMace::Pins { pins }) => Some(pins.iter().map(|p| p.id.clone()).collect()),
            _ => None,
        })
    }

    #[test]
    fn a_drop_down_pinned_stays_and_its_plugin_keeps_drawing() {
        let ctx = egui::Context::default();
        setup(&ctx);
        let mut ui = Ui { drop: Drop::Plugin, plugin_drop: Some(("p".into(), "b".into())), ..Default::default() };
        let mut data = snapshot(vec![]);
        data.plugins = vec![PluginButton { plugin: "p".into(), id: "b".into(), icon: "star".into(), tip: "P".into(), badge: None }];
        let menu: Menu = serde_json::from_str(r#"{"title":"P","children":[{"type":"progress","value":5,"max":10,"rate":1}]}"#).unwrap();
        data.menus.insert(("p".into(), "b".into()), (Arc::new(menu), Instant::now()));
        let run = |ui: &mut Ui, progress: f32, events: Vec<egui::Event>, data: &Snapshot| {
            let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1280.0, 800.0))), events,
                                       ..Default::default() };
            let mut actions = Vec::new();
            let mut deltas = ctx.run_ui(raw, |root| actions = ui.show(root, progress, data, false)).textures_delta;
            deltas.clear();
            actions
        };
        assert_eq!(sent_plugin(&run(&mut ui, 1.0, vec![], &data)), [(String::new(), "open".to_string(), None)]);
        // the pin, in the drop-down's top right corner (it hangs from the bar's trailing end)
        let width = BAR_WIDEST.min(1280.0 - 48.0);
        let right = 640.0 + width / 2.0;
        let pin = pos2(right - 14.0 - 16.0, BAR_TOP + BAR_HEIGHT + 8.0 + 14.0 + 16.0);
        run(&mut ui, 1.0, vec![egui::Event::PointerMoved(pin)], &data);
        run(&mut ui, 1.0, vec![click(pin, true)], &data);
        let actions = run(&mut ui, 1.0, vec![click(pin, false)], &data);
        assert_eq!(sent_pins(&actions), Some(vec!["plugin:p/b".to_string()]));
        assert!(ui.drop == Drop::None);
        // closed: still drawn, and the plugin still told it's showing (no "close")
        let actions = run(&mut ui, 0.0, vec![], &data);
        assert!(sent_plugin(&actions).is_empty(), "{:?}", sent_plugin(&actions));
        assert!(ui.is_pinned("plugin:p/b"));
        // Mace's copy of the pins comes back the same
        data.config.as_mut().unwrap().pins = ui.pins.clone();
        run(&mut ui, 0.0, vec![], &data);
        // unpinned (as another game's panel might): closed, so the plugin hears it's gone
        data.config.as_mut().unwrap().pins.clear();
        assert_eq!(sent_plugin(&run(&mut ui, 0.0, vec![], &data)), [(String::new(), "close".to_string(), None)]);
    }

    #[test]
    fn the_bar_pins_itself() {
        let ctx = egui::Context::default();
        setup(&ctx);
        let mut ui = Ui::default();
        frame(&ctx, &mut ui, 1.0, vec![], false, vec![]);
        // second from the trailing end, after Mace Settings
        let width = BAR_WIDEST.min(1280.0 - 48.0);
        let x = 640.0 + width / 2.0 - INSET - (CONTROL + BUTTON_GAP) - CONTROL / 2.0;
        let actions = clicked(&ctx, &mut ui, pos2(x, BAR_TOP + BAR_HEIGHT / 2.0));
        assert_eq!(sent_pins(&actions), Some(vec!["bar".to_string()]));
        assert!(ui.is_pinned(BAR_PIN));
    }
}

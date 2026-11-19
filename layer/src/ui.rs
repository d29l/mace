//! The panel: a bar floating at the top of the game (who you're playing as,
//! this server, then icon buttons: rejoin and copy the link; servers, recent
//! games and alts; Mace Settings) with drop-downs under it, and toasts.
//! Everything shown comes from Mace; every button is a request to Mace (or a
//! copy to the clipboard, done right here).
//!
//! Layout follows the usual rules for toolbars: an 8-point grid, icon buttons
//! with tooltips, groups kept apart by space rather than lines, settings at the
//! trailing end, 40-point targets, inner corners following the outer ones, and
//! no borders (a soft shadow lifts things off the game).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use egui::{Align2, Color32, CornerRadius, FontFamily, FontId, Rect, Response, Sense, Ui as EUi, pos2, vec2};
use mace_proto::{Account, Config, Game, PublicServer, ServerPage, Sort, State, Theme, ToMace};

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
        style.interaction.tooltip_delay = 0.5;
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

/// Icons from GNOME's Adwaita theme, the ones Mace Settings shows
/// (`dev/icons.sh` makes them; layer/icons/README.md says whose they are).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Icon {
    Refresh,
    Copy,
    Servers,
    Games,
    Alts,
    Settings,
    Close,
    Focus,
    Volume,
    Muted,
}

/// The pixel sizes each icon comes in, in the order its file has them.
const ICON_SIZES: [u32; 6] = [16, 20, 24, 32, 40, 48];

impl Icon {
    fn coverage(self) -> &'static [u8] {
        match self {
            Icon::Refresh => include_bytes!("../icons/view-refresh.a8"),
            Icon::Copy => include_bytes!("../icons/edit-copy.a8"),
            Icon::Servers => include_bytes!("../icons/network-server.a8"),
            Icon::Games => include_bytes!("../icons/input-gaming.a8"),
            Icon::Alts => include_bytes!("../icons/system-users.a8"),
            Icon::Settings => include_bytes!("../icons/emblem-system.a8"),
            Icon::Close => include_bytes!("../icons/window-close.a8"),
            Icon::Focus => include_bytes!("../icons/focus-windows.a8"),
            Icon::Volume => include_bytes!("../icons/audio-volume-high.a8"),
            Icon::Muted => include_bytes!("../icons/audio-volume-muted.a8"),
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

/// A round icon button (32 points, a 16-point icon) for rows and headers.
fn icon_button(ui: &mut EUi, c: Colors, icon: Icon, tip: &str, tint: Option<Color32>) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::click());
    if response.hovered() {
        ui.painter().circle_filled(rect.center(), 16.0, c.hover);
    }
    paint_icon(ui.painter(), Rect::from_center_size(rect.center(), vec2(16.0, 16.0)), icon, tint.unwrap_or(c.text));
    response.on_hover_text(tip)
}

/// A button in the bar.
struct BarButton {
    salt: &'static str,
    icon: Icon,
    tip: &'static str,
    /// Its drop-down is open.
    selected: bool,
    enabled: bool,
    /// Counts something, in a small pill at its top right.
    badge: Option<usize>,
}

/// A bar button: a 20-point icon in a round 40-point target, flat until
/// hovered and filled while its drop-down is open.
fn bar_button(ui: &mut EUi, c: Colors, rect: Rect, b: &BarButton) -> Response {
    let BarButton { salt, icon, selected, enabled, badge, .. } = *b;
    let response = ui.interact(rect, ui.scope_id().with(salt), if enabled { Sense::click() } else { Sense::hover() });
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
    paint_icon(painter, Rect::from_center_size(rect.center(), vec2(20.0, 20.0)), icon, ink);
    if let Some(n) = badge.filter(|n| *n > 0) {
        let digits = painter.layout_no_wrap(n.to_string(), bold(10.5), c.on_accent);
        let pill = Rect::from_min_size(pos2(rect.center().x + 3.0, rect.min.y + 3.0),
                                       vec2(16.0f32.max(digits.size().x + 8.0), 16.0));
        // a ring of the bar's colour keeps it off the icon
        painter.rect_filled(pill.expand(2.0), CornerRadius::same(10), c.background);
        painter.rect_filled(pill, CornerRadius::same(8), if enabled { c.accent } else { c.subtext });
        painter.galley(pill.center() - digits.size() / 2.0, digits, c.on_accent);
    }
    response.on_hover_text(b.tip)
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
}

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
        self.drop = if self.drop == drop { Drop::None } else { drop };
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

        if progress > 0.0 {
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
            // comes down from above the top edge, fading in on the way
            let opacity = (progress * 1.6).min(1.0);
            let width = BAR_WIDEST.min(screen.width() - 48.0).max(300.0);
            let top = screen.min.y + BAR_TOP - (1.0 - progress) * (BAR_HEIGHT + BAR_TOP + 16.0);
            let bar = Rect::from_min_size(pos2(screen.center().x - width / 2.0, top), vec2(width, BAR_HEIGHT));
            self.bar(&ctx, c, d, bar, opacity, &mut actions);
            if self.drop != Drop::None {
                // hangs from the bar's trailing end, under the buttons that open it
                let w = f32::min(if self.drop == Drop::Servers { 520.0 } else { 440.0 }, bar.width());
                let at = pos2(bar.max.x - w, bar.max.y + 8.0);
                // never down to the bottom edge, where toasts show while the bar is open
                let room = (screen.max.y - at.y - 120.0).clamp(160.0, 600.0);
                self.dropdown(&ctx, c, d, Rect::from_min_size(at, vec2(w, room)), opacity, &mut actions);
            }
        }
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

                // the actions, from the trailing end, in groups kept apart by space:
                // Mace Settings; the drop-downs (servers, games, alts); this server (copy its link, rejoin)
                let in_game = me.server.is_some();
                let alts = d.state.as_ref().map_or(0, |s| s.accounts.iter().filter(|a| a.id != me.id && a.running).count());
                let open = |drop: Drop| self.drop == drop;
                let button = |salt, icon, tip, selected, enabled, badge| BarButton { salt, icon, tip, selected, enabled, badge };
                let groups = [
                    vec![button("settings", Icon::Settings, "Mace Settings", false, true, None)],
                    vec![button("alts", Icon::Alts, "Alts", open(Drop::Alts), true, Some(alts)),
                         button("games", Icon::Games, "Recent games", open(Drop::Games), true, None),
                         button("servers", Icon::Servers, "Servers", open(Drop::Servers), in_game, None)],
                    vec![button("link", Icon::Copy, "Copy join link", false, in_game, None),
                         button("rejoin", Icon::Refresh, "Rejoin", false, in_game, None)],
                ];
                let mut right = bar.max.x - INSET;
                let mut clicked = Vec::new();
                for group in groups {
                    for b in &group {
                        let rect = Rect::from_min_max(pos2(right - CONTROL, mid - CONTROL / 2.0), pos2(right, mid + CONTROL / 2.0));
                        right = rect.min.x - 4.0;
                        if bar_button(ui, c, rect, b).clicked() {
                            clicked.push(b.salt);
                        }
                    }
                    right -= 12.0;
                }
                right += 16.0;
                for salt in clicked {
                    match salt {
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
                        _ => {
                            self.run(actions, &["rejoin", &me.id]);
                            self.say("Rejoining…");
                            actions.push(Action::Close);
                        }
                    }
                }

                // you, then this server, in the space left (with room to breathe before the buttons)
                let room = right + 8.0 - 24.0 - text_left;
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

    fn dropdown(&mut self, ctx: &egui::Context, c: Colors, d: &Snapshot, room: Rect, opacity: f32,
                actions: &mut Vec<Action>) {
        egui::Area::new(egui::Id::unique("mace-drop")).order(egui::Order::Foreground).fade_in(false).constrain(false)
            .fixed_pos(room.min)
            .show(ctx, |ui| {
                ui.set_opacity(opacity);
                let margin = 20.0;
                egui::Frame::new().fill(c.background).corner_radius(CornerRadius::same(20)).inner_margin(margin)
                    .shadow(shadow())
                    .show(ui, |ui| {
                        ui.set_width(room.width() - 2.0 * margin);
                        ui.set_max_height(room.height() - 2.0 * margin);
                        match self.drop {
                            Drop::Servers => self.servers(ui, c, d, actions),
                            Drop::Alts => self.alts(ui, c, d, actions),
                            Drop::Games => self.games(ui, c, d, actions),
                            Drop::None => {}
                        }
                    });
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
                paint_icon(ui.painter(), icon.shrink(8.0), Icon::Games, c.subtext);
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
            self.drop = Drop::None;
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
        let closest = others.iter().filter(|s| s.ping.is_some()).min_by_key(|s| s.ping).cloned();
        ui.horizontal_wrapped(|ui| {
            for (label, pick) in [("Emptiest", emptiest), ("Fullest", fullest), ("Lowest ping", closest)] {
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
    // the server's id; under it the ping, and the place when one of your accounts has been there
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
}

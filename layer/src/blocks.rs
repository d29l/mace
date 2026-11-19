//! Plugin menus, drawn from Mace's blocks (`mace_proto::Node`). Each block is
//! measured, then placed in a rectangle, so a row can centre its blocks, let
//! one grow into the room the others leave and line them up across, and an
//! overlay can lay blocks over another (controls on a cover).

use std::time::Instant;

use egui::{Align2, Color32, CornerRadius, FontId, Rect, Sense, Ui as EUi, Vec2, pos2, vec2};
use mace_proto::Node;

use super::{Action, Colors, PluginAt, Snapshot, Ui, bold, paint_icon, regular, small, title, two_lines_at};

fn gap_of(name: &str) -> f32 {
    match name {
        "none" => 0.0,
        "small" => 4.0,
        "medium" => 12.0,
        "large" => 20.0,
        _ => 8.0,
    }
}

fn space_of(name: &str) -> f32 {
    match name {
        "medium" => 16.0,
        "large" => 24.0,
        _ => 8.0,
    }
}

fn text_font(style: &str, size: &str) -> FontId {
    let points = match size {
        "small" => 12.0,
        "normal" => 13.5,
        "large" => 18.0,
        "huge" => 24.0,
        _ => match style {
            "title" => 15.0,
            "subtle" => 12.5,
            _ => 13.5,
        },
    };
    if style == "title" { bold(points) } else { regular(points) }
}

fn text_galley(ui: &EUi, c: Colors, text: &str, style: &str, size: &str, lines: u32, width: f32) -> std::sync::Arc<egui::Galley> {
    let color = if style == "subtle" { c.subtext } else { c.text };
    let mut job = egui::text::LayoutJob::simple(text.to_string(), text_font(style, size), color, width.max(1.0));
    job.wrap = egui::text::TextWrapping {
        max_width: width.max(1.0),
        max_rows: if lines == 0 { usize::MAX } else { lines as usize },
        break_anywhere: lines == 1,
        overflow_character: Some('…'),
    };
    ui.painter().layout_job(job)
}

/// A button's size: round (no label) or a pill with its label.
fn button_size(ui: &EUi, label: &str, size: &str, icon: bool) -> Vec2 {
    if label.is_empty() {
        let side = match size {
            "small" => 28.0,
            "large" => 48.0,
            "huge" => 64.0,
            _ => 36.0,
        };
        return vec2(side, side);
    }
    let height = match size {
        "small" => 30.0,
        "large" => 44.0,
        _ => 36.0,
    };
    let width = ui.painter().layout_no_wrap(label.into(), regular(13.5), Color32::WHITE).size().x;
    vec2(width + if icon { 24.0 } else { 0.0 } + 32.0, height)
}

fn image_side(size: &str, width: f32) -> f32 {
    let side = match size {
        "small" => 48.0,
        "large" => 160.0,
        "fill" => width,
        number => number.parse::<f32>().ok().filter(|n| n.is_finite() && *n > 0.0).unwrap_or(96.0),
    };
    side.min(width).max(1.0)
}

/// A stack that centres (or ends) its blocks: standing in a column, it's
/// given the column's width to do that in. In a row it's as wide as its blocks.
fn spans(node: &Node) -> bool {
    matches!(node, Node::Stack { align, .. } if align == "center" || align == "end")
}

/// Blocks that take the room there is (in a row, the room the others leave).
fn fills(node: &Node) -> bool {
    match node {
        Node::Progress { .. } | Node::Input { .. } | Node::Row { .. } | Node::Toggle { .. } => true,
        Node::Stack { grow, .. } => *grow,
        Node::Image { size, .. } => size == "fill",
        _ => false,
    }
}

impl Ui {
    /// A plugin's card: its menu as Mace last sent it. A menu with no title has
    /// no header: the card's slim top strip (grip and pin) is all there is.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn plugin_menu(&mut self, ui: &mut EUi, c: Colors, d: &Snapshot, plugin: &str, button: &str,
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
        if !menu.title.is_empty() {
            ui.scope(|ui| {
                ui.set_max_width(header);
                title(ui, c, &menu.title, 16.0);
                if let Some(subtitle) = menu.subtitle.as_deref().filter(|s| !s.is_empty()) {
                    small(ui, c, subtitle);
                }
            });
            ui.add_space(8.0);
        }
        let height = (ui.max_rect().bottom() - ui.cursor().top()).max(40.0);
        egui::ScrollArea::vertical().auto_shrink([false, true]).max_height(height).show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            let at = PluginAt { plugin: &plugin, button: &button, arrived };
            for (i, node) in menu.children.iter().enumerate() {
                self.block(ui, c, d, node, &at, &i.to_string(), actions);
            }
        });
    }

    /// One block in a column: as wide as it is, starting at the left.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn block(&mut self, ui: &mut EUi, c: Colors, d: &Snapshot, node: &Node, at: &PluginAt, path: &str,
             actions: &mut Vec<Action>) {
        let width = ui.available_width();
        let size = self.measure(ui, c, node, width);
        let (rect, _) = ui.allocate_exact_size(vec2(width, size.y), Sense::hover());
        let w = if spans(node) { width } else { size.x.min(width) };
        self.place(ui, c, d, node, Rect::from_min_size(rect.min, vec2(w, size.y)), at, path, actions);
    }

    /// The room a block takes when it has `width` points.
    fn measure(&self, ui: &EUi, c: Colors, node: &Node, width: f32) -> Vec2 {
        match node {
            Node::Text { text, style, size, lines } => {
                let galley = text_galley(ui, c, text, style, size, *lines, width);
                vec2(galley.size().x.min(width), galley.size().y)
            }
            Node::Button { label, size, icon, .. } => {
                let s = button_size(ui, label, size, icon.is_some());
                vec2(s.x.min(width), s.y)
            }
            Node::Row { subtitle, .. } => vec2(width, if subtitle.as_deref().is_some_and(|s| !s.is_empty()) { 56.0 } else { 44.0 }),
            Node::Toggle { .. } => vec2(width, 40.0),
            Node::Input { .. } => vec2(width, 38.0),
            Node::Progress { times, .. } => vec2(width, if *times { 26.0 } else { 6.0 }),
            Node::Image { size, .. } => {
                let side = image_side(size, width);
                vec2(side, side)
            }
            Node::Space { size } => vec2(space_of(size), space_of(size)),
            Node::Stack { direction, gap, children, grow, .. } => {
                // as wide as its blocks (centring spreads it only where it stands alone: see `spans`)
                let wide = *grow || children.iter().any(fills);
                let gap = gap_of(gap);
                if children.is_empty() {
                    return Vec2::ZERO;
                }
                if direction == "row" {
                    let widths = self.row_widths(ui, c, children, width, gap);
                    let height = children.iter().zip(&widths).map(|(n, w)| self.measure(ui, c, n, *w).y).fold(0.0, f32::max);
                    let used = widths.iter().sum::<f32>() + gap * (children.len() - 1) as f32;
                    vec2(if wide { width } else { used.min(width) }, height)
                } else {
                    let sizes: Vec<Vec2> = children.iter().map(|n| self.measure(ui, c, n, width)).collect();
                    let w = if wide { width } else { sizes.iter().map(|s| s.x).fold(0.0, f32::max) };
                    vec2(w, sizes.iter().map(|s| s.y).sum::<f32>() + gap * (children.len() - 1) as f32)
                }
            }
            Node::Overlay { children, .. } => children.first().map_or(Vec2::ZERO, |base| self.measure(ui, c, base, width)),
            Node::Unknown => Vec2::ZERO,
        }
    }

    /// How wide each block of a row is: as wide as it is, and those that
    /// grow share what's left.
    pub(super) fn row_widths(&self, ui: &EUi, c: Colors, children: &[Node], width: f32, gap: f32) -> Vec<f32> {
        let gaps = gap * children.len().saturating_sub(1) as f32;
        let natural: Vec<Option<f32>> = children.iter()
            .map(|n| (!fills(n)).then(|| self.measure(ui, c, n, (width - gaps).max(0.0)).x))
            .collect();
        let fixed: f32 = natural.iter().flatten().sum();
        let growing = natural.iter().filter(|w| w.is_none()).count().max(1) as f32;
        let share = ((width - gaps - fixed) / growing).max(0.0);
        natural.into_iter().map(|w| w.unwrap_or(share)).collect()
    }

    /// Draw a block in `rect` (from `measure`), and say what was done with it.
    #[allow(clippy::too_many_arguments)]
    fn place(&mut self, ui: &mut EUi, c: Colors, d: &Snapshot, node: &Node, rect: Rect, at: &PluginAt, path: &str,
             actions: &mut Vec<Action>) {
        let id = ui.scope_id().with(("block", at.plugin, at.button, path));
        match node {
            Node::Text { text, style, size, lines } => {
                let galley = text_galley(ui, c, text, style, size, *lines, rect.width());
                let color = if style == "subtle" { c.subtext } else { c.text };
                ui.painter().galley(rect.min, galley, color);
            }
            Node::Button { label, style, icon, key, .. } => {
                let icon = icon.as_deref().map(|i| self.plugin_icon(ui.ctx(), i, &d.avatars));
                let enabled = key.is_some();
                let response = ui.interact(rect, id, if enabled { Sense::click() } else { Sense::hover() });
                let hovered = enabled && response.hovered();
                let (fill, ink) = match style.as_str() {
                    "primary" => (Some(if hovered { c.accent.lerp_to_gamma(c.text, 0.35) } else { c.accent }), c.on_accent),
                    "plain" => (hovered.then(|| c.hover.lerp_to_gamma(c.text, 0.1)), c.text),
                    "danger" => (Some(if hovered { c.hover.lerp_to_gamma(c.danger, 0.15) } else { c.hover }), c.danger),
                    _ => (Some(if hovered { c.hover.lerp_to_gamma(c.text, 0.08) } else { c.hover }), c.text),
                };
                let ink = if enabled { ink } else { ink.gamma_multiply(0.5) };
                let painter = ui.painter();
                if label.is_empty() {
                    if let Some(fill) = fill {
                        painter.circle_filled(rect.center(), rect.width().min(rect.height()) / 2.0, fill);
                    }
                    if let Some(icon) = icon {
                        let side = (rect.height() * 0.52).round();
                        paint_icon(painter, Rect::from_center_size(rect.center(), vec2(side, side)), icon, ink);
                    }
                } else {
                    if let Some(fill) = fill {
                        painter.rect_filled(rect, CornerRadius::same(10), fill);
                    }
                    let mut x = rect.min.x + 16.0;
                    if let Some(icon) = icon {
                        paint_icon(painter, Rect::from_center_size(pos2(x + 9.0, rect.center().y), vec2(18.0, 18.0)), icon, ink);
                        x += 24.0;
                    }
                    painter.text(pos2(x, rect.center().y), Align2::LEFT_CENTER, label, regular(13.5), ink);
                }
                if response.clicked() {
                    at.send(actions, key, "click", None);
                }
            }
            Node::Row { icon, image, title, subtitle, key, trailing } => {
                let sense = if key.is_some() { Sense::click() } else { Sense::hover() };
                let response = ui.interact(rect, id, sense);
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
                let end = match trailing {
                    Some(trailing) => {
                        let size = self.measure(ui, c, trailing, rect.width() / 2.0);
                        let at_end = Rect::from_min_size(pos2(rect.max.x - 8.0 - size.x, rect.center().y - size.y / 2.0), size);
                        self.place(ui, c, d, trailing, at_end, at, &format!("{path}.t"), actions);
                        at_end.min.x - 12.0
                    }
                    None => rect.max.x - 12.0,
                };
                two_lines_at(ui.painter(), c, x, rect.center().y, end - x, title, subtitle.as_deref().unwrap_or(""), 14.0);
                if response.clicked() {
                    at.send(actions, key, "click", None);
                }
            }
            Node::Toggle { label, value, key } => {
                let response = ui.interact(rect, id, Sense::click());
                if response.hovered() {
                    ui.painter().rect_filled(rect, CornerRadius::same(12), c.hover);
                }
                let switch = Rect::from_center_size(pos2(rect.max.x - 12.0 - 18.0, rect.center().y), vec2(36.0, 20.0));
                let galley = text_galley(ui, c, label, "", "", 1, switch.min.x - 12.0 - (rect.min.x + 12.0));
                ui.painter().galley(pos2(rect.min.x + 12.0, rect.center().y - galley.size().y / 2.0), galley, c.text);
                let track = if *value { c.accent } else { c.hover.lerp_to_gamma(c.text, 0.18) };
                ui.painter().rect_filled(switch, CornerRadius::same(10), track);
                let knob = if *value { switch.max.x - 10.0 } else { switch.min.x + 10.0 };
                ui.painter().circle_filled(pos2(knob, switch.center().y), 7.0, if *value { c.on_accent } else { c.text });
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
                let mut field = ui.new_child(egui::UiBuilder::new().max_rect(rect).id_salt(id));
                let response = {
                    // no outlines: a field is a surface, like the rest
                    let visuals = field.visuals_mut();
                    for state in [&mut visuals.widgets.inactive, &mut visuals.widgets.hovered, &mut visuals.widgets.active] {
                        state.bg_stroke = egui::Stroke::NONE;
                        state.corner_radius = CornerRadius::same(10);
                    }
                    visuals.selection.stroke = egui::Stroke::NONE;
                    visuals.selection.bg_fill = c.accent.gamma_multiply(0.35);
                    visuals.text_cursor.stroke = egui::Stroke::new(1.5, c.text);
                    field.add(egui::TextEdit::singleline(&mut entry.1)
                        .hint_text(egui::RichText::new(placeholder).font(regular(13.5)).color(c.subtext))
                        .font(regular(13.5)).text_color(c.text).background_color(c.surface)
                        .margin(vec2(12.0, 9.0)).desired_width(f32::INFINITY))
                };
                if response.lost_focus() && field.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let text = entry.1.clone();
                    at.send(actions, key, "submit", Some(serde_json::Value::String(text)));
                }
            }
            Node::Progress { value, max, rate, times, key } => {
                let max = max.max(0.0);
                let held = format!("{}/{}/{path}", at.plugin, at.button);
                // where it is now: moving on by itself, or where it was just seeked to (until Mace's next menu)
                let mut now = value + rate * at.arrived.elapsed().as_secs_f64();
                match self.seeks.get(&held) {
                    Some((to, when)) if *when > at.arrived && when.elapsed().as_secs_f32() < 3.0 => {
                        now = to + rate * when.elapsed().as_secs_f64();
                    }
                    Some(_) => {
                        self.seeks.remove(&held);
                    }
                    None => {}
                }
                let track = Rect::from_min_size(pos2(rect.min.x, rect.min.y + 1.0), vec2(rect.width(), 4.0));
                let mut hovered = false;
                if key.is_some() {
                    let response = ui.interact(track.expand2(vec2(0.0, 6.0)), id, Sense::click_and_drag());
                    hovered = response.hovered() || response.dragged();
                    let pointer = response.interact_pointer_pos().or(response.hover_pos());
                    let at_pointer = |p: egui::Pos2| ((p.x - track.min.x) / track.width()).clamp(0.0, 1.0) as f64 * max;
                    if response.dragged() && let Some(p) = pointer {
                        now = at_pointer(p);
                    }
                    if (response.clicked() || response.drag_stopped()) && let Some(p) = pointer {
                        let to = at_pointer(p);
                        now = to;
                        self.seeks.insert(held, (to, Instant::now()));
                        at.send(actions, key, "change", Some(serde_json::json!(to)));
                    }
                }
                let now = now.clamp(0.0, max);
                let filled = if max > 0.0 { (now / max) as f32 } else { 0.0 };
                let painter = ui.painter();
                painter.rect_filled(track, CornerRadius::same(2), c.hover.lerp_to_gamma(c.text, 0.18));
                painter.rect_filled(Rect::from_min_size(track.min, vec2(track.width() * filled, 4.0)), CornerRadius::same(2),
                                    if hovered { c.accent } else { c.text.gamma_multiply(0.85) });
                if hovered {
                    painter.circle_filled(pos2(track.min.x + track.width() * filled, track.center().y), 6.0, c.text);
                }
                if *times {
                    let clock = |s: f64| format!("{}:{:02}", (s as u64) / 60, (s as u64) % 60);
                    painter.text(pos2(rect.min.x, rect.max.y), Align2::LEFT_BOTTOM, clock(now), regular(11.5), c.subtext);
                    painter.text(rect.max, Align2::RIGHT_BOTTOM, clock(max), regular(11.5), c.subtext);
                }
                if *rate != 0.0 || hovered {
                    ui.ctx().request_repaint();
                }
            }
            Node::Image { image, .. } => {
                let radius = CornerRadius::same((rect.width() * 0.06).clamp(4.0, 12.0) as u8);
                match self.texture(ui.ctx(), image, &d.avatars) {
                    Some(texture) => {
                        egui::Image::new((texture, rect.size())).corner_radius(radius).paint_at(ui, rect);
                    }
                    // still on its way
                    None => {
                        ui.painter().rect_filled(rect, radius, c.surface);
                    }
                }
            }
            Node::Space { .. } | Node::Unknown => {}
            Node::Stack { direction, gap, align, children, .. } => {
                let gap = gap_of(gap);
                if direction == "row" {
                    let widths = self.row_widths(ui, c, children, rect.width(), gap);
                    let used = widths.iter().sum::<f32>() + gap * children.len().saturating_sub(1) as f32;
                    let mut x = rect.min.x + match align.as_str() {
                        "center" => ((rect.width() - used) / 2.0).max(0.0),
                        "end" => (rect.width() - used).max(0.0),
                        _ => 0.0,
                    };
                    for (i, (child, w)) in children.iter().zip(widths).enumerate() {
                        let h = self.measure(ui, c, child, w).y;
                        let cell = Rect::from_min_size(pos2(x, rect.center().y - h / 2.0), vec2(w, h));
                        self.place(ui, c, d, child, cell, at, &format!("{path}.{i}"), actions);
                        x += w + gap;
                    }
                } else {
                    let mut y = rect.min.y;
                    for (i, child) in children.iter().enumerate() {
                        let mut size = self.measure(ui, c, child, rect.width());
                        if spans(child) {
                            size.x = rect.width();
                        }
                        let x = rect.min.x + match align.as_str() {
                            "center" => ((rect.width() - size.x) / 2.0).max(0.0),
                            "end" => (rect.width() - size.x).max(0.0),
                            _ => 0.0,
                        };
                        let cell = Rect::from_min_size(pos2(x, y), vec2(size.x.min(rect.width()), size.y));
                        self.place(ui, c, d, child, cell, at, &format!("{path}.{i}"), actions);
                        y += size.y + gap;
                    }
                }
            }
            Node::Overlay { children, align, reveal } => {
                let Some((base, layers)) = children.split_first() else { return };
                self.place(ui, c, d, base, rect, at, &format!("{path}.0"), actions);
                if reveal == "hover" {
                    if !ui.rect_contains_pointer(rect) {
                        return;
                    }
                    let radius = CornerRadius::same((rect.width() * 0.06).clamp(4.0, 12.0) as u8);
                    ui.painter().rect_filled(rect, radius, Color32::from_black_alpha(120));
                }
                let sizes: Vec<Vec2> = layers.iter().map(|n| self.measure(ui, c, n, rect.width())).collect();
                let total = sizes.iter().map(|s| s.y).sum::<f32>() + 8.0 * sizes.len().saturating_sub(1) as f32;
                let mut y = rect.min.y + match align.as_str() {
                    "start" => 12.0,
                    "end" => (rect.height() - total - 12.0).max(0.0),
                    _ => ((rect.height() - total) / 2.0).max(0.0),
                };
                for (i, (layer, size)) in layers.iter().zip(sizes).enumerate() {
                    let cell = Rect::from_min_size(pos2(rect.min.x, y), vec2(rect.width(), size.y));
                    self.place(ui, c, d, layer, cell, at, &format!("{path}.{}", i + 1), actions);
                    y += size.y + 8.0;
                }
            }
        }
    }
}


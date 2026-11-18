//! Small building blocks shared by the windows.

use adw::prelude::*;
use serde_json::{Map, Value};

/// Run `work` on a worker thread and hand its result to `done` on the main loop.
pub fn background<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static, done: impl FnOnce(T) + 'static) {
    let handle = gtk::gio::spawn_blocking(work);
    gtk::glib::spawn_future_local(async move {
        if let Ok(result) = handle.await {
            done(result);
        }
    });
}

/// A ComboRow over (value, label) pairs, with the current value selected.
pub fn combo(title: &str, choices: &[(String, String)], current: &str, subtitle: &str) -> adw::ComboRow {
    let labels: Vec<&str> = choices.iter().map(|(_, label)| label.as_str()).collect();
    let row = adw::ComboRow::builder().title(title).model(&gtk::StringList::new(&labels)).build();
    if !subtitle.is_empty() {
        row.set_subtitle(subtitle);
    }
    let at = choices.iter().position(|(value, _)| value == current).unwrap_or(0);
    row.set_selected(at as u32);
    row
}

pub fn chosen(row: &adw::ComboRow, choices: &[(String, String)]) -> String {
    choices.get(row.selected() as usize).map(|(value, _)| value.clone()).unwrap_or_default()
}

pub fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items.iter().map(|(v, l)| (v.to_string(), l.to_string())).collect()
}

pub fn spin(title: &str, value: u64, upper: f64, subtitle: &str) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(0.0, upper, 1.0);
    row.set_title(title);
    if !subtitle.is_empty() {
        row.set_subtitle(subtitle);
    }
    row.set_value(value as f64);
    row
}

pub const PRESENTATION_CHOICES: [(&str, &str); 5] = [("", "Default"), ("immediate", "Immediate"),
    ("mailbox", "Mailbox"), ("fifo", "VSync (FIFO)"), ("fifo_relaxed", "FIFO relaxed")];

/// A multi-line NAME=value editor.
pub fn pairs_editor(values: &Map<String, Value>) -> (gtk::Frame, gtk::TextBuffer) {
    let buffer = gtk::TextBuffer::new(None);
    let text: Vec<String> = values
        .iter()
        .map(|(k, v)| match v {
            Value::String(s) => format!("{k}={s}"),
            other => format!("{k}={other}"),
        })
        .collect();
    buffer.set_text(&text.join("\n"));
    let view = gtk::TextView::builder()
        .buffer(&buffer)
        .monospace(true)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(8)
        .right_margin(8)
        .wrap_mode(gtk::WrapMode::Char)
        .build();
    let scrolled = gtk::ScrolledWindow::builder().child(&view).min_content_height(120).propagate_natural_height(true).build();
    (gtk::Frame::builder().child(&scrolled).build(), buffer)
}

/// NAME=value lines; the pairs, and the lines that aren't.
pub fn read_pairs(buffer: &gtk::TextBuffer) -> (Vec<(String, String)>, Vec<String>) {
    let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
    let mut pairs = Vec::new();
    let mut bad = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        match line.split_once('=') {
            Some((name, value)) if !name.trim().is_empty() => pairs.push((name.trim().into(), value.trim().into())),
            _ => bad.push(trimmed.to_string()),
        }
    }
    (pairs, bad)
}

pub fn markup(text: &str) -> String {
    gtk::glib::markup_escape_text(text).to_string()
}

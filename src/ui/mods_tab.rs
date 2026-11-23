//! The Mods tab: Roblox's cursors, death sound and font, and any other files
//! in the mods folder. Mods are for every account and go in at each start.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk_pixbuf, gio};

use super::widgets::background;
use crate::mods;

/// Roblox's cursor images are this square, with the pointer at their centre.
const CURSOR_SIZE: i32 = 64;
/// Shift lock's cursor is smaller, and centred the same way.
const SHIFT_LOCK_SIZE: i32 = 32;
const CURSOR_CHOICES: [&str; 4] = ["Default", "2006", "2013", "Custom"];
const PICTURES: [&str; 5] = ["png", "jpg", "jpeg", "svg", "webp"];

pub struct ModsTab {
    pub page: adw::PreferencesPage,
    cursor: adw::ComboRow,
    preview: gtk::Image,
    shift_lock: adw::ActionRow,
    shift_lock_preview: gtk::Image,
    shift_lock_reset: gtk::Button,
    sound: adw::ActionRow,
    sound_reset: gtk::Button,
    font: adw::ActionRow,
    font_reset: gtk::Button,
    files: adw::PreferencesGroup,
    rows: RefCell<Vec<adw::ActionRow>>,
    /// The cursor choice is being set from the mods folder, not by a click.
    quiet: Cell<bool>,
    toast: Box<dyn Fn(&str)>,
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    gtk::Button::builder().icon_name(icon).tooltip_text(tooltip).valign(gtk::Align::Center).css_classes(["flat"]).build()
}

fn choose_button() -> gtk::Button {
    gtk::Button::builder().label("Choose…").valign(gtk::Align::Center).build()
}

/// A picture as one of Roblox's cursors, `size` square: kept as it is when
/// it's already a PNG that size, otherwise scaled to fit and centred.
fn cursor_png(path: &Path, size: i32) -> anyhow::Result<Vec<u8>> {
    let bytes = std::fs::read(path)?;
    let (format, w, h) = gdk_pixbuf::Pixbuf::file_info(path).ok_or_else(|| anyhow::anyhow!("not a picture"))?;
    if format.name().is_some_and(|n| n == "png") && w == size && h == size {
        return Ok(bytes);
    }
    let scaled = gdk_pixbuf::Pixbuf::from_file_at_scale(path, size, size, true)?;
    let canvas = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, true, 8, size, size)
        .ok_or_else(|| anyhow::anyhow!("out of memory"))?;
    canvas.fill(0);
    let (sw, sh) = (scaled.width(), scaled.height());
    scaled.copy_area(0, 0, sw, sh, &canvas, (size - sw) / 2, (size - sh) / 2);
    Ok(canvas.save_to_bufferv("png", &[])?)
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

impl ModsTab {
    pub fn new(toast: impl Fn(&str) + 'static) -> Rc<ModsTab> {
        let page = adw::PreferencesPage::builder().title("Mods").icon_name("applications-engineering-symbolic").build();

        let group = adw::PreferencesGroup::builder().title("Look and sound")
            .description("Applies to games started afterwards.").build();
        page.add(&group);
        let cursor = adw::ComboRow::builder().title("Cursor").model(&gtk::StringList::new(&CURSOR_CHOICES)).build();
        let preview = gtk::Image::builder().pixel_size(32).visible(false).build();
        cursor.add_suffix(&preview);
        let shift_lock = adw::ActionRow::builder().title("Shift lock cursor").build();
        let shift_lock_preview = gtk::Image::builder().pixel_size(SHIFT_LOCK_SIZE).visible(false).build();
        let (shift_lock_choose, shift_lock_reset) = (choose_button(), icon_button("edit-undo-symbolic", "Back to Roblox's own"));
        shift_lock.add_suffix(&shift_lock_preview);
        shift_lock.add_suffix(&shift_lock_reset);
        shift_lock.add_suffix(&shift_lock_choose);
        let sound = adw::ActionRow::builder().title("Death sound").build();
        let (sound_choose, sound_reset) = (choose_button(), icon_button("edit-undo-symbolic", "Back to Roblox's own"));
        sound.add_suffix(&sound_reset);
        sound.add_suffix(&sound_choose);
        let font = adw::ActionRow::builder().title("Font").build();
        let (font_choose, font_reset) = (choose_button(), icon_button("edit-undo-symbolic", "Back to Roblox's own"));
        font.add_suffix(&font_reset);
        font.add_suffix(&font_choose);
        group.add(&cursor);
        group.add(&shift_lock);
        group.add(&sound);
        group.add(&font);

        let open = gtk::Button::builder()
            .child(&adw::ButtonContent::builder().icon_name("folder-open-symbolic").label("Open folder").build())
            .css_classes(["flat"]).build();
        let files = adw::PreferencesGroup::builder().title("Files")
            .description("Files in the mods folder replace Roblox's own at the same path.")
            .header_suffix(&open).build();
        page.add(&files);

        let me = Rc::new(ModsTab {
            page, cursor, preview, shift_lock, shift_lock_preview, shift_lock_reset, sound, sound_reset, font, font_reset,
            files, rows: RefCell::new(Vec::new()), quiet: Cell::new(false), toast: Box::new(toast),
        });

        let weak = Rc::downgrade(&me);
        me.cursor.connect_selected_notify(move |row| {
            if let Some(me) = weak.upgrade()
                && !me.quiet.get()
            {
                me.cursor_chosen(row.selected());
            }
        });
        let weak = Rc::downgrade(&me);
        shift_lock_choose.connect_clicked(move |_| {
            if let Some(me) = weak.upgrade() {
                me.choose("Choose a shift lock cursor", "Pictures", &PICTURES, |me, path| {
                    match cursor_png(&path, SHIFT_LOCK_SIZE).and_then(|png| mods::put(mods::SHIFT_LOCK, &png)) {
                        Ok(()) => mods::set_note("shift-lock", Some(&file_name(&path))),
                        Err(error) => (me.toast)(&format!("Couldn't use that picture: {error}")),
                    }
                    me.refresh();
                });
            }
        });
        let weak = Rc::downgrade(&me);
        me.shift_lock_reset.connect_clicked(move |_| {
            if let Some(me) = weak.upgrade() {
                me.reset(&[mods::SHIFT_LOCK], "shift-lock");
            }
        });
        let weak = Rc::downgrade(&me);
        sound_choose.connect_clicked(move |_| {
            if let Some(me) = weak.upgrade() {
                me.choose("Choose a death sound", "Sounds", &["ogg", "mp3", "wav"], |me, path| {
                    me.install(mods::DEATH_SOUND, "death-sound", &path);
                });
            }
        });
        let weak = Rc::downgrade(&me);
        me.sound_reset.connect_clicked(move |_| {
            if let Some(me) = weak.upgrade() {
                me.reset(&[mods::DEATH_SOUND], "death-sound");
            }
        });
        let weak = Rc::downgrade(&me);
        font_choose.connect_clicked(move |_| {
            if let Some(me) = weak.upgrade() {
                me.choose("Choose a font", "Fonts", &["ttf", "otf"], |me, path| me.install(mods::FONT, "font", &path));
            }
        });
        let weak = Rc::downgrade(&me);
        me.font_reset.connect_clicked(move |_| {
            if let Some(me) = weak.upgrade() {
                me.reset(&[mods::FONT], "font");
            }
        });
        let weak = Rc::downgrade(&me);
        open.connect_clicked(move |_| {
            if let Some(me) = weak.upgrade() {
                let _ = std::fs::create_dir_all(mods::folder());
                let window = me.page.root().and_downcast::<gtk::Window>();
                gtk::FileLauncher::new(Some(&gio::File::for_path(mods::folder())))
                    .launch(window.as_ref(), gio::Cancellable::NONE, |_| {});
            }
        });
        // files may have been added in the file manager meanwhile
        let weak = Rc::downgrade(&me);
        me.page.connect_map(move |_| {
            if let Some(me) = weak.upgrade() {
                me.refresh();
            }
        });
        me.refresh();
        me
    }

    /// Show what the mods folder holds now.
    fn refresh(self: &Rc<Self>) {
        let cursor = if !mods::has(mods::CURSORS[0]) {
            0
        } else {
            match mods::note("cursor").as_deref() {
                Some("2006") => 1,
                Some("2013") => 2,
                _ => 3,
            }
        };
        self.quiet.set(true);
        self.cursor.set_selected(cursor);
        self.quiet.set(false);
        let image = mods::folder().join(mods::CURSORS[0]);
        self.preview.set_visible(cursor != 0);
        if cursor != 0 {
            self.preview.set_from_file(Some(&image));
        }
        for (row, reset, rel, note) in [(&self.shift_lock, &self.shift_lock_reset, mods::SHIFT_LOCK, "shift-lock"),
                                        (&self.sound, &self.sound_reset, mods::DEATH_SOUND, "death-sound"),
                                        (&self.font, &self.font_reset, mods::FONT, "font")] {
            let set = mods::has(rel);
            row.set_subtitle(&if set { mods::note(note).unwrap_or_else(|| "Custom".into()) } else { "Roblox's own".into() });
            reset.set_visible(set);
        }
        let shift_lock = mods::has(mods::SHIFT_LOCK);
        self.shift_lock_preview.set_visible(shift_lock);
        if shift_lock {
            self.shift_lock_preview.set_from_file(Some(&mods::folder().join(mods::SHIFT_LOCK)));
        }

        for row in self.rows.borrow_mut().drain(..) {
            self.files.remove(&row);
        }
        let others = mods::other_files();
        if others.is_empty() {
            let row = adw::ActionRow::builder().title("None yet").css_classes(["dim-label"]).build();
            self.files.add(&row);
            self.rows.borrow_mut().push(row);
        }
        for rel in others {
            let path = Path::new(&rel);
            let row = adw::ActionRow::builder().title(file_name(path))
                .subtitle(path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default())
                .subtitle_lines(1).build();
            let remove = icon_button("user-trash-symbolic", "Remove");
            let weak = Rc::downgrade(self);
            remove.connect_clicked(move |_| {
                if let Some(me) = weak.upgrade() {
                    if let Err(error) = mods::remove(&rel) {
                        (me.toast)(&format!("Couldn't remove it: {error}"));
                    }
                    me.refresh();
                }
            });
            row.add_suffix(&remove);
            self.files.add(&row);
            self.rows.borrow_mut().push(row);
        }
    }

    fn cursor_chosen(self: &Rc<Self>, choice: u32) {
        match choice {
            0 => self.reset(&mods::CURSORS, "cursor"),
            1 | 2 => {
                let year = CURSOR_CHOICES[choice as usize];
                let weak = Rc::downgrade(self);
                background(move || mods::fetch_cursor_preset(year), move |result| {
                    let Some(me) = weak.upgrade() else { return };
                    let saved = result.and_then(|[arrow, far]| {
                        mods::put(mods::CURSORS[0], &arrow)?;
                        mods::put(mods::CURSORS[1], &far)
                    });
                    match saved {
                        Ok(()) => mods::set_note("cursor", Some(year)),
                        Err(_) => (me.toast)(&format!("Couldn't download the {year} cursor")),
                    }
                    me.refresh();
                });
            }
            _ => self.choose("Choose a cursor", "Pictures", &PICTURES, |me, path| {
                let saved = cursor_png(&path, CURSOR_SIZE).and_then(|png| {
                    mods::put(mods::CURSORS[0], &png)?;
                    mods::put(mods::CURSORS[1], &png)
                });
                match saved {
                    Ok(()) => mods::set_note("cursor", Some("custom")),
                    Err(error) => (me.toast)(&format!("Couldn't use that picture: {error}")),
                }
                me.refresh();
            }),
        }
    }

    /// Copy a chosen file in as the mod at `rel`, remembering its name.
    fn install(self: &Rc<Self>, rel: &str, note: &str, path: &Path) {
        match std::fs::read(path).map_err(anyhow::Error::from).and_then(|bytes| mods::put(rel, &bytes)) {
            Ok(()) => mods::set_note(note, Some(&file_name(path))),
            Err(error) => (self.toast)(&format!("Couldn't copy it: {error}")),
        }
        self.refresh();
    }

    fn reset(self: &Rc<Self>, rels: &[&str], note: &str) {
        for rel in rels {
            if let Err(error) = mods::remove(rel) {
                (self.toast)(&format!("Couldn't remove it: {error}"));
            }
        }
        mods::set_note(note, None);
        self.refresh();
    }

    /// Ask for a file; `then` gets it, and a cancel just shows things as they were.
    fn choose(self: &Rc<Self>, title: &str, kind: &str, suffixes: &[&str], then: impl Fn(&Rc<Self>, PathBuf) + 'static) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(kind));
        for suffix in suffixes {
            filter.add_suffix(suffix);
        }
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder().title(title).modal(true).filters(&filters).default_filter(&filter).build();
        let window = self.page.root().and_downcast::<gtk::Window>();
        let weak = Rc::downgrade(self);
        dialog.open(window.as_ref(), gio::Cancellable::NONE, move |result| {
            let Some(me) = weak.upgrade() else { return };
            match result.ok().and_then(|file| file.path()) {
                Some(path) => then(&me, path),
                None => me.refresh(),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_pictures_become_roblox_sized() {
        let dir = tempfile::tempdir().unwrap();
        // a wide picture: scaled to fit and centred on a 64×64 canvas
        let wide = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, true, 8, 128, 32).unwrap();
        wide.fill(0xffffffff);
        let path = dir.path().join("wide.png");
        wide.savev(&path, "png", &[]).unwrap();
        for size in [CURSOR_SIZE, SHIFT_LOCK_SIZE] {
            let png = cursor_png(&path, size).unwrap();
            let out = dir.path().join(format!("out{size}.png"));
            std::fs::write(&out, &png).unwrap();
            let made = gdk_pixbuf::Pixbuf::from_file(&out).unwrap();
            assert_eq!((made.width(), made.height()), (size, size));
            let pixels = made.read_pixel_bytes();
            let alpha = |x: i32, y: i32| pixels[(y * made.rowstride() + x * 4 + 3) as usize];
            assert_eq!(alpha(size / 2, size / 2), 255); // the picture, in the middle
            assert_eq!(alpha(size / 2, 2), 0); // transparent above and below it
            // already that size: kept byte for byte; for the other size it's redrawn
            assert_eq!(cursor_png(&out, size).unwrap(), png);
            let other = if size == CURSOR_SIZE { SHIFT_LOCK_SIZE } else { CURSOR_SIZE };
            assert_ne!(cursor_png(&out, other).unwrap(), png);
        }
        assert!(cursor_png(&dir.path().join("missing.png"), CURSOR_SIZE).is_err());
    }
}

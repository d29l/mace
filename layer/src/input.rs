//! What the panel does with the game's input, decided event by event in SDL's
//! event filter (see `sdl`). Kept free of SDL calls so it can be tested alone.
//!
//! - The panel key opens and closes the panel, and the game never sees it.
//! - While the panel is open ("captured"), keys, typing and the mouse go to the
//!   panel instead of the game. Window, quit, device and the game's own events
//!   always pass.
//! - Keys and buttons the game saw go down before the panel opened are let go
//!   (synthetic releases, sent from the main thread), so nothing stays held;
//!   their real releases are then swallowed, like those of everything pressed
//!   while the panel was open.

use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use mace_proto::{Key, MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER};

pub const QUIT: u32 = 0x100;
pub const WINDOW_RESIZED: u32 = 0x206;
pub const WINDOW_PIXEL_SIZE_CHANGED: u32 = 0x207;
pub const WINDOW_FOCUS_GAINED: u32 = 0x20e;
pub const WINDOW_FOCUS_LOST: u32 = 0x20f;
pub const KEY_DOWN: u32 = 0x300;
pub const KEY_UP: u32 = 0x301;
pub const TEXT_EDITING: u32 = 0x302;
pub const TEXT_INPUT: u32 = 0x303;
pub const TEXT_EDITING_CANDIDATES: u32 = 0x307;
pub const MOUSE_MOTION: u32 = 0x400;
pub const MOUSE_BUTTON_DOWN: u32 = 0x401;
pub const MOUSE_BUTTON_UP: u32 = 0x402;
pub const MOUSE_WHEEL: u32 = 0x403;
pub const USER: u32 = 0x8000;
/// `SDL_EVENT_USER` with this code (data1: a C string) asks for a picture of
/// the next frame; only tests send it, and only `MACE_PANEL_DUMP` listens.
pub const DUMP_CODE: i32 = 0x4d41_4345;

pub const SCANCODE_ESCAPE: u32 = 41;
/// Pressed and let go by anti-AFK: a key no game uses.
pub const SCANCODE_F13: u32 = 104;

/// An `SDL_Event`: 128 bytes, read at the offsets SDL3's headers give.
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub struct Event(pub [u8; 128]);

const _: () = assert!(std::mem::size_of::<Event>() == 128);

impl Event {
    pub fn new(kind: u32) -> Event {
        let mut e = Event([0; 128]);
        e.put(0, kind);
        e
    }
    pub fn put<T: Copy>(&mut self, at: usize, value: T) {
        assert!(at + std::mem::size_of::<T>() <= 128);
        // SAFETY: in bounds (checked above).
        unsafe { std::ptr::write_unaligned(self.0.as_mut_ptr().add(at).cast(), value) }
    }
    pub fn get<T: Copy>(&self, at: usize) -> T {
        assert!(at + std::mem::size_of::<T>() <= 128);
        // SAFETY: in bounds (checked above).
        unsafe { std::ptr::read_unaligned(self.0.as_ptr().add(at).cast()) }
    }
    /// A text input event's text (`SDL_TextInputEvent.text`, a C string SDL
    /// owns while the event is handled).
    pub fn text(&self) -> Option<String> {
        let pointer: usize = self.get(24);
        if pointer == 0 {
            return None;
        }
        // SAFETY: SDL's pointer to a NUL-terminated string, valid while the filter runs.
        let text = unsafe { std::ffi::CStr::from_ptr(pointer as *const std::ffi::c_char) };
        Some(text.to_string_lossy().into_owned()).filter(|t| !t.is_empty())
    }
    pub fn kind(&self) -> u32 {
        self.get(0)
    }
    pub fn window(&self) -> u32 {
        self.get(16)
    }
    // keyboard
    pub fn scancode(&self) -> u32 {
        self.get(24)
    }
    pub fn keymod(&self) -> u16 {
        self.get(32)
    }
    pub fn repeat(&self) -> bool {
        self.get::<u8>(37) != 0
    }
    // mouse
    pub fn motion(&self) -> (f32, f32) {
        (self.get(28), self.get(32))
    }
    pub fn button(&self) -> u8 {
        self.get(24)
    }
    pub fn button_at(&self) -> (f32, f32) {
        (self.get(28), self.get(32))
    }
    /// Wheel: (x, y) scrolled, with "flipped" devices put right.
    pub fn wheel(&self) -> (f32, f32) {
        let flip = if self.get::<u32>(32) == 1 { -1.0 } else { 1.0 };
        (self.get::<f32>(24) * flip, self.get::<f32>(28) * flip)
    }
    pub fn wheel_at(&self) -> (f32, f32) {
        (self.get(36), self.get(40))
    }
    // window
    pub fn data(&self) -> (i32, i32) {
        (self.get(20), self.get(24))
    }
    // user
    pub fn code(&self) -> i32 {
        self.get(20)
    }
    pub fn data1(&self) -> usize {
        self.get(24)
    }
}

/// SDL_Keymod bits → ours.
pub fn mods(keymod: u16) -> u8 {
    let mut m = 0;
    if keymod & 0x0003 != 0 { m |= MOD_SHIFT }
    if keymod & 0x00c0 != 0 { m |= MOD_CTRL }
    if keymod & 0x0300 != 0 { m |= MOD_ALT }
    if keymod & 0x0c00 != 0 { m |= MOD_SUPER }
    m
}

/// What the panel's UI gets (positions in window points).
#[derive(Debug, Clone, PartialEq)]
pub enum UiEvent {
    Move(f32, f32),
    Button { button: u8, down: bool, x: f32, y: f32 },
    Wheel { dx: f32, dy: f32 },
    Key { scancode: u32, down: bool, mods: u8 },
    /// Typed text (SDL's text input).
    Text(String),
}

/// Something held when the panel opened, to be released for the game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    Key(u32),
    Button(u8),
}

#[derive(Default)]
struct Pressed {
    /// Down as far as the game knows.
    game: Vec<Held>,
    /// Downs the game never saw: their releases are swallowed too.
    swallowed: Vec<Held>,
}

impl Pressed {
    fn add(list: &mut Vec<Held>, h: Held) {
        if !list.contains(&h) {
            list.push(h);
        }
    }
    fn remove(list: &mut Vec<Held>, h: Held) -> bool {
        let before = list.len();
        list.retain(|x| *x != h);
        list.len() != before
    }
}

pub struct Input {
    /// The panel is open: input goes to it, not the game.
    pub captured: AtomicBool,
    /// The panel's key was pressed this many times since the panel last looked.
    toggles: AtomicU32,
    /// scancode | mods << 32; 0 = no key
    hotkey: AtomicU64,
    pressed: Mutex<Pressed>,
    events: Mutex<Vec<UiEvent>>,
    /// The game window's size in points (f32 bits, w << 32 | h).
    points: AtomicU64,
    /// The game window's SDL id.
    pub window: AtomicU32,
    /// Off for good (the panel was disabled): everything passes.
    pub off: AtomicBool,
    /// Where pinned things are (window points, x0 y0 x1 y1): with the mouse
    /// free, clicks there are theirs, the panel open or not.
    hits: Mutex<Vec<[f32; 4]>>,
    /// The mouse is free (not held by the game for its camera), as last seen.
    mouse_free: AtomicBool,
    /// A press on a pinned thing is going on: what follows is the panel's too.
    grabbed: AtomicBool,
    /// The pointer was last said to be away (locked by the game).
    away: AtomicBool,
}

thread_local! {
    /// Set while the panel itself pushes events (releases for the game), which pass untouched.
    static SYNTHETIC: Cell<bool> = const { Cell::new(false) };
}

/// The event being filtered is one the panel pushed itself.
pub fn is_synthetic() -> bool {
    SYNTHETIC.with(Cell::get)
}

/// Run `f` with the panel's own events passing the filter.
pub fn synthetic<T>(f: impl FnOnce() -> T) -> T {
    SYNTHETIC.with(|s| s.set(true));
    let value = f();
    SYNTHETIC.with(|s| s.set(false));
    value
}

impl Default for Input {
    fn default() -> Self {
        Self::new()
    }
}

impl Input {
    pub const fn new() -> Input {
        Input {
            captured: AtomicBool::new(false),
            toggles: AtomicU32::new(0),
            hotkey: AtomicU64::new(0),
            pressed: Mutex::new(Pressed { game: Vec::new(), swallowed: Vec::new() }),
            events: Mutex::new(Vec::new()),
            points: AtomicU64::new(0),
            window: AtomicU32::new(0),
            off: AtomicBool::new(false),
            hits: Mutex::new(Vec::new()),
            mouse_free: AtomicBool::new(true),
            grabbed: AtomicBool::new(false),
            away: AtomicBool::new(false),
        }
    }

    pub fn set_hotkey(&self, key: Key) {
        let value = if key.scancode == 0 { 0 } else { key.scancode as u64 | (key.mods as u64) << 32 };
        self.hotkey.store(value, Ordering::Relaxed);
    }

    fn is_hotkey(&self, scancode: u32, keymod: u16) -> bool {
        let hotkey = self.hotkey.load(Ordering::Relaxed);
        hotkey != 0 && hotkey as u32 == scancode && (hotkey >> 32) as u8 == mods(keymod)
    }

    pub fn toggle(&self) {
        self.toggles.fetch_add(1, Ordering::Relaxed);
    }

    pub fn take_toggles(&self) -> u32 {
        self.toggles.swap(0, Ordering::Relaxed)
    }

    pub fn toggle_pending(&self) -> bool {
        self.toggles.load(Ordering::Relaxed) != 0
    }

    pub fn set_points(&self, w: f32, h: f32) {
        self.points.store((w.to_bits() as u64) << 32 | h.to_bits() as u64, Ordering::Relaxed);
    }

    /// The game window's size in points, once known.
    pub fn points(&self) -> Option<(f32, f32)> {
        let v = self.points.load(Ordering::Relaxed);
        let (w, h) = (f32::from_bits((v >> 32) as u32), f32::from_bits(v as u32));
        (w > 0.0 && h > 0.0).then_some((w, h))
    }

    pub fn set_hits(&self, hits: Vec<[f32; 4]>) {
        *self.hits.lock().unwrap() = hits;
    }

    pub fn set_mouse_free(&self, free: bool) {
        self.mouse_free.store(free, Ordering::Relaxed);
    }

    /// Whether (x, y) is on something pinned, with the mouse free to click it.
    fn on_pinned(&self, x: f32, y: f32) -> bool {
        self.mouse_free.load(Ordering::Relaxed)
            && self.hits.lock().unwrap().iter().any(|r| x >= r[0] && y >= r[1] && x < r[2] && y < r[3])
    }

    pub fn drain(&self) -> Vec<UiEvent> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }

    fn queue(&self, e: UiEvent) {
        let mut events = self.events.lock().unwrap();
        if events.len() < 4096 {
            events.push(e);
        }
    }

    /// Everything the game holds down, now to be released for it; from here on
    /// their real releases are swallowed.
    pub fn release_held(&self) -> Vec<Held> {
        let mut p = self.pressed.lock().unwrap();
        let held = std::mem::take(&mut p.game);
        for h in &held {
            Pressed::add(&mut p.swallowed, *h);
        }
        held
    }

    /// The filter's decision for one event: true lets the game have it.
    pub fn filter(&self, e: &Event) -> bool {
        if SYNTHETIC.with(Cell::get) || self.off.load(Ordering::Relaxed) {
            return true;
        }
        let captured = self.captured.load(Ordering::Relaxed);
        match e.kind() {
            KEY_DOWN => {
                let key = Held::Key(e.scancode());
                if self.is_hotkey(e.scancode(), e.keymod()) {
                    if !e.repeat() {
                        // swallowing starts now; the panel opens (or closes) on its next frame
                        self.captured.store(true, Ordering::Relaxed);
                        self.toggle();
                    }
                    Pressed::add(&mut self.pressed.lock().unwrap().swallowed, key);
                    return false;
                }
                if captured {
                    if !e.repeat() {
                        Pressed::add(&mut self.pressed.lock().unwrap().swallowed, key);
                    }
                    self.queue(UiEvent::Key { scancode: e.scancode(), down: true, mods: mods(e.keymod()) });
                    return false;
                }
                Pressed::add(&mut self.pressed.lock().unwrap().game, key);
                true
            }
            KEY_UP => {
                let key = Held::Key(e.scancode());
                let mut p = self.pressed.lock().unwrap();
                if Pressed::remove(&mut p.swallowed, key) {
                    drop(p);
                    if captured {
                        self.queue(UiEvent::Key { scancode: e.scancode(), down: false, mods: mods(e.keymod()) });
                    }
                    return false;
                }
                // the game saw this go down: it gets the release, open or not
                Pressed::remove(&mut p.game, key)
                    || !captured
            }
            TEXT_INPUT => {
                if captured && let Some(text) = e.text() {
                    self.queue(UiEvent::Text(text));
                }
                !captured
            }
            TEXT_EDITING | TEXT_EDITING_CANDIDATES => !captured,
            MOUSE_MOTION => {
                if captured {
                    let (x, y) = e.motion();
                    self.queue(UiEvent::Move(x, y));
                    return false;
                }
                if self.hits.lock().unwrap().is_empty() {
                    return true;
                }
                // pinned things show the pointer over them (the game still gets it), unless they've been grabbed
                if self.mouse_free.load(Ordering::Relaxed) {
                    let (x, y) = e.motion();
                    self.away.store(false, Ordering::Relaxed);
                    self.queue(UiEvent::Move(x, y));
                } else if !self.away.swap(true, Ordering::Relaxed) {
                    self.queue(UiEvent::Move(-10_000.0, -10_000.0));
                }
                !self.grabbed.load(Ordering::Relaxed)
            }
            MOUSE_BUTTON_DOWN => {
                let button = Held::Button(e.button());
                let (x, y) = e.button_at();
                if !captured && self.on_pinned(x, y) {
                    self.grabbed.store(true, Ordering::Relaxed);
                    Pressed::add(&mut self.pressed.lock().unwrap().swallowed, button);
                    self.queue(UiEvent::Move(x, y));
                    self.queue(UiEvent::Button { button: e.button(), down: true, x, y });
                    return false;
                }
                if captured {
                    Pressed::add(&mut self.pressed.lock().unwrap().swallowed, button);
                    let (x, y) = e.button_at();
                    self.queue(UiEvent::Button { button: e.button(), down: true, x, y });
                    return false;
                }
                Pressed::add(&mut self.pressed.lock().unwrap().game, button);
                true
            }
            MOUSE_BUTTON_UP => {
                let button = Held::Button(e.button());
                let mut p = self.pressed.lock().unwrap();
                if Pressed::remove(&mut p.swallowed, button) {
                    drop(p);
                    let grabbed = self.grabbed.swap(false, Ordering::Relaxed);
                    if captured || grabbed {
                        let (x, y) = e.button_at();
                        self.queue(UiEvent::Button { button: e.button(), down: false, x, y });
                    }
                    return false;
                }
                Pressed::remove(&mut p.game, button) || !captured
            }
            MOUSE_WHEEL => {
                let (x, y) = e.wheel_at();
                if captured || self.on_pinned(x, y) {
                    let (dx, dy) = e.wheel();
                    self.queue(UiEvent::Wheel { dx, dy });
                    return false;
                }
                true
            }
            // touch, pinch and pen: the panel doesn't use them, the game doesn't get them while it's open
            0x700..=0x7ff | 0x1300..=0x13ff => !captured,
            WINDOW_RESIZED => {
                let (w, h) = e.data();
                self.set_points(w as f32, h as f32);
                true
            }
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAB: u32 = 43;
    const W: u32 = 26;
    const LGUI: u32 = 227;
    const GUI_MOD: u16 = 0x0400;

    fn key(down: bool, scancode: u32, keymod: u16) -> Event {
        let mut e = Event::new(if down { KEY_DOWN } else { KEY_UP });
        e.put(24, scancode);
        e.put(32, keymod);
        e.put(36, down as u8);
        e
    }

    fn button(down: bool, b: u8, x: f32, y: f32) -> Event {
        let mut e = Event::new(if down { MOUSE_BUTTON_DOWN } else { MOUSE_BUTTON_UP });
        e.put(24, b);
        e.put(25, down as u8);
        e.put(28, x);
        e.put(32, y);
        e
    }

    fn panel() -> Input {
        let input = Input::new();
        input.set_hotkey(Key { scancode: TAB, mods: MOD_SUPER });
        input
    }

    #[test]
    fn the_key_opens_and_the_game_never_sees_it() {
        let input = panel();
        assert!(input.filter(&key(true, LGUI, GUI_MOD)), "Super alone is the game's");
        assert!(!input.filter(&key(true, TAB, GUI_MOD)));
        assert!(input.captured.load(Ordering::Relaxed));
        assert_eq!(input.take_toggles(), 1);
        // the panel releases what the game held (Super) ...
        assert_eq!(input.release_held(), vec![Held::Key(LGUI)]);
        // ... so the real releases of both are swallowed
        assert!(!input.filter(&key(false, TAB, GUI_MOD)));
        assert!(!input.filter(&key(false, LGUI, 0)));
        // Tab without Super is just Tab, for the panel while it's open
        assert!(!input.filter(&key(true, TAB, 0)));
        assert_eq!(input.take_toggles(), 0);
    }

    #[test]
    fn a_held_key_is_released_once_and_its_real_release_swallowed() {
        let input = panel();
        assert!(input.filter(&key(true, W, 0)));
        assert!(!input.filter(&key(true, TAB, GUI_MOD)));
        assert_eq!(input.release_held(), vec![Held::Key(W)]);
        assert!(!input.filter(&key(true, W, 0)), "a repeat of W isn't the game's while open");
        assert!(!input.filter(&key(false, W, 0)));
        input.captured.store(false, Ordering::Relaxed);
        assert!(input.filter(&key(true, W, 0)), "after closing, W is the game's again");
        assert!(input.filter(&key(false, W, 0)));
    }

    #[test]
    fn a_release_the_game_is_owed_passes_even_while_open() {
        let input = panel();
        assert!(input.filter(&button(true, 3, 10.0, 10.0)));
        input.captured.store(true, Ordering::Relaxed);
        // released before the main thread got to it: the game still gets it
        assert!(input.filter(&button(false, 3, 10.0, 10.0)));
        assert_eq!(input.release_held(), vec![]);
    }

    #[test]
    fn while_open_the_mouse_and_keys_go_to_the_panel() {
        let input = panel();
        input.captured.store(true, Ordering::Relaxed);
        let mut motion = Event::new(MOUSE_MOTION);
        motion.put(28, 100.0f32);
        motion.put(32, 50.0f32);
        assert!(!input.filter(&motion));
        assert!(!input.filter(&button(true, 1, 100.0, 50.0)));
        assert!(!input.filter(&button(false, 1, 100.0, 50.0)));
        assert!(!input.filter(&key(true, SCANCODE_ESCAPE, 0)));
        assert!(!input.filter(&Event::new(TEXT_INPUT)));
        assert!(input.filter(&Event::new(QUIT)), "quitting is always the game's");
        assert!(input.filter(&Event::new(USER)), "the game's own events always pass");
        assert_eq!(input.drain(), vec![
            UiEvent::Move(100.0, 50.0),
            UiEvent::Button { button: 1, down: true, x: 100.0, y: 50.0 },
            UiEvent::Button { button: 1, down: false, x: 100.0, y: 50.0 },
            UiEvent::Key { scancode: SCANCODE_ESCAPE, down: true, mods: 0 },
        ]);
        input.captured.store(false, Ordering::Relaxed);
        assert!(input.filter(&motion));
        assert!(input.drain().is_empty());
    }

    #[test]
    fn closing_with_the_key_keeps_its_release_from_the_game() {
        let input = panel();
        input.captured.store(true, Ordering::Relaxed);
        assert!(!input.filter(&key(true, LGUI, GUI_MOD)));
        assert!(!input.filter(&key(true, TAB, GUI_MOD)));
        assert_eq!(input.take_toggles(), 1);
        input.captured.store(false, Ordering::Relaxed); // the panel closed
        assert!(!input.filter(&key(false, TAB, GUI_MOD)));
        assert!(!input.filter(&key(false, LGUI, 0)));
        assert!(input.filter(&key(true, W, 0)));
    }

    #[test]
    fn synthetic_and_off_pass_everything() {
        let input = panel();
        input.captured.store(true, Ordering::Relaxed);
        assert!(synthetic(|| input.filter(&key(false, W, 0))));
        input.off.store(true, Ordering::Relaxed);
        assert!(input.filter(&key(true, TAB, GUI_MOD)));
    }

    #[test]
    fn modifiers_must_match_exactly() {
        let input = panel();
        assert!(input.filter(&key(true, TAB, GUI_MOD | 0x0001)), "Super+Shift+Tab isn't the key");
        assert!(!input.captured.load(Ordering::Relaxed));
        assert!(input.filter(&key(true, TAB, 0)));
    }

    #[test]
    fn a_pinned_card_takes_its_clicks_with_the_panel_closed() {
        let input = panel();
        input.set_hits(vec![[100.0, 100.0, 300.0, 200.0]]);
        let mut motion = Event::new(MOUSE_MOTION);
        motion.put(28, 150.0f32);
        motion.put(32, 150.0f32);
        // the game sees the pointer move; the card does too, for hovering
        assert!(input.filter(&motion));
        assert_eq!(input.drain(), [UiEvent::Move(150.0, 150.0)]);
        // a click on it is the card's, its release too
        assert!(!input.filter(&button(true, 1, 150.0, 150.0)));
        assert!(!input.filter(&button(false, 1, 150.0, 150.0)));
        let got = input.drain();
        assert!(got.contains(&UiEvent::Button { button: 1, down: true, x: 150.0, y: 150.0 }), "{got:?}");
        assert!(got.contains(&UiEvent::Button { button: 1, down: false, x: 150.0, y: 150.0 }), "{got:?}");
        // one elsewhere is the game's
        assert!(input.filter(&button(true, 1, 500.0, 150.0)));
        assert!(input.filter(&button(false, 1, 500.0, 150.0)));
        // with the mouse held by the game (its camera), the card isn't clickable
        input.set_mouse_free(false);
        assert!(input.filter(&button(true, 1, 150.0, 150.0)));
        assert!(input.filter(&button(false, 1, 150.0, 150.0)));
        // nothing pinned: nothing changes
        input.set_mouse_free(true);
        input.set_hits(Vec::new());
        input.drain();
        assert!(input.filter(&motion));
        assert!(input.drain().is_empty());
    }
}

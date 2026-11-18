//! The game's own SDL3, found where Rosemary loaded it. The panel uses SDL's
//! public API only: an event filter for input, and a few main-thread calls to
//! free and restore the mouse. SDL is never loaded here, only looked up.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::input::{self, Event, Held};
use crate::{guard, note, panel};

type Window = *mut c_void;
type EventFilter = unsafe extern "C" fn(*mut c_void, *mut Event) -> bool;
type MainThreadCallback = unsafe extern "C" fn(*mut c_void);

macro_rules! sdl_table {
    ($($field:ident: $name:literal => fn($($arg:ty),*) $(-> $ret:ty)?;)*) => {
        pub struct Sdl { $($field: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)* }
        impl Sdl {
            /// SDL3 as already loaded in this process, if every function the panel uses is there.
            fn find() -> Option<Sdl> {
                // SAFETY: RTLD_NOLOAD only finds a library already loaded (by soname);
                // each symbol has SDL3's documented signature.
                unsafe {
                    let lib = libc::dlopen(c"libSDL3.so.0".as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD);
                    if lib.is_null() {
                        return None;
                    }
                    let sdl = Sdl { $($field: {
                        let symbol = libc::dlsym(lib, concat!($name, "\0").as_ptr().cast());
                        if symbol.is_null() {
                            note!("the game's SDL3 has no {}", $name);
                            return None;
                        }
                        std::mem::transmute::<*mut c_void, unsafe extern "C" fn($($arg),*) $(-> $ret)?>(symbol)
                    },)* };
                    Some(sdl)
                }
            }
        }
    };
}

sdl_table! {
    version: "SDL_GetVersion" => fn() -> c_int;
    was_init: "SDL_WasInit" => fn(u32) -> u32;
    video_driver: "SDL_GetCurrentVideoDriver" => fn() -> *const c_char;
    set_event_filter: "SDL_SetEventFilter" => fn(Option<EventFilter>, *mut c_void);
    get_event_filter: "SDL_GetEventFilter" => fn(*mut Option<EventFilter>, *mut *mut c_void) -> bool;
    run_on_main_thread: "SDL_RunOnMainThread" => fn(MainThreadCallback, *mut c_void, bool) -> bool;
    push_event: "SDL_PushEvent" => fn(*mut Event) -> bool;
    keyboard_focus: "SDL_GetKeyboardFocus" => fn() -> Window;
    windows: "SDL_GetWindows" => fn(*mut c_int) -> *mut Window;
    free: "SDL_free" => fn(*mut c_void);
    window_id: "SDL_GetWindowID" => fn(Window) -> u32;
    window_size: "SDL_GetWindowSize" => fn(Window, *mut c_int, *mut c_int) -> bool;
    get_relative: "SDL_GetWindowRelativeMouseMode" => fn(Window) -> bool;
    set_relative: "SDL_SetWindowRelativeMouseMode" => fn(Window, bool) -> bool;
    cursor_visible: "SDL_CursorVisible" => fn() -> bool;
    show_cursor: "SDL_ShowCursor" => fn() -> bool;
    hide_cursor: "SDL_HideCursor" => fn() -> bool;
    warp_mouse: "SDL_WarpMouseInWindow" => fn(Window, f32, f32);
    mouse_state: "SDL_GetMouseState" => fn(*mut f32, *mut f32) -> u32;
    key_from_scancode: "SDL_GetKeyFromScancode" => fn(u32, u16, bool) -> u32;
    set_clipboard_text: "SDL_SetClipboardText" => fn(*const c_char) -> bool;
    ticks_ns: "SDL_GetTicksNS" => fn() -> u64;
}

const INIT_VIDEO: u32 = 0x20;

static SDL: OnceLock<Sdl> = OnceLock::new();

/// The game's SDL3, once it's loaded (3.2 or newer) with its video started.
pub fn get() -> Option<&'static Sdl> {
    if let Some(s) = SDL.get() {
        return Some(s);
    }
    let found = Sdl::find()?;
    // SAFETY: plain SDL queries.
    let (version, video) = unsafe { ((found.version)(), (found.was_init)(INIT_VIDEO)) };
    if version < 3_002_000 || video == 0 {
        return None;
    }
    Some(SDL.get_or_init(|| found))
}

pub fn video_driver() -> String {
    let Some(s) = get() else { return "none".into() };
    // SAFETY: SDL returns a static string or NULL.
    unsafe {
        let name = (s.video_driver)();
        if name.is_null() { "none".into() } else { CStr::from_ptr(name).to_string_lossy().into_owned() }
    }
}

static PREVIOUS_FILTER: AtomicUsize = AtomicUsize::new(0);
static PREVIOUS_DATA: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static IN_FILTER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Every event SDL queues for the game passes through here first.
unsafe extern "C" fn filter(_: *mut c_void, event: *mut Event) -> bool {
    if event.is_null() {
        return true;
    }
    IN_FILTER.with(|f| f.set(true));
    // SAFETY: SDL hands a valid event for the duration of the call.
    let keep = guard(true, || {
        let e = unsafe { &*event };
        if e.kind() == input::USER && e.code() == input::DUMP_CODE {
            if e.data1() != 0 {
                // SAFETY: tests pass a C string in data1.
                let name = unsafe { CStr::from_ptr(e.data1() as *const c_char) }.to_string_lossy().into_owned();
                crate::debug::request(name);
            }
            return true;
        }
        panel::INPUT.filter(e)
    });
    IN_FILTER.with(|f| f.set(false));
    if !keep {
        return false;
    }
    let previous = PREVIOUS_FILTER.load(Ordering::Relaxed);
    if previous != 0 {
        // SAFETY: the filter that was installed before ours, with its own data.
        unsafe {
            let previous: EventFilter = std::mem::transmute(previous);
            return previous(PREVIOUS_DATA.load(Ordering::Relaxed) as *mut c_void, event);
        }
    }
    true
}

fn install_filter(s: &Sdl) {
    // SAFETY: SDL's filter API, which may be called from any thread.
    unsafe {
        let mut current: Option<EventFilter> = None;
        let mut data = std::ptr::null_mut();
        let has = (s.get_event_filter)(&mut current, &mut data);
        if has && current.is_some_and(|f| std::ptr::eq(f as *const (), filter as *const ())) {
            return;
        }
        let (previous, data) = if has { (current.map_or(0, |f| f as *const () as usize), data as usize) } else { (0, 0) };
        PREVIOUS_DATA.store(data, Ordering::Relaxed);
        PREVIOUS_FILTER.store(previous, Ordering::Relaxed);
        (s.set_event_filter)(Some(filter), std::ptr::null_mut());
    }
}

/// What the main thread did to the mouse for the panel, to undo on close.
struct Mouse {
    freed: bool,
    /// The game had (or asked for) the mouse locked to it.
    game_relative: bool,
    /// The game had the cursor hidden.
    game_hidden: bool,
}

static MOUSE: Mutex<Mouse> = Mutex::new(Mouse { freed: false, game_relative: false, game_hidden: false });
static TICK_QUEUED: AtomicBool = AtomicBool::new(false);
static CLIPBOARD: Mutex<Option<String>> = Mutex::new(None);

/// Have the main thread look at the panel's state soon (SDL's window and mouse
/// calls belong there). At most one is queued; it never waits.
pub fn schedule_tick() {
    if IN_FILTER.with(|f| f.get()) {
        return;
    }
    let Some(s) = get() else { return };
    if TICK_QUEUED.swap(true, Ordering::AcqRel) {
        return;
    }
    // SAFETY: SDL's own way to run code on the main thread; with wait = false it only queues.
    if !unsafe { (s.run_on_main_thread)(tick, std::ptr::null_mut(), false) } {
        TICK_QUEUED.store(false, Ordering::Release);
    }
}

pub fn copy(text: String) {
    *CLIPBOARD.lock().unwrap() = Some(text);
    schedule_tick();
}

unsafe extern "C" fn tick(_: *mut c_void) {
    TICK_QUEUED.store(false, Ordering::Release);
    guard((), main_tick);
}

fn focused_window(s: &Sdl) -> Option<Window> {
    // SAFETY: main-thread SDL queries; the window list is SDL-allocated and freed here.
    unsafe {
        let focused = (s.keyboard_focus)();
        if !focused.is_null() {
            return Some(focused);
        }
        let mut count = 0;
        let list = (s.windows)(&mut count);
        if list.is_null() {
            return None;
        }
        let first = (count > 0).then(|| *list).filter(|w| !w.is_null());
        (s.free)(list.cast());
        first
    }
}

fn main_tick() {
    let Some(s) = get() else { return };
    install_filter(s);
    let Some(window) = focused_window(s) else { return };
    let input = &panel::INPUT;
    // SAFETY: SDL window and mouse calls, on the main thread (where this runs).
    unsafe {
        input.window.store((s.window_id)(window), Ordering::Relaxed);
        let (mut w, mut h) = (0, 0);
        if (s.window_size)(window, &mut w, &mut h) {
            input.set_points(w as f32, h as f32);
        }
        let captured = input.captured.load(Ordering::Relaxed) && !input.off.load(Ordering::Relaxed);
        let mut mouse = MOUSE.lock().unwrap();
        if captured && !mouse.freed {
            mouse.game_relative = (s.get_relative)(window);
            mouse.game_hidden = !(s.cursor_visible)();
            mouse.freed = true;
            drop(mouse);
            release(s, input.window.load(Ordering::Relaxed), input.release_held());
            if (s.get_relative)(window) {
                // put the pointer on the bar before the game lets go of it
                (s.warp_mouse)(window, w as f32 / 2.0, crate::ui::BAR_TOP + crate::ui::BAR_HEIGHT / 2.0);
                (s.set_relative)(window, false);
            }
            (s.show_cursor)();
        } else if captured {
            // the game may lock the mouse again while the panel is open: it wants it back on close
            if (s.get_relative)(window) {
                mouse.game_relative = true;
                (s.set_relative)(window, false);
            }
            if !(s.cursor_visible)() {
                mouse.game_hidden = true;
                (s.show_cursor)();
            }
        } else if mouse.freed {
            mouse.freed = false;
            if mouse.game_relative {
                (s.set_relative)(window, true);
            }
            if mouse.game_hidden {
                (s.hide_cursor)();
            }
        }
        if let Some(text) = CLIPBOARD.lock().unwrap().take()
            && let Ok(text) = std::ffi::CString::new(text)
        {
            (s.set_clipboard_text)(text.as_ptr());
        }
    }
}

/// Tell the game everything it held is let go, through its own event queue.
fn release(s: &Sdl, window: u32, held: Vec<Held>) {
    if held.is_empty() {
        return;
    }
    // SAFETY: fully initialised 128-byte events pushed onto SDL's queue, on the main thread.
    unsafe {
        let now = (s.ticks_ns)();
        let (mut x, mut y) = (0.0f32, 0.0f32);
        (s.mouse_state)(&mut x, &mut y);
        for h in held {
            let mut e = match h {
                Held::Key(scancode) => {
                    let mut e = Event::new(input::KEY_UP);
                    e.put(24, scancode);
                    e.put(28, (s.key_from_scancode)(scancode, 0, true));
                    e
                }
                Held::Button(button) => {
                    let mut e = Event::new(input::MOUSE_BUTTON_UP);
                    e.put(24, button);
                    e.put(26, 1u8);
                    e.put(28, x);
                    e.put(32, y);
                    e
                }
            };
            e.put(8, now);
            e.put(16, window);
            input::synthetic(|| (s.push_event)(&mut e));
        }
    }
}

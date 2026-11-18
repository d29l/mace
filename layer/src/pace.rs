//! The game while you're not using it: a lower frame rate when its window is
//! in the background, and a small nudge now and then so Roblox doesn't kick
//! it for idling. Both are set by Mace (`Config`); the decisions are plain
//! functions so they can be tested without a game.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use mace_proto::ToMace;

use crate::{client, sdl};

/// The game's window has keyboard focus. Assumed until SDL says otherwise.
static FOCUSED: AtomicBool = AtomicBool::new(true);
/// Frames per second while unfocused; 0 = no limit.
static BACKGROUND_FPS: AtomicU32 = AtomicU32::new(0);
static ANTI_AFK: AtomicBool = AtomicBool::new(false);
/// Milliseconds since `start()` of the player's last input, and of the last nudge.
static LAST_INPUT: AtomicU64 = AtomicU64::new(0);
static LAST_NUDGE: AtomicU64 = AtomicU64::new(0);
/// A nudge for the main thread to send.
static NUDGE_DUE: AtomicBool = AtomicBool::new(false);
/// The mouse SDL reports the player's motion from.
static MOUSE_ID: AtomicU32 = AtomicU32::new(0);
static LAST_PRESENT: Mutex<Option<Instant>> = Mutex::new(None);
/// Wakes the pace thread when focus changes (sent from SDL's event filter, which mustn't block).
static WAKE: OnceLock<Mutex<Sender<()>>> = OnceLock::new();

/// Roblox kicks after 20 idle minutes; a nudge comes after this much quiet.
const IDLE: Duration = Duration::from_secs(5 * 60);

fn clock() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

fn now_ms() -> u64 {
    clock().elapsed().as_millis() as u64
}

/// How long the game may go without input before it's nudged (`MACE_PANEL_AFK_SECONDS` in tests).
fn idle_limit() -> Duration {
    static LIMIT: OnceLock<Duration> = OnceLock::new();
    *LIMIT.get_or_init(|| {
        std::env::var("MACE_PANEL_AFK_SECONDS").ok().and_then(|s| s.parse::<u64>().ok()).filter(|s| *s > 0)
            .map_or(IDLE, Duration::from_secs)
    })
}

/// Mace's settings for this game.
pub fn configure(background_fps: u32, anti_afk: bool) {
    BACKGROUND_FPS.store(background_fps, Ordering::Relaxed);
    ANTI_AFK.store(anti_afk, Ordering::Relaxed);
}

/// The window gained or lost keyboard focus (from SDL's event filter).
pub fn focus(gained: bool) {
    let was = FOCUSED.swap(gained, Ordering::Relaxed);
    if gained && !was {
        wake();
    }
}

/// The focus SDL's main thread sees at the start; from then on SDL's focus
/// events keep it.
pub fn seed_focus(focused: bool) {
    static SEEDED: AtomicBool = AtomicBool::new(false);
    if !SEEDED.swap(true, Ordering::AcqRel) {
        focus(focused);
    }
}

/// The player touched the game (from SDL's event filter; never the panel's own events).
pub fn input_seen(mouse: Option<u32>) {
    LAST_INPUT.store(now_ms(), Ordering::Relaxed);
    if let Some(id) = mouse {
        MOUSE_ID.store(id, Ordering::Relaxed);
    }
}

pub fn mouse_id() -> u32 {
    MOUSE_ID.load(Ordering::Relaxed)
}

/// Whether the main thread should nudge the game now (taking the request).
pub fn take_nudge() -> bool {
    NUDGE_DUE.swap(false, Ordering::AcqRel)
}

/// How long to hold a present so frames come no faster than `fps`.
pub fn hold(last: Option<Instant>, now: Instant, fps: u32) -> Duration {
    match last {
        Some(last) if fps > 0 => (last + Duration::from_secs_f64(1.0 / f64::from(fps))).saturating_duration_since(now),
        _ => Duration::ZERO,
    }
}

/// Whether presents are slowed right now.
fn throttled(panel_open: bool) -> Option<u32> {
    let fps = BACKGROUND_FPS.load(Ordering::Relaxed);
    (fps > 0 && !panel_open && !FOCUSED.load(Ordering::Relaxed)).then_some(fps)
}

/// Called before each of the game's presents: waits out the rest of the frame
/// when the window is in the background.
pub fn before_present(panel_open: bool) {
    let Some(fps) = throttled(panel_open) else {
        if BACKGROUND_FPS.load(Ordering::Relaxed) > 0 {
            *LAST_PRESENT.lock().unwrap() = Some(Instant::now());
        }
        return;
    };
    let wait = hold(*LAST_PRESENT.lock().unwrap(), Instant::now(), fps);
    if !wait.is_zero() {
        // never longer than a second, whatever the clock did
        std::thread::sleep(wait.min(Duration::from_secs(1)));
    }
    *LAST_PRESENT.lock().unwrap() = Some(Instant::now());
}

/// A nudge is due when anti-AFK is on, the panel is closed, and neither the
/// player nor a nudge has touched the game for `idle`.
pub fn nudge_due(enabled: bool, panel_open: bool, last_input: u64, last_nudge: u64, now: u64, idle: Duration) -> bool {
    enabled && !panel_open && now.saturating_sub(last_input.max(last_nudge)) >= idle.as_millis() as u64
}

fn wake() {
    if let Some(wake) = WAKE.get()
        && let Ok(sender) = wake.try_lock()
    {
        let _ = sender.send(());
    }
}

/// Start watching (once per process): tells Mace when this window gains
/// focus, and asks the main thread for a nudge when the game sits idle.
pub fn start(panel_open: fn() -> bool) {
    let (sender, receiver) = mpsc::channel::<()>();
    if WAKE.set(Mutex::new(sender)).is_err() {
        return;
    }
    clock();
    let _ = std::thread::Builder::new().name("mace-pace".into()).spawn(move || {
        let check = idle_limit().min(Duration::from_secs(60)) / 4;
        let mut told = false;
        loop {
            match receiver.recv_timeout(check) {
                Ok(()) => told = false,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            // Mace learns which game you used last (it keeps that one at full speed)
            if FOCUSED.load(Ordering::Relaxed) && !told {
                told = client::send(&ToMace::Event { what: "focused".into() });
            }
            let now = now_ms();
            let due = nudge_due(ANTI_AFK.load(Ordering::Relaxed), panel_open(), LAST_INPUT.load(Ordering::Relaxed),
                                LAST_NUDGE.load(Ordering::Relaxed), now, idle_limit());
            if due {
                LAST_NUDGE.store(now, Ordering::Relaxed);
                NUDGE_DUE.store(true, Ordering::Release);
                sdl::schedule_tick();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_held_to_the_rate() {
        let start = Instant::now();
        assert_eq!(hold(None, start, 10), Duration::ZERO);
        assert_eq!(hold(Some(start), start, 0), Duration::ZERO);
        let wait = hold(Some(start), start + Duration::from_millis(30), 10);
        assert!(wait > Duration::from_millis(69) && wait <= Duration::from_millis(70), "{wait:?}");
        // a slow frame isn't held at all
        assert_eq!(hold(Some(start), start + Duration::from_millis(250), 10), Duration::ZERO);
    }

    #[test]
    fn nudges_come_only_after_quiet() {
        let idle = Duration::from_secs(300);
        let minute = 60_000;
        assert!(!nudge_due(true, false, 0, 0, 4 * minute, idle));
        assert!(nudge_due(true, false, 0, 0, 5 * minute, idle));
        // off, or the panel is open
        assert!(!nudge_due(false, false, 0, 0, 30 * minute, idle));
        assert!(!nudge_due(true, true, 0, 0, 30 * minute, idle));
        // the player was here a minute ago
        assert!(!nudge_due(true, false, 29 * minute, 0, 30 * minute, idle));
        // the last nudge counts like input: one every five minutes, not every check
        assert!(!nudge_due(true, false, 0, 26 * minute, 30 * minute, idle));
        assert!(nudge_due(true, false, 0, 25 * minute, 30 * minute, idle));
    }
}

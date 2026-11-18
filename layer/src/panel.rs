//! The panel in this process: whether it's open, how far it has slid in, and
//! each frame's UI, built on the thread that presents.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use ash::vk;
use mace_proto::ToMace;

use crate::input::{Input, UiEvent};
use crate::ui::{Action, Ui};
use crate::{client, debug, note, pace, sdl};

pub static INPUT: Input = Input::new();

/// Coming in eases out over a little longer than going away.
const OPEN_SECONDS: f32 = 0.24;
const CLOSE_SECONDS: f32 = 0.16;

static DISABLED: AtomicBool = AtomicBool::new(false);
/// Open, sliding or showing toasts: the only thing the present hook checks otherwise.
static VISIBLE: AtomicBool = AtomicBool::new(false);
static ACTIVATED: AtomicBool = AtomicBool::new(false);
/// A toast came in while nothing was drawn.
static TOAST_PENDING: AtomicBool = AtomicBool::new(false);

pub fn toast_arrived() {
    TOAST_PENDING.store(true, Ordering::Release);
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1")
}

/// The socket Mace gave, if this game should have the panel.
fn socket() -> Option<&'static str> {
    static SOCKET: OnceLock<Option<String>> = OnceLock::new();
    SOCKET.get_or_init(|| std::env::var("MACE_PANEL").ok().filter(|s| !s.is_empty())).as_deref()
}

/// Tests draw the panel without SDL or Mace.
pub fn force_open() -> bool {
    static FORCE: OnceLock<bool> = OnceLock::new();
    *FORCE.get_or_init(|| env_flag("MACE_PANEL_FORCE_OPEN"))
}

pub fn verbose() -> bool {
    static VERBOSE: OnceLock<bool> = OnceLock::new();
    *VERBOSE.get_or_init(|| env_flag("MACE_PANEL_LOG"))
}

/// Mace asked for the panel in this process.
pub fn wanted() -> bool {
    socket().is_some() || force_open() || debug::fake().is_some()
}

/// A swapchain made now could have the panel: SDL3 (the game's input) is here.
pub fn can_draw() -> bool {
    !DISABLED.load(Ordering::Relaxed) && wanted() && (force_open() || sdl::get().is_some())
}

pub fn video_driver() -> String {
    sdl::video_driver()
}

/// Turn the panel off for the rest of this session; the game carries on as if it was never there.
pub fn disable(why: &str) {
    if DISABLED.swap(true, Ordering::AcqRel) {
        return;
    }
    note!("the panel is off for this session: {why}");
    VISIBLE.store(false, Ordering::Release);
    INPUT.captured.store(false, Ordering::Relaxed);
    INPUT.off.store(true, Ordering::Relaxed);
    // give the mouse back if the panel had it
    sdl::schedule_tick();
}

/// The first swapchain the panel can draw on: connect to Mace and start
/// listening to the game's input.
pub fn activate() {
    static PID: OnceLock<u32> = OnceLock::new();
    let pid = std::process::id();
    if *PID.get_or_init(|| pid) != pid || ACTIVATED.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Some(file) = debug::fake() {
        client::start_fake(file);
    } else if let Some(path) = socket() {
        client::start(path.to_string(), ToMace::Hello {
            v: mace_proto::VERSION, pid, video: video_driver(), swapchain: String::new(),
        });
    }
    pace::start(|| INPUT.captured.load(Ordering::Relaxed));
    sdl::schedule_tick();
}

/// Whether presents need the panel's attention at all.
pub fn visible() -> bool {
    !DISABLED.load(Ordering::Relaxed)
        && (VISIBLE.load(Ordering::Acquire) || INPUT.toggle_pending() || TOAST_PENDING.load(Ordering::Acquire)
            || debug::pending() || force_open())
}

/// One frame of the panel, for the renderer.
pub struct Frame {
    /// 0..1: how much of the game is blurred (follows the slide).
    pub blur: f32,
    pub ppp: f32,
    pub primitives: Vec<egui::ClippedPrimitive>,
    pub textures: Deltas,
    pub dump: Option<String>,
}

struct Panel {
    ctx: egui::Context,
    ui: Ui,
    open: bool,
    progress: f32,
    last: Instant,
    start: Instant,
    frames: u64,
    asked: Instant,
}

static PANEL: Mutex<Option<Panel>> = Mutex::new(None);

fn ease(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

fn open_panel(p: &mut Panel) {
    p.open = true;
    // while closed no frames ran: the slide starts from now, not from the last frame drawn
    p.last = Instant::now();
    INPUT.captured.store(true, Ordering::Relaxed);
    p.ui.opened();
    client::send(&ToMace::State);
    client::send(&ToMace::Event { what: "open".into() });
    p.asked = Instant::now();
}

fn close_panel(p: &mut Panel) {
    p.open = false;
    INPUT.captured.store(false, Ordering::Relaxed);
    client::send(&ToMace::Event { what: "closed".into() });
}

/// The panel's next frame for a swapchain of this size, or None when there's
/// nothing to draw (closed, and no test picture wanted).
pub fn frame(extent: vk::Extent2D) -> Option<Frame> {
    if DISABLED.load(Ordering::Relaxed) {
        return None;
    }
    let mut guard = PANEL.lock().unwrap();
    let p = guard.get_or_insert_with(|| {
        let ctx = egui::Context::default();
        crate::ui::setup(&ctx);
        Panel { ctx, ui: Ui::default(), open: false, progress: 0.0, last: Instant::now(), start: Instant::now(),
                frames: 0, asked: Instant::now() }
    });
    p.frames += 1;
    for _ in 0..INPUT.take_toggles() {
        if p.open { close_panel(p) } else { open_panel(p) }
    }
    if force_open() && !p.open {
        open_panel(p);
    }
    // a slow frame (the first open builds the fonts) moves the slide one ordinary step, never a leap
    let dt = if debug::test_clock() { 1.0 / 60.0 } else { p.last.elapsed().as_secs_f32().min(1.0 / 30.0) };
    p.last = Instant::now();
    let target = if p.open { 1.0 } else { 0.0 };
    let step = dt / if p.open { OPEN_SECONDS } else { CLOSE_SECONDS };
    p.progress = if p.progress < target { (p.progress + step).min(target) } else { (p.progress - step).max(target) };
    let toasts = TOAST_PENDING.swap(false, Ordering::AcqRel) || p.ui.has_toasts();
    let showing = p.open || p.progress > 0.0 || toasts;
    VISIBLE.store(showing, Ordering::Release);
    // the main thread frees the mouse while open, and gives it back after
    sdl::schedule_tick();
    let dump = debug::take();
    if !showing {
        INPUT.drain();
        return dump.map(|dump| Frame { blur: 0.0, ppp: 1.0, primitives: Vec::new(), textures: Deltas(Default::default()), dump: Some(dump) });
    }
    if p.open && p.asked.elapsed().as_secs_f32() >= 1.0 {
        client::send(&ToMace::State);
        p.asked = Instant::now();
    }

    let (w, h) = (extent.width as f32, extent.height as f32);
    let ppp = INPUT.points().map_or(1.0, |(pw, _)| (w / pw).clamp(0.5, 4.0));
    let mut raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w / ppp, h / ppp))),
        time: Some(if debug::test_clock() { p.frames as f64 / 60.0 } else { p.start.elapsed().as_secs_f64() }),
        focused: true,
        ..Default::default()
    };
    raw.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point = Some(ppp);
    let mut escape = false;
    for e in INPUT.drain() {
        match e {
            UiEvent::Move(x, y) => raw.events.push(egui::Event::PointerMoved(egui::pos2(x, y))),
            UiEvent::Button { button, down, x, y } => {
                let button = match button {
                    1 => egui::PointerButton::Primary,
                    2 => egui::PointerButton::Middle,
                    3 => egui::PointerButton::Secondary,
                    _ => continue,
                };
                raw.events.push(egui::Event::PointerButton {
                    pos: egui::pos2(x, y), button, pressed: down, modifiers: egui::Modifiers::NONE,
                });
            }
            UiEvent::Wheel { dx, dy } => raw.events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line, delta: egui::vec2(dx, dy), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Wheel,
                modifiers: egui::Modifiers::NONE,
            }),
            UiEvent::Key { scancode, down, .. } => {
                if scancode == crate::input::SCANCODE_ESCAPE && down {
                    escape = true;
                }
            }
        }
    }
    let data = crate::ui::Snapshot::take();
    let progress = ease(p.progress);
    let mut actions = Vec::new();
    let ui = &mut p.ui;
    let output = p.ctx.run_ui(raw, |root| actions = ui.show(root, progress, &data, escape));
    for action in actions {
        match action {
            Action::Close => close_panel(p),
            Action::Copy(text) => sdl::copy(text),
            Action::Send(message) => {
                client::send(&message);
            }
        }
    }
    let primitives = p.ctx.tessellate(output.shapes, output.pixels_per_point);
    // dimmed by the UI; blurred too only when that's asked for
    let blur = if data.config.as_ref().is_some_and(|c| c.blur) { progress } else { 0.0 };
    Some(Frame { blur, ppp: output.pixels_per_point, primitives, textures: Deltas(output.textures_delta), dump })
}

/// egui's texture changes for one frame. egui insists they're handled; once
/// the frame is drawn (or given up on) they're done with either way.
pub struct Deltas(pub egui::TexturesDelta);

impl Drop for Deltas {
    fn drop(&mut self) {
        self.0.clear();
    }
}

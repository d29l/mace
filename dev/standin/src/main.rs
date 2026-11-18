//! Plays Rosemary's part in the in-game panel's tests, with no Roblox anywhere:
//! SDL3 opened by name at run time (as Rosemary does), Vulkan through SDL, an
//! animated pattern, scripted input pushed through SDL's own event queue, and a
//! JSON-lines log of every event that reaches it and of its mouse state.
//!
//!   mace-standin [--sdl PATH] [--render-thread] [--format unorm|srgb]
//!                [--present fifo|mailbox|immediate] [--size WxH] [--frames N]
//!                [--script FILE] [--log FILE]
//!
//! Script lines, `FRAME COMMAND ARGS...` (`#` starts a comment):
//!   key SCANCODE down|up [MODS]   a key event (MODS: SDL_Keymod bits)
//!   motion X Y                    the mouse moved to X, Y (window points)
//!   button N down|up X Y
//!   wheel DY [X Y]
//!   relative on|off               the game locks or frees the mouse (as Roblox does)
//!   cursor show|hide
//!   resize W H
//!   dump NAME                     ask the panel to save this frame as NAME.png
//!   focus gained|lost            the window gains or loses keyboard focus
//!   quit

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ash::vk;

type Window = *mut c_void;

macro_rules! sdl_table {
    ($($field:ident: $name:literal => fn($($arg:ty),*) $(-> $ret:ty)?;)*) => {
        #[allow(dead_code)]
        struct Sdl { $($field: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)* }
        impl Sdl {
            fn open(path: &str) -> Sdl {
                let path = CString::new(path).unwrap();
                // SAFETY: dlopen/dlsym with valid strings; each symbol is SDL3's documented signature.
                unsafe {
                    let lib = libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL);
                    assert!(!lib.is_null(), "can't open SDL3: {:?}", CStr::from_ptr(libc::dlerror()));
                    Sdl { $($field: {
                        let symbol = libc::dlsym(lib, concat!($name, "\0").as_ptr().cast());
                        assert!(!symbol.is_null(), "SDL3 has no {}", $name);
                        std::mem::transmute::<*mut c_void, unsafe extern "C" fn($($arg),*) $(-> $ret)?>(symbol)
                    },)* }
                }
            }
        }
    };
}

sdl_table! {
    init: "SDL_Init" => fn(u32) -> bool;
    get_error: "SDL_GetError" => fn() -> *const c_char;
    create_window: "SDL_CreateWindow" => fn(*const c_char, c_int, c_int, u64) -> Window;
    vulkan_load_library: "SDL_Vulkan_LoadLibrary" => fn(*const c_char) -> bool;
    vulkan_gipa: "SDL_Vulkan_GetVkGetInstanceProcAddr" => fn() -> *const c_void;
    vulkan_extensions: "SDL_Vulkan_GetInstanceExtensions" => fn(*mut u32) -> *const *const c_char;
    vulkan_create_surface: "SDL_Vulkan_CreateSurface" => fn(Window, vk::Instance, *const c_void, *mut vk::SurfaceKHR) -> bool;
    poll_event: "SDL_PollEvent" => fn(*mut Event) -> bool;
    wait_event_timeout: "SDL_WaitEventTimeout" => fn(*mut Event, i32) -> bool;
    push_event: "SDL_PushEvent" => fn(*mut Event) -> bool;
    window_id: "SDL_GetWindowID" => fn(Window) -> u32;
    set_relative: "SDL_SetWindowRelativeMouseMode" => fn(Window, bool) -> bool;
    get_relative: "SDL_GetWindowRelativeMouseMode" => fn(Window) -> bool;
    cursor_visible: "SDL_CursorVisible" => fn() -> bool;
    show_cursor: "SDL_ShowCursor" => fn() -> bool;
    hide_cursor: "SDL_HideCursor" => fn() -> bool;
    size_in_pixels: "SDL_GetWindowSizeInPixels" => fn(Window, *mut c_int, *mut c_int) -> bool;
    set_window_size: "SDL_SetWindowSize" => fn(Window, c_int, c_int) -> bool;
    ticks_ns: "SDL_GetTicksNS" => fn() -> u64;
    quit: "SDL_Quit" => fn();
}

const INIT_VIDEO: u32 = 0x20;
const WINDOW_RESIZABLE: u64 = 0x20;
const WINDOW_VULKAN: u64 = 0x1000_0000;
const QUIT: u32 = 0x100;
const PIXEL_SIZE_CHANGED: u32 = 0x207;
const WINDOW_FOCUS_GAINED: u32 = 0x20e;
const WINDOW_FOCUS_LOST: u32 = 0x20f;
const KEY_DOWN: u32 = 0x300;
const KEY_UP: u32 = 0x301;
const MOUSE_MOTION: u32 = 0x400;
const MOUSE_BUTTON_DOWN: u32 = 0x401;
const MOUSE_BUTTON_UP: u32 = 0x402;
const MOUSE_WHEEL: u32 = 0x403;
const USER: u32 = 0x8000;
/// `SDL_EVENT_USER` with this code asks the panel layer for a dump (data1: the name).
const DUMP_CODE: i32 = 0x4d41_4345;

/// An `SDL_Event`: 128 bytes, read and written at the offsets SDL3's headers give.
#[repr(C, align(8))]
#[derive(Clone, Copy)]
struct Event([u8; 128]);

impl Event {
    fn new(kind: u32, window: u32, now: u64) -> Event {
        let mut event = Event([0; 128]);
        event.put(0, kind);
        event.put(8, now);
        event.put(16, window);
        event
    }
    fn put<T: Copy>(&mut self, at: usize, value: T) {
        // SAFETY: every offset used is inside the 128 bytes.
        unsafe { std::ptr::write_unaligned(self.0.as_mut_ptr().add(at).cast(), value) }
    }
    fn get<T: Copy>(&self, at: usize) -> T {
        // SAFETY: as above.
        unsafe { std::ptr::read_unaligned(self.0.as_ptr().add(at).cast()) }
    }
    fn kind(&self) -> u32 {
        self.get(0)
    }
    fn describe(&self) -> String {
        match self.kind() {
            KEY_DOWN | KEY_UP => format!(
                "\"ev\":\"{}\",\"scancode\":{},\"mod\":{},\"repeat\":{}",
                if self.kind() == KEY_DOWN { "key_down" } else { "key_up" },
                self.get::<u32>(24), self.get::<u16>(32), self.get::<u8>(37) != 0),
            MOUSE_MOTION => format!("\"ev\":\"motion\",\"x\":{},\"y\":{},\"xrel\":{}", self.get::<f32>(28), self.get::<f32>(32), self.get::<f32>(36)),
            MOUSE_BUTTON_DOWN | MOUSE_BUTTON_UP => format!(
                "\"ev\":\"{}\",\"button\":{},\"x\":{},\"y\":{}",
                if self.kind() == MOUSE_BUTTON_DOWN { "button_down" } else { "button_up" },
                self.get::<u8>(24), self.get::<f32>(28), self.get::<f32>(32)),
            MOUSE_WHEEL => format!("\"ev\":\"wheel\",\"y\":{}", self.get::<f32>(28)),
            PIXEL_SIZE_CHANGED => format!("\"ev\":\"pixel_size\",\"w\":{},\"h\":{}", self.get::<i32>(20), self.get::<i32>(24)),
            kind => format!("\"ev\":\"0x{kind:x}\""),
        }
    }
}

struct Options {
    sdl: String,
    render_thread: bool,
    srgb: bool,
    present: vk::PresentModeKHR,
    size: (i32, i32),
    frames: u64,
    script: Vec<(u64, Vec<String>)>,
    log: Option<String>,
}

fn options() -> Options {
    let mut o = Options {
        sdl: "libSDL3.so.0".into(), render_thread: false, srgb: false, present: vk::PresentModeKHR::FIFO,
        size: (1280, 800), frames: 0, script: Vec::new(), log: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| panic!("{arg} needs a value"));
        match arg.as_str() {
            "--sdl" => o.sdl = value(),
            "--render-thread" => o.render_thread = true,
            "--format" => o.srgb = value() == "srgb",
            "--present" => o.present = match value().as_str() {
                "mailbox" => vk::PresentModeKHR::MAILBOX,
                "immediate" => vk::PresentModeKHR::IMMEDIATE,
                _ => vk::PresentModeKHR::FIFO,
            },
            "--size" => {
                let v = value();
                let (w, h) = v.split_once('x').expect("--size WxH");
                o.size = (w.parse().unwrap(), h.parse().unwrap());
            }
            "--frames" => o.frames = value().parse().unwrap(),
            "--script" => {
                let text = std::fs::read_to_string(value()).expect("the script");
                for line in text.lines() {
                    let line = line.split('#').next().unwrap().trim();
                    let mut words = line.split_whitespace().map(String::from);
                    if let Some(frame) = words.next() {
                        o.script.push((frame.parse().expect("a frame number"), words.collect()));
                    }
                }
            }
            "--log" => o.log = Some(value()),
            other => panic!("unknown option {other}"),
        }
    }
    o
}

struct Log(Mutex<Box<dyn Write + Send>>);

impl Log {
    fn line(&self, frame: u64, body: &str) {
        let mut out = self.0.lock().unwrap();
        let _ = writeln!(out, "{{\"frame\":{frame},{body}}}");
        let _ = out.flush();
    }
}

fn main() {
    let o = options();
    let sdl = Sdl::open(&o.sdl);
    // SAFETY: plain SDL3 calls with valid arguments, all on this (the main) thread.
    let window = unsafe {
        if !(sdl.init)(INIT_VIDEO) {
            panic!("SDL_Init: {:?}", CStr::from_ptr((sdl.get_error)()));
        }
        assert!((sdl.vulkan_load_library)(std::ptr::null()), "SDL_Vulkan_LoadLibrary: {:?}", CStr::from_ptr((sdl.get_error)()));
        let title = CString::new("Mace stand-in").unwrap();
        let window = (sdl.create_window)(title.as_ptr(), o.size.0, o.size.1, WINDOW_VULKAN | WINDOW_RESIZABLE);
        assert!(!window.is_null(), "SDL_CreateWindow: {:?}", CStr::from_ptr((sdl.get_error)()));
        window
    };
    let log = Arc::new(Log(Mutex::new(match &o.log {
        Some(path) => Box::new(std::fs::File::create(path).expect("the log")),
        None => Box::new(std::io::stdout()),
    })));
    let (mut w, mut h) = (0, 0);
    // SAFETY: as above.
    unsafe { (sdl.size_in_pixels)(window, &mut w, &mut h) };
    let size = Arc::new(AtomicU64::new((w as u64) << 32 | h as u64));
    let gpu = Gpu::new(&sdl, window, o.srgb, o.present, (w as u32, h as u32));
    log.line(0, &format!("\"ev\":\"started\",\"format\":\"{:?}\",\"render_thread\":{}", gpu.format, o.render_thread));

    let frame = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let window_id = Arc::new(AtomicU32::new(0));
    // SAFETY: as above.
    window_id.store(unsafe { (sdl.window_id)(window) }, Ordering::Relaxed);

    let mut gpu = Some(gpu);
    let renderer = o.render_thread.then(|| {
        let (gpu, frame, stop, size) = (gpu.take().unwrap(), frame.clone(), stop.clone(), size.clone());
        std::thread::spawn(move || {
            let mut gpu = gpu;
            while !stop.load(Ordering::Relaxed) {
                let s = size.load(Ordering::Relaxed);
                gpu.draw(frame.load(Ordering::Relaxed), ((s >> 32) as u32, s as u32));
                frame.fetch_add(1, Ordering::Relaxed);
            }
            gpu.destroy();
        })
    });

    let mut script = o.script.clone();
    script.sort_by_key(|(f, _)| *f);
    let mut next = 0;
    let mut last_state = (None, None);
    let mut event = Event([0; 128]);
    let began = std::time::Instant::now();
    let mut second = began;
    loop {
        let now = frame.load(Ordering::Relaxed);
        // SAFETY: SDL event calls on the main thread with a 128-byte event buffer.
        unsafe {
            if o.render_thread && (sdl.wait_event_timeout)(&mut event, 5) {
                handle(&event, &log, now, &size, &stop);
            }
            while (sdl.poll_event)(&mut event) {
                handle(&event, &log, now, &size, &stop);
            }
        }
        while next < script.len() && script[next].0 <= now {
            run_step(&sdl, window, window_id.load(Ordering::Relaxed), &script[next].1, &log, now, &stop);
            next += 1;
        }
        // SAFETY: main-thread SDL queries.
        let state = unsafe { (Some((sdl.get_relative)(window)), Some((sdl.cursor_visible)())) };
        if state != last_state {
            log.line(now, &format!("\"ev\":\"state\",\"relative\":{},\"cursor\":{}", state.0.unwrap(), state.1.unwrap()));
            last_state = state;
        }
        // the frame count once a second, for tests of frame pacing
        if second.elapsed() >= std::time::Duration::from_secs(1) {
            second = std::time::Instant::now();
            log.line(now, &format!("\"ev\":\"second\",\"t\":{:.2}", began.elapsed().as_secs_f64()));
        }
        if let Some(gpu) = gpu.as_mut() {
            let s = size.load(Ordering::Relaxed);
            gpu.draw(now, ((s >> 32) as u32, s as u32));
            frame.fetch_add(1, Ordering::Relaxed);
        }
        if stop.load(Ordering::Relaxed) || (o.frames > 0 && frame.load(Ordering::Relaxed) >= o.frames) {
            break;
        }
    }
    stop.store(true, Ordering::Relaxed);
    if let Some(renderer) = renderer {
        let _ = renderer.join();
    }
    if let Some(gpu) = gpu.take() {
        gpu.destroy();
    }
    let frames = frame.load(Ordering::Relaxed);
    log.line(frames, &format!("\"ev\":\"done\",\"seconds\":{:.3},\"fps\":{:.1}", began.elapsed().as_secs_f64(),
                                frames as f64 / began.elapsed().as_secs_f64()));
    // SAFETY: last SDL call, main thread.
    unsafe { (sdl.quit)() };
}

fn handle(event: &Event, log: &Log, frame: u64, size: &AtomicU64, stop: &AtomicBool) {
    match event.kind() {
        QUIT => stop.store(true, Ordering::Relaxed),
        PIXEL_SIZE_CHANGED => {
            size.store((event.get::<i32>(20) as u64) << 32 | event.get::<i32>(24) as u32 as u64, Ordering::Relaxed)
        }
        _ => {}
    }
    log.line(frame, &event.describe());
}

fn run_step(sdl: &Sdl, window: Window, id: u32, step: &[String], log: &Log, frame: u64, stop: &AtomicBool) {
    let num = |i: usize| -> f32 { step.get(i).and_then(|s| s.parse().ok()).unwrap_or(0.0) };
    let down = |i: usize| step.get(i).is_some_and(|s| s == "down");
    // SAFETY: SDL calls on the main thread; pushed events are fully initialised 128-byte buffers.
    unsafe {
        let now = (sdl.ticks_ns)();
        let push = |mut event: Event| {
            (sdl.push_event)(&mut event);
        };
        match step.first().map(String::as_str) {
            Some("key") => {
                let mut e = Event::new(if down(2) { KEY_DOWN } else { KEY_UP }, id, now);
                e.put(24, num(1) as u32);
                e.put(32, num(3) as u16);
                e.put(36, down(2) as u8);
                push(e);
            }
            Some("motion") => {
                let mut e = Event::new(MOUSE_MOTION, id, now);
                e.put(28, num(1));
                e.put(32, num(2));
                push(e);
            }
            Some("button") => {
                let mut e = Event::new(if down(2) { MOUSE_BUTTON_DOWN } else { MOUSE_BUTTON_UP }, id, now);
                e.put(24, num(1) as u8);
                e.put(25, down(2) as u8);
                e.put(26, 1u8);
                e.put(28, num(3));
                e.put(32, num(4));
                push(e);
            }
            Some("wheel") => {
                let mut e = Event::new(MOUSE_WHEEL, id, now);
                e.put(28, num(1));
                e.put(36, num(2));
                e.put(40, num(3));
                push(e);
            }
            Some("relative") => {
                (sdl.set_relative)(window, step.get(1).is_some_and(|s| s == "on"));
            }
            Some("cursor") => {
                if step.get(1).is_some_and(|s| s == "show") { (sdl.show_cursor)() } else { (sdl.hide_cursor)() };
            }
            Some("resize") => {
                (sdl.set_window_size)(window, num(1) as c_int, num(2) as c_int);
            }
            Some("dump") => {
                let mut e = Event::new(USER, id, now);
                e.put(20, DUMP_CODE);
                let name = CString::new(step.get(1).cloned().unwrap_or_default()).unwrap();
                e.put(24, name.into_raw() as usize);
                push(e);
            }
            Some("focus") => {
                // what the compositor's keyboard enter / leave turns into
                let gained = step.get(1).is_some_and(|s| s == "gained");
                push(Event::new(if gained { WINDOW_FOCUS_GAINED } else { WINDOW_FOCUS_LOST }, id, now));
            }
            Some("quit") => stop.store(true, Ordering::Relaxed),
            _ => {}
        }
    }
    log.line(frame, &format!("\"ev\":\"step\",\"step\":\"{}\"", step.join(" ")));
}

/// Just enough Vulkan to put an animated pattern on the window's swapchain.
struct Gpu {
    instance: ash::Instance,
    surface_fn: ash::khr::surface::Instance,
    surface: vk::SurfaceKHR,
    physical: vk::PhysicalDevice,
    device: ash::Device,
    queue: vk::Queue,
    swap_fn: ash::khr::swapchain::Device,
    swapchain: vk::SwapchainKHR,
    format: vk::Format,
    present: vk::PresentModeKHR,
    extent: vk::Extent2D,
    views: Vec<vk::ImageView>,
    framebuffers: Vec<vk::Framebuffer>,
    render_done: Vec<vk::Semaphore>,
    render_pass: vk::RenderPass,
    pool: vk::CommandPool,
    commands: [vk::CommandBuffer; 2],
    acquired: [vk::Semaphore; 2],
    fences: [vk::Fence; 2],
    slot: usize,
}

// SAFETY: the renderer is used by one thread at a time (moved to the render thread whole).
unsafe impl Send for Gpu {}

impl Gpu {
    fn new(sdl: &Sdl, window: Window, srgb: bool, present: vk::PresentModeKHR, size: (u32, u32)) -> Gpu {
        // SAFETY: the usual Vulkan setup; every handle is created here and destroyed in `destroy`.
        unsafe {
            let gipa: vk::PFN_vkGetInstanceProcAddr = std::mem::transmute((sdl.vulkan_gipa)());
            let entry = ash::Entry::from_static_fn(ash::StaticFn { get_instance_proc_addr: gipa });
            let mut count = 0;
            let names = (sdl.vulkan_extensions)(&mut count);
            let extensions = std::slice::from_raw_parts(names, count as usize);
            let app = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_1);
            let instance = entry.create_instance(
                &vk::InstanceCreateInfo::default().application_info(&app).enabled_extension_names(extensions), None)
                .expect("vkCreateInstance");
            let mut surface = vk::SurfaceKHR::null();
            assert!((sdl.vulkan_create_surface)(window, instance.handle(), std::ptr::null(), &mut surface));
            let surface_fn = ash::khr::surface::Instance::new(&entry, &instance);
            let (physical, family) = instance.enumerate_physical_devices().unwrap().into_iter().find_map(|p| {
                instance.get_physical_device_queue_family_properties(p).iter().enumerate().find_map(|(i, q)| {
                    (q.queue_flags.contains(vk::QueueFlags::GRAPHICS)
                        && surface_fn.get_physical_device_surface_support(p, i as u32, surface).unwrap_or(false))
                        .then_some((p, i as u32))
                })
            }).expect("a GPU that can draw to the window");
            let priorities = [1.0];
            let queues = [vk::DeviceQueueCreateInfo::default().queue_family_index(family).queue_priorities(&priorities)];
            let device_extensions = [ash::khr::swapchain::NAME.as_ptr()];
            let device = instance.create_device(physical, &vk::DeviceCreateInfo::default()
                .queue_create_infos(&queues).enabled_extension_names(&device_extensions), None).expect("vkCreateDevice");
            let queue = device.get_device_queue(family, 0);
            let swap_fn = ash::khr::swapchain::Device::new(&instance, &device);
            let wanted = if srgb { vk::Format::B8G8R8A8_SRGB } else { vk::Format::B8G8R8A8_UNORM };
            let format = surface_fn.get_physical_device_surface_formats(physical, surface).unwrap().iter()
                .map(|f| f.format).find(|f| *f == wanted).unwrap_or(wanted);
            let render_pass = device.create_render_pass(&vk::RenderPassCreateInfo::default()
                .attachments(&[vk::AttachmentDescription::default().format(format).samples(vk::SampleCountFlags::TYPE_1)
                    .load_op(vk::AttachmentLoadOp::CLEAR).store_op(vk::AttachmentStoreOp::STORE)
                    .initial_layout(vk::ImageLayout::UNDEFINED).final_layout(vk::ImageLayout::PRESENT_SRC_KHR)])
                .subpasses(&[vk::SubpassDescription::default().color_attachments(&[vk::AttachmentReference::default()
                    .attachment(0).layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)])])
                .dependencies(&[vk::SubpassDependency::default().src_subpass(vk::SUBPASS_EXTERNAL).dst_subpass(0)
                    .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
                    .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
                    .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)]), None).unwrap();
            let pool = device.create_command_pool(&vk::CommandPoolCreateInfo::default().queue_family_index(family)
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER), None).unwrap();
            let commands = device.allocate_command_buffers(&vk::CommandBufferAllocateInfo::default()
                .command_pool(pool).command_buffer_count(2)).unwrap();
            let semaphore = || device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None).unwrap();
            let fence = || device.create_fence(&vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED), None).unwrap();
            let mut gpu = Gpu {
                acquired: [semaphore(), semaphore()], fences: [fence(), fence()],
                instance, surface_fn, surface, physical, device, queue, swap_fn, swapchain: vk::SwapchainKHR::null(),
                format, present, extent: vk::Extent2D::default(), views: Vec::new(), framebuffers: Vec::new(),
                render_done: Vec::new(), render_pass, pool, commands: [commands[0], commands[1]], slot: 0,
            };
            gpu.build(size);
            gpu
        }
    }

    fn build(&mut self, size: (u32, u32)) {
        // SAFETY: the device is idle when this runs (start, or after wait_idle in `draw`).
        unsafe {
            let caps = self.surface_fn.get_physical_device_surface_capabilities(self.physical, self.surface).unwrap();
            self.extent = if caps.current_extent.width != u32::MAX { caps.current_extent }
                          else { vk::Extent2D { width: size.0.max(1), height: size.1.max(1) } };
            let modes = self.surface_fn.get_physical_device_surface_present_modes(self.physical, self.surface).unwrap();
            let mode = if modes.contains(&self.present) { self.present } else { vk::PresentModeKHR::FIFO };
            let old = self.swapchain;
            self.swapchain = self.swap_fn.create_swapchain(&vk::SwapchainCreateInfoKHR::default()
                .surface(self.surface).min_image_count(caps.min_image_count.max(2).min(if caps.max_image_count == 0 { 8 } else { caps.max_image_count }))
                .image_format(self.format).image_color_space(vk::ColorSpaceKHR::SRGB_NONLINEAR)
                .image_extent(self.extent).image_array_layers(1).image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
                .pre_transform(caps.current_transform).composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
                .present_mode(mode).clipped(true).old_swapchain(old), None).expect("vkCreateSwapchainKHR");
            self.teardown_images();
            if old != vk::SwapchainKHR::null() {
                self.swap_fn.destroy_swapchain(old, None);
            }
            for image in self.swap_fn.get_swapchain_images(self.swapchain).unwrap() {
                let view = self.device.create_image_view(&vk::ImageViewCreateInfo::default().image(image)
                    .view_type(vk::ImageViewType::TYPE_2D).format(self.format)
                    .subresource_range(vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1).layer_count(1)), None).unwrap();
                self.framebuffers.push(self.device.create_framebuffer(&vk::FramebufferCreateInfo::default()
                    .render_pass(self.render_pass).attachments(&[view]).width(self.extent.width)
                    .height(self.extent.height).layers(1), None).unwrap());
                self.views.push(view);
                self.render_done.push(self.device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None).unwrap());
            }
        }
    }

    fn teardown_images(&mut self) {
        // SAFETY: only called with the device idle.
        unsafe {
            for f in self.framebuffers.drain(..) { self.device.destroy_framebuffer(f, None); }
            for v in self.views.drain(..) { self.device.destroy_image_view(v, None); }
            for s in self.render_done.drain(..) { self.device.destroy_semaphore(s, None); }
        }
    }

    fn draw(&mut self, frame: u64, size: (u32, u32)) {
        // SAFETY: one frame of the usual acquire / record / submit / present loop.
        unsafe {
            if size.0 > 0 && (size.0 != self.extent.width || size.1 != self.extent.height) {
                self.device.device_wait_idle().unwrap();
                self.build(size);
            }
            let slot = self.slot;
            self.slot = (slot + 1) % 2;
            self.device.wait_for_fences(&[self.fences[slot]], true, u64::MAX).unwrap();
            let index = match self.swap_fn.acquire_next_image(self.swapchain, u64::MAX, self.acquired[slot], vk::Fence::null()) {
                Ok((index, _)) => index,
                Err(_) => {
                    self.device.device_wait_idle().unwrap();
                    self.build(size);
                    return;
                }
            };
            self.device.reset_fences(&[self.fences[slot]]).unwrap();
            let cmd = self.commands[slot];
            self.device.reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty()).unwrap();
            self.device.begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default()).unwrap();
            let t = frame as f32 / 60.0;
            let clear = [vk::ClearValue { color: vk::ClearColorValue { float32: [0.10, 0.16 + 0.05 * t.sin(), 0.30, 1.0] } }];
            let (w, h) = (self.extent.width, self.extent.height);
            self.device.cmd_begin_render_pass(cmd, &vk::RenderPassBeginInfo::default().render_pass(self.render_pass)
                .framebuffer(self.framebuffers[index as usize])
                .render_area(vk::Rect2D { offset: vk::Offset2D::default(), extent: self.extent }).clear_values(&clear),
                vk::SubpassContents::INLINE);
            // stripes that move, and a bright square that bounces: something with edges for the blur to soften
            let rect = |x: i32, y: i32, rw: u32, rh: u32| vk::ClearRect {
                rect: vk::Rect2D { offset: vk::Offset2D { x: x.max(0), y: y.max(0) },
                                   extent: vk::Extent2D { width: rw.min(w.saturating_sub(x.max(0) as u32)).max(1),
                                                          height: rh.min(h.saturating_sub(y.max(0) as u32)).max(1) } },
                base_array_layer: 0, layer_count: 1,
            };
            let stripes: Vec<_> = (0..w / 64 + 2).map(|i| ((i * 64) as i32 + (frame % 64) as i32) - 64)
                .filter(|x| *x < w as i32).map(|x| rect(x, 0, 24, h)).collect();
            self.device.cmd_clear_attachments(cmd, &[vk::ClearAttachment { aspect_mask: vk::ImageAspectFlags::COLOR,
                color_attachment: 0, clear_value: vk::ClearValue { color: vk::ClearColorValue { float32: [0.85, 0.55, 0.20, 1.0] } } }],
                &stripes);
            let bounce = |span: u32, speed: f32| ((t * speed).sin() * 0.5 + 0.5) * span.saturating_sub(160) as f32;
            self.device.cmd_clear_attachments(cmd, &[vk::ClearAttachment { aspect_mask: vk::ImageAspectFlags::COLOR,
                color_attachment: 0, clear_value: vk::ClearValue { color: vk::ClearColorValue { float32: [0.95, 0.95, 0.95, 1.0] } } }],
                &[rect(bounce(w, 1.3) as i32, bounce(h, 0.9) as i32, 160, 160)]);
            self.device.cmd_end_render_pass(cmd);
            self.device.end_command_buffer(cmd).unwrap();
            let wait = [self.acquired[slot]];
            let signal = [self.render_done[index as usize]];
            self.device.queue_submit(self.queue, &[vk::SubmitInfo::default().wait_semaphores(&wait)
                .wait_dst_stage_mask(&[vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT]).command_buffers(&[cmd])
                .signal_semaphores(&signal)], self.fences[slot]).unwrap();
            let swapchains = [self.swapchain];
            let indices = [index];
            match self.swap_fn.queue_present(self.queue, &vk::PresentInfoKHR::default().wait_semaphores(&signal)
                .swapchains(&swapchains).image_indices(&indices)) {
                Ok(false) => {}
                _ => {
                    self.device.device_wait_idle().unwrap();
                    self.build(size);
                }
            }
        }
    }

    fn destroy(mut self) {
        // SAFETY: tear down in reverse order with the device idle.
        unsafe {
            self.device.device_wait_idle().unwrap();
            self.teardown_images();
            self.swap_fn.destroy_swapchain(self.swapchain, None);
            for s in self.acquired { self.device.destroy_semaphore(s, None); }
            for f in self.fences { self.device.destroy_fence(f, None); }
            self.device.destroy_command_pool(self.pool, None);
            self.device.destroy_render_pass(self.render_pass, None);
            self.device.destroy_device(None);
            self.surface_fn.destroy_surface(self.surface, None);
            self.instance.destroy_instance(None);
        }
    }
}

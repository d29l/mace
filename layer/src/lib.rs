//! Mace's in-game panel, drawn inside Roblox: a Vulkan layer that Mace turns on
//! for the games it starts.
//!
//! - `hooks`: the layer itself. It passes every call down the chain; on the
//!   game's swapchain it adds the panel to frames as they're presented.
//! - `sdl` and `input`: Rosemary reads input through SDL3, so the panel takes
//!   its key, clicks and typing from SDL's event filter, and gives the mouse
//!   back to the game when it closes. No desktop is involved.
//! - `client`: everything shown and done comes from the account's supervisor,
//!   over a Unix socket (`MACE_PANEL`).
//! - `ui`: the side sheet; `render`: blur, dim and the UI's triangles.
//!
//! Nothing here may take the game down: every entry point catches panics, and
//! any failure turns the panel off for the session and passes frames through.

mod client;
mod debug;
pub mod input;
mod hooks;
mod loader;
mod pace;
mod panel;
mod render;
mod sdl;
mod ui;

use std::ffi::c_void;

use ash::vk;

/// One line on the game's stderr, which the supervisor copies into the account's log.
macro_rules! note {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), "mace-panel: {}", format_args!($($arg)*));
    }};
}
pub(crate) use note;

/// Details only when `MACE_PANEL_LOG=1`.
macro_rules! detail {
    ($($arg:tt)*) => {{
        if $crate::panel::verbose() {
            $crate::note!($($arg)*);
        }
    }};
}
pub(crate) use detail;

/// Run `f`; a panic turns the panel off (with the reason in the log) and gives `fallback`.
pub(crate) fn guard<T>(fallback: T, f: impl FnOnce() -> T) -> T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(cause) => {
            let why = cause.downcast_ref::<&str>().map(|s| s.to_string())
                .or_else(|| cause.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "a panic".into());
            panel::disable(&format!("internal error: {why}"));
            fallback
        }
    }
}

/// The loader's way in: agree on interface version 2 and hand over our two
/// proc-address functions. The only symbol this library exports.
///
/// # Safety
/// Called by the Vulkan loader with a valid negotiation struct.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkNegotiateLoaderLayerInterfaceVersion(
    interface: *mut loader::NegotiateLayerInterface,
) -> vk::Result {
    guard(vk::Result::ERROR_INITIALIZATION_FAILED, || {
        // SAFETY: the loader passes a valid, writable struct.
        let interface = unsafe { interface.as_mut() };
        let Some(interface) = interface else { return vk::Result::ERROR_INITIALIZATION_FAILED };
        if interface.s_type != loader::LAYER_NEGOTIATE_INTERFACE_STRUCT || interface.loader_layer_interface_version < 2 {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        }
        interface.loader_layer_interface_version = 2;
        interface.get_instance_proc_addr = Some(hooks::get_instance_proc_addr);
        interface.get_device_proc_addr = Some(hooks::get_device_proc_addr);
        interface.get_physical_device_proc_addr = None;
        vk::Result::SUCCESS
    })
}

/// A function pointer as the loader wants it.
pub(crate) fn void_fn(f: *const c_void) -> vk::PFN_vkVoidFunction {
    // SAFETY: `f` is one of our `extern "system"` hooks or a pointer the next layer gave.
    unsafe { std::mem::transmute::<*const c_void, vk::PFN_vkVoidFunction>(f) }
}

//! The parts of the Vulkan loader's layer interface (`vk_layer.h`) a layer
//! needs, which `ash` doesn't carry.

use std::ffi::{c_char, c_void};

use ash::vk;

pub const LAYER_NEGOTIATE_INTERFACE_STRUCT: u32 = 1;
pub const LAYER_LINK_INFO: u32 = 0;
pub const LOADER_DATA_CALLBACK: u32 = 1;

pub type GetPhysicalDeviceProcAddr = Option<unsafe extern "system" fn(vk::Instance, *const c_char) -> vk::PFN_vkVoidFunction>;
pub type SetDeviceLoaderData = unsafe extern "system" fn(vk::Device, *mut c_void) -> vk::Result;

#[repr(C)]
pub struct NegotiateLayerInterface {
    pub s_type: u32,
    pub p_next: *mut c_void,
    pub loader_layer_interface_version: u32,
    pub get_instance_proc_addr: Option<vk::PFN_vkGetInstanceProcAddr>,
    pub get_device_proc_addr: Option<vk::PFN_vkGetDeviceProcAddr>,
    pub get_physical_device_proc_addr: GetPhysicalDeviceProcAddr,
}

#[repr(C)]
pub struct LayerInstanceLink {
    pub next: *mut LayerInstanceLink,
    pub next_get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    pub next_get_physical_device_proc_addr: GetPhysicalDeviceProcAddr,
}

/// `VkLayerInstanceCreateInfo`; its union is two pointers wide.
#[repr(C)]
pub struct LayerInstanceCreateInfo {
    pub s_type: vk::StructureType,
    pub p_next: *const c_void,
    pub function: u32,
    pub u: [usize; 2],
}

#[repr(C)]
pub struct LayerDeviceLink {
    pub next: *mut LayerDeviceLink,
    pub next_get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    pub next_get_device_proc_addr: vk::PFN_vkGetDeviceProcAddr,
}

#[repr(C)]
pub struct LayerDeviceCreateInfo {
    pub s_type: vk::StructureType,
    pub p_next: *const c_void,
    pub function: u32,
    pub u: usize,
}

/// The loader's element of a create-info chain with this structure type and function.
///
/// # Safety
/// `chain` must be a valid Vulkan `pNext` chain.
pub unsafe fn find<T>(mut chain: *const c_void, s_type: vk::StructureType, function: u32) -> *mut T {
    while !chain.is_null() {
        // SAFETY: every element of a pNext chain starts with sType and pNext.
        let base = unsafe { &*(chain as *const vk::BaseInStructure) };
        if base.s_type == s_type {
            // SAFETY: both loader structures put `function` right after sType and pNext.
            let candidate = unsafe { &*(chain as *const LayerDeviceCreateInfo) };
            if candidate.function == function {
                return chain as *mut T;
            }
        }
        chain = base.p_next as *const c_void;
    }
    std::ptr::null_mut()
}

/// The key the loader's dispatch table pointer gives every dispatchable handle
/// of one instance or device (a queue shares its device's, a physical device
/// its instance's).
///
/// # Safety
/// `handle` must be a live dispatchable handle.
pub unsafe fn key<H: vk::Handle>(handle: H) -> usize {
    // SAFETY: dispatchable handles point at the loader's dispatch table pointer.
    unsafe { *(handle.as_raw() as usize as *const usize) }
}

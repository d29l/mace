//! The layer's hooks. Every one calls down the chain and returns what the next
//! layer returned; the panel's own work happens around those calls, never
//! instead of them.

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_void};
use std::sync::{Arc, Mutex, RwLock};

use ash::vk;
use ash::vk::Handle;

use crate::loader::{self, LayerDeviceCreateInfo, LayerDeviceLink, LayerInstanceCreateInfo, LayerInstanceLink, key};
use crate::render::Swapchain;
use crate::{guard, note, pace, panel, render, void_fn};

pub struct Instance {
    pub handle: vk::Instance,
    pub next_gipa: vk::PFN_vkGetInstanceProcAddr,
    pub fns: ash::Instance,
    pub surface: ash::khr::surface::InstanceFn,
    /// Mace asked for the panel (`MACE_PANEL`); otherwise the layer only passes calls on.
    pub active: bool,
}

pub struct Device {
    pub handle: vk::Device,
    pub physical: vk::PhysicalDevice,
    pub instance: Arc<Instance>,
    pub next_gdpa: vk::PFN_vkGetDeviceProcAddr,
    pub set_loader_data: loader::SetDeviceLoaderData,
    pub fns: ash::Device,
    pub swap: ash::khr::swapchain::DeviceFn,
    pub active: bool,
    pub families: Vec<vk::QueueFamilyProperties>,
    pub memory: vk::PhysicalDeviceMemoryProperties,
    /// queue → its family
    pub queues: Mutex<HashMap<u64, u32>>,
    pub swapchains: Mutex<HashMap<u64, Arc<Mutex<Swapchain>>>>,
    /// Pipelines and textures, made the first time the panel is drawn.
    pub renderer: Mutex<Option<render::Renderer>>,
}

static INSTANCES: RwLock<Option<HashMap<usize, Arc<Instance>>>> = RwLock::new(None);
static DEVICES: RwLock<Option<HashMap<usize, Arc<Device>>>> = RwLock::new(None);

fn instance_of(k: usize) -> Option<Arc<Instance>> {
    INSTANCES.read().ok()?.as_ref()?.get(&k).cloned()
}

fn device_of(k: usize) -> Option<Arc<Device>> {
    DEVICES.read().ok()?.as_ref()?.get(&k).cloned()
}

fn name_of(name: *const c_char) -> &'static str {
    if name.is_null() {
        return "";
    }
    // SAFETY: Vulkan passes NUL-terminated names; we only compare them.
    unsafe { std::mem::transmute::<&str, &'static str>(CStr::from_ptr(name).to_str().unwrap_or("")) }
}

/// Our hook for a device-level function, if we hook it.
fn device_hook(name: &str) -> Option<*const c_void> {
    Some(match name {
        "vkGetDeviceProcAddr" => get_device_proc_addr as *const c_void,
        "vkDestroyDevice" => destroy_device as *const c_void,
        "vkGetDeviceQueue" => get_device_queue as *const c_void,
        "vkGetDeviceQueue2" => get_device_queue2 as *const c_void,
        "vkCreateSwapchainKHR" => create_swapchain as *const c_void,
        "vkDestroySwapchainKHR" => destroy_swapchain as *const c_void,
        "vkQueuePresentKHR" => queue_present as *const c_void,
        _ => return None,
    })
}

pub unsafe extern "system" fn get_instance_proc_addr(instance: vk::Instance, raw: *const c_char) -> vk::PFN_vkVoidFunction {
    let name = name_of(raw);
    let ours = match name {
        "vkGetInstanceProcAddr" => Some(get_instance_proc_addr as *const c_void),
        "vkCreateInstance" => Some(create_instance as *const c_void),
        "vkDestroyInstance" => Some(destroy_instance as *const c_void),
        // always ours: the device chain has to be advanced past this layer
        "vkCreateDevice" => Some(create_device as *const c_void),
        _ => None,
    };
    if let Some(f) = ours {
        return void_fn(f);
    }
    if instance.is_null() {
        return None;
    }
    // SAFETY: a live instance handle from the application.
    let inst = instance_of(unsafe { key(instance) })?;
    if inst.active
        && let Some(f) = device_hook(name)
    {
        return void_fn(f);
    }
    // SAFETY: the next layer's own lookup, with the name as given.
    unsafe { (inst.next_gipa)(instance, raw) }
}

pub unsafe extern "system" fn get_device_proc_addr(device: vk::Device, name: *const c_char) -> vk::PFN_vkVoidFunction {
    let text = name_of(name);
    if device.is_null() {
        return None;
    }
    // SAFETY: a live device handle from the application.
    let dev = device_of(unsafe { key(device) })?;
    let hooked = match text {
        "vkGetDeviceProcAddr" | "vkDestroyDevice" => device_hook(text),
        _ if dev.active => device_hook(text),
        _ => None,
    };
    if let Some(f) = hooked {
        return void_fn(f);
    }
    // SAFETY: the next layer's own lookup, with the name as given.
    unsafe { (dev.next_gdpa)(device, name) }
}

unsafe extern "system" fn create_instance(
    info: *const vk::InstanceCreateInfo<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::Instance,
) -> vk::Result {
    // SAFETY: the loader's create-info chain, as vk_layer.h describes it.
    unsafe {
        let chain: *mut LayerInstanceCreateInfo =
            loader::find((*info).p_next, vk::StructureType::LOADER_INSTANCE_CREATE_INFO, loader::LAYER_LINK_INFO);
        if chain.is_null() {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        }
        let link = (*chain).u[0] as *mut LayerInstanceLink;
        let next_gipa = (*link).next_get_instance_proc_addr;
        (*chain).u[0] = (*link).next as usize;
        let Some(create) = next_gipa(vk::Instance::null(), c"vkCreateInstance".as_ptr()) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let create: vk::PFN_vkCreateInstance = std::mem::transmute(create);
        let result = create(info, allocator, out);
        if result != vk::Result::SUCCESS {
            return result;
        }
        let handle = *out;
        guard((), || {
            let fns = ash::Instance::load_with(|n| std::mem::transmute(next_gipa(handle, n.as_ptr())), handle);
            let surface = ash::khr::surface::InstanceFn::load(|n| std::mem::transmute(next_gipa(handle, n.as_ptr())));
            let active = panel::wanted();
            let instance = Arc::new(Instance { handle, next_gipa, fns, surface, active });
            INSTANCES.write().unwrap().get_or_insert_with(HashMap::new).insert(key(handle), instance);
        });
        result
    }
}

unsafe extern "system" fn destroy_instance(instance: vk::Instance, allocator: *const vk::AllocationCallbacks<'_>) {
    if instance.is_null() {
        return;
    }
    // SAFETY: a live instance; removed from our map before it goes.
    unsafe {
        let k = key(instance);
        let inst = INSTANCES.write().ok().and_then(|mut m| m.as_mut()?.remove(&k));
        if let Some(inst) = inst {
            (inst.fns.fp_v1_0().destroy_instance)(instance, allocator);
        }
    }
}

unsafe extern "system" fn create_device(
    physical: vk::PhysicalDevice,
    info: *const vk::DeviceCreateInfo<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::Device,
) -> vk::Result {
    // SAFETY: the loader's device create-info chain, as vk_layer.h describes it.
    unsafe {
        let chain: *mut LayerDeviceCreateInfo =
            loader::find((*info).p_next, vk::StructureType::LOADER_DEVICE_CREATE_INFO, loader::LAYER_LINK_INFO);
        if chain.is_null() {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        }
        let link = (*chain).u as *mut LayerDeviceLink;
        let next_gipa = (*link).next_get_instance_proc_addr;
        let next_gdpa = (*link).next_get_device_proc_addr;
        (*chain).u = (*link).next as usize;
        let Some(inst) = instance_of(key(physical)) else { return vk::Result::ERROR_INITIALIZATION_FAILED };
        let Some(create) = next_gipa(inst.handle, c"vkCreateDevice".as_ptr()) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let create: vk::PFN_vkCreateDevice = std::mem::transmute(create);
        let result = create(physical, info, allocator, out);
        if result != vk::Result::SUCCESS {
            return result;
        }
        let handle = *out;
        guard((), || {
            let data: *mut LayerDeviceCreateInfo =
                loader::find((*info).p_next, vk::StructureType::LOADER_DEVICE_CREATE_INFO, loader::LOADER_DATA_CALLBACK);
            if data.is_null() {
                note!("the loader gave no device callback; the panel stays off");
                return;
            }
            let set_loader_data: loader::SetDeviceLoaderData = std::mem::transmute((*data).u);
            let load = |n: &CStr| std::mem::transmute::<vk::PFN_vkVoidFunction, *const c_void>(next_gdpa(handle, n.as_ptr()));
            let fns = ash::Device::load_with(load, handle);
            let swap = ash::khr::swapchain::DeviceFn::load(load);
            let has_swapchain = {
                let i = &*info;
                (0..i.enabled_extension_count as usize).any(|n| {
                    CStr::from_ptr(*i.pp_enabled_extension_names.add(n)) == ash::khr::swapchain::NAME
                })
            };
            let families = inst.fns.get_physical_device_queue_family_properties(physical);
            let memory = inst.fns.get_physical_device_memory_properties(physical);
            let device = Arc::new(Device {
                handle, physical, instance: inst.clone(), next_gdpa, set_loader_data, fns, swap,
                active: inst.active && has_swapchain, families, memory,
                queues: Mutex::new(HashMap::new()), swapchains: Mutex::new(HashMap::new()), renderer: Mutex::new(None),
            });
            DEVICES.write().unwrap().get_or_insert_with(HashMap::new).insert(key(handle), device);
        });
        result
    }
}

unsafe extern "system" fn destroy_device(device: vk::Device, allocator: *const vk::AllocationCallbacks<'_>) {
    if device.is_null() {
        return;
    }
    // SAFETY: a live device, removed from our map (and our objects on it freed) before it goes.
    unsafe {
        let k = key(device);
        let Some(dev) = DEVICES.write().ok().and_then(|mut m| m.as_mut()?.remove(&k)) else { return };
        guard((), || {
            let chains: Vec<_> = dev.swapchains.lock().unwrap().drain().map(|(_, s)| s).collect();
            for chain in chains {
                chain.lock().unwrap().destroy(&dev);
            }
            if let Some(renderer) = dev.renderer.lock().unwrap().take() {
                renderer.destroy(&dev);
            }
        });
        (dev.fns.fp_v1_0().destroy_device)(device, allocator);
    }
}

unsafe extern "system" fn get_device_queue(device: vk::Device, family: u32, index: u32, out: *mut vk::Queue) {
    // SAFETY: passed down as given; the queue is recorded afterwards.
    unsafe {
        let Some(dev) = device_of(key(device)) else { return };
        (dev.fns.fp_v1_0().get_device_queue)(device, family, index, out);
        if !(*out).is_null() {
            dev.queues.lock().unwrap().insert((*out).as_raw(), family);
        }
    }
}

unsafe extern "system" fn get_device_queue2(device: vk::Device, info: *const vk::DeviceQueueInfo2<'_>, out: *mut vk::Queue) {
    // SAFETY: as above.
    unsafe {
        let Some(dev) = device_of(key(device)) else { return };
        (dev.fns.fp_v1_1().get_device_queue2)(device, info, out);
        if !(*out).is_null() {
            dev.queues.lock().unwrap().insert((*out).as_raw(), (*info).queue_family_index);
        }
    }
}

unsafe extern "system" fn create_swapchain(
    device: vk::Device,
    info: *const vk::SwapchainCreateInfoKHR<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::SwapchainKHR,
) -> vk::Result {
    // SAFETY: the create info is copied, only its usage widened, and passed down.
    unsafe {
        let Some(dev) = device_of(key(device)) else { return vk::Result::ERROR_INITIALIZATION_FAILED };
        let mut ours = *info;
        let qualifies = guard(false, || render::qualifies(&dev, &mut ours));
        let result = (dev.swap.create_swapchain_khr)(device, if qualifies { &ours } else { info }, allocator, out);
        if result == vk::Result::SUCCESS && qualifies {
            let handle = *out;
            guard((), || {
                let chain = Swapchain::new(&dev, handle, &ours);
                dev.swapchains.lock().unwrap().insert(handle.as_raw(), Arc::new(Mutex::new(chain)));
                panel::activate();
            });
        }
        result
    }
}

unsafe extern "system" fn destroy_swapchain(
    device: vk::Device,
    swapchain: vk::SwapchainKHR,
    allocator: *const vk::AllocationCallbacks<'_>,
) {
    // SAFETY: our objects for this swapchain go first, then the swapchain.
    unsafe {
        let Some(dev) = device_of(key(device)) else { return };
        let chain = dev.swapchains.lock().unwrap().remove(&swapchain.as_raw());
        if let Some(chain) = chain {
            guard((), || chain.lock().unwrap().destroy(&dev));
        }
        (dev.swap.destroy_swapchain_khr)(device, swapchain, allocator);
    }
}

unsafe extern "system" fn queue_present(queue: vk::Queue, info: *const vk::PresentInfoKHR<'_>) -> vk::Result {
    // SAFETY: the present info is passed down as given, or as a copy that waits on
    // our semaphore instead (which our submission signals after waiting on the game's).
    unsafe {
        let Some(dev) = device_of(key(queue)) else { return vk::Result::ERROR_DEVICE_LOST };
        if dev.active {
            // a game in the background waits here for its next frame slot
            pace::before_present(panel::INPUT.captured.load(std::sync::atomic::Ordering::Relaxed));
        }
        if !dev.active || !panel::visible() {
            return (dev.swap.queue_present_khr)(queue, info);
        }
        // set as soon as our submission is in; kept even if something after it fails,
        // because that submission has already waited on the game's semaphores
        let mut ours = None;
        guard((), || render::present(&dev, queue, &*info, &mut ours));
        match ours {
            Some(semaphore) => {
                let mut changed = *info;
                let waits = [semaphore];
                changed.wait_semaphore_count = 1;
                changed.p_wait_semaphores = waits.as_ptr();
                (dev.swap.queue_present_khr)(queue, &changed)
            }
            None => (dev.swap.queue_present_khr)(queue, info),
        }
    }
}

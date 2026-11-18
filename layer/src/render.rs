//! Drawing the panel onto the game's frames.
//!
//! While the panel is open (or sliding), each present of the game's swapchain
//! gets one submission of ours: it waits on the game's semaphores, blurs a small
//! copy of the frame behind the panel, draws the UI on top, and signals a
//! semaphore the present then waits on. While it's closed nothing is recorded:
//! the present hook only checks one flag. Everything per image (view,
//! framebuffer, commands) is made the first time that image is drawn on.

use std::collections::HashMap;
use std::ffi::CStr;
use std::sync::atomic::{AtomicBool, Ordering};

use ash::vk;
use ash::vk::Handle;

use crate::hooks::Device;
use crate::panel::{self, Frame};
use crate::{debug, detail, note};

static SPIRV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/panel.spv"));
const FENCE_WAIT: u64 = 1_000_000_000;

/// Whether the panel can draw on a swapchain being made with `info`; if so,
/// the image usage it needs is added (when the surface allows it).
pub fn qualifies(dev: &Device, info: &mut vk::SwapchainCreateInfoKHR<'_>) -> bool {
    if !dev.active || !panel::can_draw() {
        return false;
    }
    let format_ok = matches!(info.image_format,
        vk::Format::B8G8R8A8_UNORM | vk::Format::B8G8R8A8_SRGB | vk::Format::R8G8B8A8_UNORM | vk::Format::R8G8B8A8_SRGB
        | vk::Format::A2B10G10R10_UNORM_PACK32 | vk::Format::A2R10G10B10_UNORM_PACK32);
    let shared = matches!(info.present_mode,
        vk::PresentModeKHR::SHARED_DEMAND_REFRESH | vk::PresentModeKHR::SHARED_CONTINUOUS_REFRESH);
    if !format_ok || info.image_color_space != vk::ColorSpaceKHR::SRGB_NONLINEAR || info.image_array_layers != 1 || shared {
        note!("not drawing on a {:?} / {:?} / {:?} swapchain", info.image_format, info.image_color_space, info.present_mode);
        return false;
    }
    let mut caps = vk::SurfaceCapabilitiesKHR::default();
    // SAFETY: the next layer's query on the surface the game is making a swapchain for.
    let got = unsafe {
        (dev.instance.surface.get_physical_device_surface_capabilities_khr)(dev.physical, info.surface, &mut caps)
    };
    if got != vk::Result::SUCCESS {
        return false;
    }
    let wanted = vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC;
    info.image_usage |= wanted & caps.supported_usage_flags;
    info.image_usage.contains(vk::ImageUsageFlags::COLOR_ATTACHMENT)
}

fn is_srgb(format: vk::Format) -> bool {
    matches!(format, vk::Format::B8G8R8A8_SRGB | vk::Format::R8G8B8A8_SRGB)
}

struct Buffer {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    size: u64,
    ptr: *mut u8,
}

// SAFETY: the mapping is only written through the swapchain's mutex.
unsafe impl Send for Buffer {}
// SAFETY: as above.
unsafe impl Sync for Buffer {}

impl Buffer {
    fn new(dev: &Device, size: u64, usage: vk::BufferUsageFlags) -> Result<Buffer, vk::Result> {
        // SAFETY: a host-visible buffer, mapped for its whole life.
        unsafe {
            let buffer = dev.fns.create_buffer(&vk::BufferCreateInfo::default().size(size).usage(usage), None)?;
            let needs = dev.fns.get_buffer_memory_requirements(buffer);
            let memory = match allocate(dev, needs, vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT) {
                Ok(m) => m,
                Err(e) => {
                    dev.fns.destroy_buffer(buffer, None);
                    return Err(e);
                }
            };
            dev.fns.bind_buffer_memory(buffer, memory, 0)?;
            let ptr = dev.fns.map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty())? as *mut u8;
            Ok(Buffer { buffer, memory, size, ptr })
        }
    }
    fn destroy(&self, dev: &Device) {
        // SAFETY: only once nothing of ours still uses it.
        unsafe {
            dev.fns.destroy_buffer(self.buffer, None);
            dev.fns.free_memory(self.memory, None);
        }
    }
    fn write(&self, at: usize, bytes: &[u8]) {
        assert!(at + bytes.len() <= self.size as usize);
        // SAFETY: inside the mapped range (checked above).
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.ptr.add(at), bytes.len()) };
    }
}

fn allocate(dev: &Device, needs: vk::MemoryRequirements, flags: vk::MemoryPropertyFlags) -> Result<vk::DeviceMemory, vk::Result> {
    let types = &dev.memory.memory_types[..dev.memory.memory_type_count as usize];
    let index = types.iter().enumerate()
        .find(|(i, t)| needs.memory_type_bits & (1 << i) != 0 && t.property_flags.contains(flags))
        .map(|(i, _)| i as u32)
        .ok_or(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY)?;
    // SAFETY: a plain allocation of a type the requirements allow.
    unsafe { dev.fns.allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(needs.size).memory_type_index(index), None) }
}

/// What one swapchain image needs to be drawn on.
struct Slot {
    view: vk::ImageView,
    framebuffer: vk::Framebuffer,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    /// Signalled by our submission, waited on by the present.
    done: vk::Semaphore,
    submitted: bool,
    vertices: Option<Buffer>,
    indices: Option<Buffer>,
    /// Texture uploads in flight with this image's last submission.
    staging: Vec<Buffer>,
}

/// The blur's image: the frame at half size, then each level half the one
/// before. Every level can be drawn into and sampled from.
struct Blur {
    image: vk::Image,
    memory: vk::DeviceMemory,
    sizes: Vec<vk::Extent2D>,
    views: Vec<vk::ImageView>,
    sets: Vec<vk::DescriptorSet>,
    framebuffers: Vec<vk::Framebuffer>,
}

impl Blur {
    fn destroy(self, dev: &Device, descriptors: Option<vk::DescriptorPool>) {
        // SAFETY: no submission of ours uses it any more.
        unsafe {
            if let Some(pool) = descriptors {
                let _ = dev.fns.free_descriptor_sets(pool, &self.sets);
            }
            for f in self.framebuffers {
                dev.fns.destroy_framebuffer(f, None);
            }
            for v in self.views {
                dev.fns.destroy_image_view(v, None);
            }
            dev.fns.destroy_image(self.image, None);
            dev.fns.free_memory(self.memory, None);
        }
    }
}

pub struct Swapchain {
    format: vk::Format,
    extent: vk::Extent2D,
    usage: vk::ImageUsageFlags,
    images: Vec<vk::Image>,
    slots: Vec<Option<Slot>>,
    pool: Option<(u32, vk::CommandPool)>,
    blur: Option<Blur>,
    /// No blur on this swapchain (format or usage don't allow the copies): dim only.
    no_blur: bool,
}

impl Swapchain {
    pub fn new(dev: &Device, handle: vk::SwapchainKHR, info: &vk::SwapchainCreateInfoKHR<'_>) -> Swapchain {
        // SAFETY: the usual two-call query on the swapchain just made.
        let images = unsafe {
            let mut count = 0;
            let _ = (dev.swap.get_swapchain_images_khr)(dev.handle, handle, &mut count, std::ptr::null_mut());
            let mut images = vec![vk::Image::null(); count as usize];
            let _ = (dev.swap.get_swapchain_images_khr)(dev.handle, handle, &mut count, images.as_mut_ptr());
            images.truncate(count as usize);
            images
        };
        note!("drawing on the game's swapchain: {:?} {}x{}, {} images, {:?}, usage {:?}, video {}",
              info.image_format, info.image_extent.width, info.image_extent.height, images.len(), info.present_mode,
              info.image_usage, panel::video_driver());
        let blur_ok = info.image_usage.contains(vk::ImageUsageFlags::TRANSFER_SRC) && {
            // SAFETY: a plain format query.
            let props = unsafe {
                dev.instance.fns.get_physical_device_format_properties(dev.physical, info.image_format)
            };
            let needed = vk::FormatFeatureFlags::BLIT_SRC | vk::FormatFeatureFlags::BLIT_DST
                | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR | vk::FormatFeatureFlags::COLOR_ATTACHMENT;
            props.optimal_tiling_features.contains(needed)
        };
        Swapchain {
            format: info.image_format, extent: info.image_extent, usage: info.image_usage,
            slots: images.iter().map(|_| None).collect(), images, pool: None, blur: None, no_blur: !blur_ok,
        }
    }

    fn wait_all(&self, dev: &Device) {
        let fences: Vec<_> = self.slots.iter().flatten().filter(|s| s.submitted).map(|s| s.fence).collect();
        if !fences.is_empty() {
            // SAFETY: our own fences.
            let _ = unsafe { dev.fns.wait_for_fences(&fences, true, FENCE_WAIT) };
        }
    }

    fn drop_slots(&mut self, dev: &Device) {
        self.wait_all(dev);
        for slot in self.slots.iter_mut().filter_map(Option::take) {
            // SAFETY: nothing of ours uses these any more (waited above).
            unsafe {
                dev.fns.destroy_framebuffer(slot.framebuffer, None);
                dev.fns.destroy_image_view(slot.view, None);
                dev.fns.destroy_fence(slot.fence, None);
                // the semaphore may still be waited on by a present that hasn't run;
                // the spec lets a semaphore go once its wait has been submitted, and
                // the present that waits on it was submitted before this call
                dev.fns.destroy_semaphore(slot.done, None);
            }
            for b in slot.vertices.iter().chain(slot.indices.iter()).chain(slot.staging.iter()) {
                b.destroy(dev);
            }
        }
        if let Some((_, pool)) = self.pool.take() {
            // SAFETY: its command buffers are no longer pending (waited above).
            unsafe { dev.fns.destroy_command_pool(pool, None) };
        }
    }

    pub fn destroy(&mut self, dev: &Device) {
        self.drop_slots(dev);
        if let Some(blur) = self.blur.take() {
            let pool = dev.renderer.lock().unwrap().as_ref().map(|r| r.descriptors);
            blur.destroy(dev, pool);
        }
    }

    fn slot(&mut self, dev: &Device, pass: vk::RenderPass, family: u32, index: usize) -> Result<&mut Slot, vk::Result> {
        if self.pool.is_some_and(|(f, _)| f != family) {
            // presents moved to another queue family: start over there
            self.drop_slots(dev);
        }
        // SAFETY: plain object creation on the game's device; each command buffer
        // gets the loader's dispatch pointer, as layers must do.
        unsafe {
            if self.pool.is_none() {
                let pool = dev.fns.create_command_pool(&vk::CommandPoolCreateInfo::default()
                    .queue_family_index(family).flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER), None)?;
                self.pool = Some((family, pool));
            }
            if self.slots[index].is_none() {
                let view = dev.fns.create_image_view(&vk::ImageViewCreateInfo::default().image(self.images[index])
                    .view_type(vk::ImageViewType::TYPE_2D).format(self.format)
                    .subresource_range(color_range(0, 1)), None)?;
                let framebuffer = dev.fns.create_framebuffer(&vk::FramebufferCreateInfo::default().render_pass(pass)
                    .attachments(&[view]).width(self.extent.width).height(self.extent.height).layers(1), None)?;
                let cmd = dev.fns.allocate_command_buffers(&vk::CommandBufferAllocateInfo::default()
                    .command_pool(self.pool.unwrap().1).command_buffer_count(1))?[0];
                let r = (dev.set_loader_data)(dev.handle, cmd.as_raw() as *mut _);
                if r != vk::Result::SUCCESS {
                    return Err(r);
                }
                let fence = dev.fns.create_fence(&vk::FenceCreateInfo::default(), None)?;
                let done = dev.fns.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?;
                self.slots[index] = Some(Slot {
                    view, framebuffer, cmd, fence, done, submitted: false, vertices: None, indices: None, staging: Vec::new(),
                });
            }
        }
        Ok(self.slots[index].as_mut().unwrap())
    }
}

fn color_range(base: u32, levels: u32) -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR)
        .base_mip_level(base).level_count(levels).layer_count(1)
}

struct Texture {
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    set: vk::DescriptorSet,
    size: [u32; 2],
}

struct Pass {
    render_pass: vk::RenderPass,
    ui: vk::Pipeline,
    blur: vk::Pipeline,
    /// Draws one level of the blur's image (overwritten whole, then sampled).
    level_pass: vk::RenderPass,
    down: vk::Pipeline,
    up: vk::Pipeline,
}

/// The panel's pipelines and textures on one device.
pub struct Renderer {
    shader: vk::ShaderModule,
    sampler: vk::Sampler,
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    descriptors: vk::DescriptorPool,
    passes: HashMap<i32, Pass>,
    textures: HashMap<egui::TextureId, Texture>,
    /// Textures egui is done with, freed once our submissions that used them are.
    retired: Vec<Texture>,
}

impl Renderer {
    fn new(dev: &Device) -> Result<Renderer, vk::Result> {
        // SAFETY: plain object creation on the game's device.
        unsafe {
            let code = ash::util::read_spv(&mut std::io::Cursor::new(SPIRV)).map_err(|_| vk::Result::ERROR_INVALID_SHADER_NV)?;
            let shader = dev.fns.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&code), None)?;
            let sampler = dev.fns.create_sampler(&vk::SamplerCreateInfo::default()
                .mag_filter(vk::Filter::LINEAR).min_filter(vk::Filter::LINEAR)
                .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE).address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE).max_lod(0.0), None)?;
            let set_layout = dev.fns.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&[
                vk::DescriptorSetLayoutBinding::default().binding(0).descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .descriptor_count(1).stage_flags(vk::ShaderStageFlags::FRAGMENT),
                vk::DescriptorSetLayoutBinding::default().binding(1).descriptor_type(vk::DescriptorType::SAMPLER)
                    .descriptor_count(1).stage_flags(vk::ShaderStageFlags::FRAGMENT),
            ]), None)?;
            let layout = dev.fns.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&[set_layout]), None)?;
            let descriptors = dev.fns.create_descriptor_pool(&vk::DescriptorPoolCreateInfo::default()
                .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET).max_sets(256)
                .pool_sizes(&[vk::DescriptorPoolSize { ty: vk::DescriptorType::SAMPLED_IMAGE, descriptor_count: 256 },
                              vk::DescriptorPoolSize { ty: vk::DescriptorType::SAMPLER, descriptor_count: 256 }]), None)?;
            Ok(Renderer { shader, sampler, set_layout, layout, descriptors, passes: HashMap::new(),
                          textures: HashMap::new(), retired: Vec::new() })
        }
    }

    pub fn destroy(mut self, dev: &Device) {
        // SAFETY: the device is going away; nothing of ours is pending (swapchains went first).
        unsafe {
            for t in self.textures.drain().map(|(_, t)| t).chain(self.retired.drain(..)) {
                dev.fns.destroy_image_view(t.view, None);
                dev.fns.destroy_image(t.image, None);
                dev.fns.free_memory(t.memory, None);
            }
            for (_, p) in self.passes.drain() {
                for pipeline in [p.ui, p.blur, p.down, p.up] {
                    dev.fns.destroy_pipeline(pipeline, None);
                }
                dev.fns.destroy_render_pass(p.render_pass, None);
                dev.fns.destroy_render_pass(p.level_pass, None);
            }
            dev.fns.destroy_descriptor_pool(self.descriptors, None);
            dev.fns.destroy_pipeline_layout(self.layout, None);
            dev.fns.destroy_descriptor_set_layout(self.set_layout, None);
            dev.fns.destroy_sampler(self.sampler, None);
            dev.fns.destroy_shader_module(self.shader, None);
        }
    }

    fn pass(&mut self, dev: &Device, format: vk::Format) -> Result<&Pass, vk::Result> {
        if !self.passes.contains_key(&format.as_raw()) {
            let pass = self.make_pass(dev, format)?;
            self.passes.insert(format.as_raw(), pass);
        }
        Ok(&self.passes[&format.as_raw()])
    }

    fn make_pass(&self, dev: &Device, format: vk::Format) -> Result<Pass, vk::Result> {
        // SAFETY: plain object creation; the attachment keeps what the game drew (LOAD)
        // and leaves the image ready to present.
        unsafe {
            let render_pass = dev.fns.create_render_pass(&vk::RenderPassCreateInfo::default()
                .attachments(&[vk::AttachmentDescription::default().format(format).samples(vk::SampleCountFlags::TYPE_1)
                    .load_op(vk::AttachmentLoadOp::LOAD).store_op(vk::AttachmentStoreOp::STORE)
                    .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE).stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
                    .initial_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL).final_layout(vk::ImageLayout::PRESENT_SRC_KHR)])
                .subpasses(&[vk::SubpassDescription::default().pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
                    .color_attachments(&[vk::AttachmentReference::default().attachment(0)
                        .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)])]), None)?;
            let fragment = if is_srgb(format) { c"fs_ui_linear" } else { c"fs_ui_gamma" };
            let premultiplied = vk::PipelineColorBlendAttachmentState::default().blend_enable(true)
                .src_color_blend_factor(vk::BlendFactor::ONE).dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                .color_blend_op(vk::BlendOp::ADD)
                .src_alpha_blend_factor(vk::BlendFactor::ZERO).dst_alpha_blend_factor(vk::BlendFactor::ONE)
                .alpha_blend_op(vk::BlendOp::ADD)
                // never the alpha channel: the window's own transparency stays the game's
                .color_write_mask(vk::ColorComponentFlags::R | vk::ColorComponentFlags::G | vk::ColorComponentFlags::B);
            let straight = premultiplied.src_color_blend_factor(vk::BlendFactor::SRC_ALPHA);
            // a level of the blur: whatever was there is replaced, and it's left ready to be sampled;
            // it waits for the reads of the level before it, and the next pass waits for its writes
            let level_pass = dev.fns.create_render_pass(&vk::RenderPassCreateInfo::default()
                .attachments(&[vk::AttachmentDescription::default().format(format).samples(vk::SampleCountFlags::TYPE_1)
                    .load_op(vk::AttachmentLoadOp::DONT_CARE).store_op(vk::AttachmentStoreOp::STORE)
                    .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE).stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
                    .initial_layout(vk::ImageLayout::UNDEFINED).final_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)])
                .subpasses(&[vk::SubpassDescription::default().pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
                    .color_attachments(&[vk::AttachmentReference::default().attachment(0)
                        .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)])])
                .dependencies(&[
                    vk::SubpassDependency::default().src_subpass(vk::SUBPASS_EXTERNAL).dst_subpass(0)
                        .src_stage_mask(vk::PipelineStageFlags::FRAGMENT_SHADER | vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                                        | vk::PipelineStageFlags::TRANSFER)
                        .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
                        .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE | vk::AccessFlags::TRANSFER_WRITE)
                        .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE),
                    vk::SubpassDependency::default().src_subpass(0).dst_subpass(vk::SUBPASS_EXTERNAL)
                        .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
                        .dst_stage_mask(vk::PipelineStageFlags::FRAGMENT_SHADER)
                        .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                        .dst_access_mask(vk::AccessFlags::SHADER_READ),
                ]), None)?;
            let replace = vk::PipelineColorBlendAttachmentState::default().color_write_mask(vk::ColorComponentFlags::RGBA);
            let made = [
                self.pipeline(dev, render_pass, fragment, premultiplied),
                self.pipeline(dev, render_pass, c"fs_blur", straight),
                self.pipeline(dev, level_pass, c"fs_down", replace),
                self.pipeline(dev, level_pass, c"fs_up", replace),
            ];
            if let [Ok(ui), Ok(blur), Ok(down), Ok(up)] = made {
                return Ok(Pass { render_pass, ui, blur, level_pass, down, up });
            }
            for p in made.into_iter().flatten() {
                dev.fns.destroy_pipeline(p, None);
            }
            dev.fns.destroy_render_pass(render_pass, None);
            dev.fns.destroy_render_pass(level_pass, None);
            Err(vk::Result::ERROR_INITIALIZATION_FAILED)
        }
    }

    fn pipeline(&self, dev: &Device, render_pass: vk::RenderPass, fragment: &CStr,
                blend: vk::PipelineColorBlendAttachmentState) -> Result<vk::Pipeline, vk::Result> {
        let stages = [
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::VERTEX).module(self.shader).name(c"vs_main"),
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::FRAGMENT).module(self.shader).name(fragment),
        ];
        let bindings = [vk::VertexInputBindingDescription { binding: 0, stride: 20, input_rate: vk::VertexInputRate::VERTEX }];
        let attributes = [
            vk::VertexInputAttributeDescription { location: 0, binding: 0, format: vk::Format::R32G32_SFLOAT, offset: 0 },
            vk::VertexInputAttributeDescription { location: 1, binding: 0, format: vk::Format::R32G32_SFLOAT, offset: 8 },
            vk::VertexInputAttributeDescription { location: 2, binding: 0, format: vk::Format::R8G8B8A8_UNORM, offset: 16 },
        ];
        let blends = [blend];
        let dynamic = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&bindings).vertex_attribute_descriptions(&attributes);
        let assembly = vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport = vk::PipelineViewportStateCreateInfo::default().viewport_count(1).scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL).cull_mode(vk::CullModeFlags::NONE).line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let blend_state = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blends);
        let dynamic_state = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic);
        let info = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&assembly)
            .viewport_state(&viewport)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .color_blend_state(&blend_state)
            .dynamic_state(&dynamic_state)
            .layout(self.layout)
            .render_pass(render_pass);
        // SAFETY: a complete pipeline description.
        unsafe {
            dev.fns.create_graphics_pipelines(vk::PipelineCache::null(), &[info], None)
                .map(|p| p[0]).map_err(|(_, e)| e)
        }
    }

    fn descriptor(&self, dev: &Device, view: vk::ImageView) -> Result<vk::DescriptorSet, vk::Result> {
        // SAFETY: one set from our pool, pointed at `view` and our sampler.
        unsafe {
            let set = dev.fns.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(self.descriptors).set_layouts(&[self.set_layout]))?[0];
            let image = [vk::DescriptorImageInfo::default().image_view(view).image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
            let sampler = [vk::DescriptorImageInfo::default().sampler(self.sampler)];
            dev.fns.update_descriptor_sets(&[
                vk::WriteDescriptorSet::default().dst_set(set).dst_binding(0).descriptor_type(vk::DescriptorType::SAMPLED_IMAGE).image_info(&image),
                vk::WriteDescriptorSet::default().dst_set(set).dst_binding(1).descriptor_type(vk::DescriptorType::SAMPLER).image_info(&sampler),
            ], &[]);
            Ok(set)
        }
    }

    fn make_texture(&self, dev: &Device, size: [u32; 2]) -> Result<Texture, vk::Result> {
        // SAFETY: a device-local sampled image with its view and descriptor.
        unsafe {
            let image = dev.fns.create_image(&vk::ImageCreateInfo::default().image_type(vk::ImageType::TYPE_2D)
                .format(vk::Format::R8G8B8A8_UNORM).extent(vk::Extent3D { width: size[0], height: size[1], depth: 1 })
                .mip_levels(1).array_layers(1).samples(vk::SampleCountFlags::TYPE_1).tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST), None)?;
            let memory = allocate(dev, dev.fns.get_image_memory_requirements(image), vk::MemoryPropertyFlags::DEVICE_LOCAL)?;
            dev.fns.bind_image_memory(image, memory, 0)?;
            let view = dev.fns.create_image_view(&vk::ImageViewCreateInfo::default().image(image)
                .view_type(vk::ImageViewType::TYPE_2D).format(vk::Format::R8G8B8A8_UNORM).subresource_range(color_range(0, 1)), None)?;
            let set = self.descriptor(dev, view)?;
            Ok(Texture { image, memory, view, set, size })
        }
    }

    fn free_texture(&self, dev: &Device, t: Texture) {
        // SAFETY: called only once our submissions that used it are done.
        unsafe {
            let _ = dev.fns.free_descriptor_sets(self.descriptors, &[t.set]);
            dev.fns.destroy_image_view(t.view, None);
            dev.fns.destroy_image(t.image, None);
            dev.fns.free_memory(t.memory, None);
        }
    }
}

fn barrier(dev: &Device, cmd: vk::CommandBuffer, image: vk::Image, range: vk::ImageSubresourceRange,
           from: (vk::ImageLayout, vk::PipelineStageFlags, vk::AccessFlags),
           to: (vk::ImageLayout, vk::PipelineStageFlags, vk::AccessFlags)) {
    // SAFETY: recording into our own command buffer.
    unsafe {
        dev.fns.cmd_pipeline_barrier(cmd, from.1, to.1, vk::DependencyFlags::empty(), &[], &[], &[
            vk::ImageMemoryBarrier::default().image(image).subresource_range(range)
                .old_layout(from.0).new_layout(to.0).src_access_mask(from.2).dst_access_mask(to.2)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED).dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED),
        ]);
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Vertex {
    pos: [f32; 2],
    uv: [f32; 2],
    color: [u8; 4],
}

fn as_bytes<T: Copy>(items: &[T]) -> &[u8] {
    // SAFETY: plain-old-data with no padding (Vertex is 20 packed bytes; u32 indices; Color32 is 4 bytes).
    unsafe { std::slice::from_raw_parts(items.as_ptr().cast(), std::mem::size_of_val(items)) }
}

static NO_GRAPHICS_NOTED: AtomicBool = AtomicBool::new(false);

/// Draw the panel for this present. Once our submission is in, `submitted`
/// holds the semaphore the present must wait on instead of the game's (whose
/// semaphores that submission has consumed): it is set before anything else
/// can go wrong, so the hook uses it even if what follows fails.
pub fn present(dev: &Device, queue: vk::Queue, info: &vk::PresentInfoKHR<'_>, submitted: &mut Option<vk::Semaphore>) {
    // SAFETY: arrays of the given counts, as the present info promises.
    let (swapchains, indices) = unsafe {
        (std::slice::from_raw_parts(info.p_swapchains, info.swapchain_count as usize),
         std::slice::from_raw_parts(info.p_image_indices, info.swapchain_count as usize))
    };
    let found = swapchains.iter().enumerate()
        .find_map(|(i, s)| dev.swapchains.lock().unwrap().get(&s.as_raw()).cloned().map(|c| (i, c)));
    let Some((at, chain)) = found else { return };
    let Some(family) = dev.queues.lock().unwrap().get(&queue.as_raw()).copied() else { return };
    if !dev.families.get(family as usize).is_some_and(|f| f.queue_flags.contains(vk::QueueFlags::GRAPHICS)) {
        if !NO_GRAPHICS_NOTED.swap(true, Ordering::Relaxed) {
            note!("the game presents from a queue that can't draw; the panel stays off");
        }
        return;
    }
    let mut chain = chain.lock().unwrap();
    let Some(frame) = panel::frame(chain.extent) else { return };
    let mut renderer = dev.renderer.lock().unwrap();
    if renderer.is_none() {
        match Renderer::new(dev) {
            Ok(r) => *renderer = Some(r),
            Err(e) => {
                panel::disable(&format!("couldn't set up drawing: {e:?}"));
                return;
            }
        }
    }
    let renderer = renderer.as_mut().unwrap();
    if let Err(e) = draw(dev, renderer, &mut chain, queue, family, indices[at] as usize, info, frame, submitted) {
        panel::disable(&format!("drawing failed: {e:?}"));
    }
}

/// One draw call: a mesh's indices, its texture, its clip rectangle.
struct Draw {
    first: u32,
    count: u32,
    base: i32,
    set: vk::DescriptorSet,
    scissor: vk::Rect2D,
}

/// Add an egui mesh to the vertex and index lists: points → pixels → clip space.
fn add_mesh(vertices: &mut Vec<Vertex>, indices: &mut Vec<u32>, mesh: &egui::Mesh, clip: egui::Rect, ppp: f32,
            extent: vk::Extent2D, set: vk::DescriptorSet) -> Option<Draw> {
    let (w, h) = (extent.width as f32, extent.height as f32);
    let x0 = (clip.min.x * ppp).round().clamp(0.0, w);
    let y0 = (clip.min.y * ppp).round().clamp(0.0, h);
    let x1 = (clip.max.x * ppp).round().clamp(x0, w);
    let y1 = (clip.max.y * ppp).round().clamp(y0, h);
    if x1 - x0 < 1.0 || y1 - y0 < 1.0 || mesh.indices.is_empty() {
        return None;
    }
    let base = vertices.len() as i32;
    let first = indices.len() as u32;
    vertices.extend(mesh.vertices.iter().map(|v| Vertex {
        pos: [v.pos.x * ppp / w * 2.0 - 1.0, v.pos.y * ppp / h * 2.0 - 1.0],
        uv: [v.uv.x, v.uv.y],
        color: v.color.to_array(),
    }));
    indices.extend_from_slice(&mesh.indices);
    Some(Draw {
        first, count: mesh.indices.len() as u32, base, set,
        scissor: vk::Rect2D { offset: vk::Offset2D { x: x0 as i32, y: y0 as i32 },
                              extent: vk::Extent2D { width: (x1 - x0) as u32, height: (y1 - y0) as u32 } },
    })
}

fn grow(dev: &Device, buffer: &mut Option<Buffer>, needed: u64, usage: vk::BufferUsageFlags) -> Result<(), vk::Result> {
    if buffer.as_ref().is_some_and(|b| b.size >= needed) {
        return Ok(());
    }
    if let Some(old) = buffer.take() {
        old.destroy(dev);
    }
    *buffer = Some(Buffer::new(dev, needed.next_power_of_two().max(64 * 1024), usage)?);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn blit(dev: &Device, cmd: vk::CommandBuffer, src: vk::Image, src_level: u32, src_size: vk::Extent2D,
        dst: vk::Image, dst_level: u32, dst_size: vk::Extent2D) {
    let layers = |level| vk::ImageSubresourceLayers::default().aspect_mask(vk::ImageAspectFlags::COLOR)
        .mip_level(level).layer_count(1);
    let corner = |e: vk::Extent2D| vk::Offset3D { x: e.width as i32, y: e.height as i32, z: 1 };
    // SAFETY: recording into our own command buffer, images in the layouts named.
    unsafe {
        dev.fns.cmd_blit_image(cmd, src, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, dst, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[
            vk::ImageBlit::default()
                .src_subresource(layers(src_level)).src_offsets([vk::Offset3D::default(), corner(src_size)])
                .dst_subresource(layers(dst_level)).dst_offsets([vk::Offset3D::default(), corner(dst_size)]),
        ], vk::Filter::LINEAR);
    }
}

/// Levels of the blur: half size, then halved again and again. Down to a
/// sixteenth of the frame and back up: a wide, smooth blur for little work.
const BLUR_LEVELS: u32 = 4;

/// The blur's image, in the swapchain's format, with a view, a framebuffer and
/// a descriptor (for sampling) per level.
fn make_blur(dev: &Device, renderer: &Renderer, level_pass: vk::RenderPass, format: vk::Format,
             extent: vk::Extent2D) -> Result<Blur, vk::Result> {
    let size = vk::Extent2D { width: (extent.width / 2).max(1), height: (extent.height / 2).max(1) };
    let levels = BLUR_LEVELS.min(32 - size.width.min(size.height).leading_zeros()).max(1);
    // SAFETY: plain object creation on the game's device; on failure what was made is freed.
    unsafe {
        let image = dev.fns.create_image(&vk::ImageCreateInfo::default().image_type(vk::ImageType::TYPE_2D)
            .format(format).extent(vk::Extent3D { width: size.width, height: size.height, depth: 1 })
            .mip_levels(levels).array_layers(1).samples(vk::SampleCountFlags::TYPE_1).tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::COLOR_ATTACHMENT),
            None)?;
        let memory = match allocate(dev, dev.fns.get_image_memory_requirements(image), vk::MemoryPropertyFlags::DEVICE_LOCAL) {
            Ok(m) => m,
            Err(e) => {
                dev.fns.destroy_image(image, None);
                return Err(e);
            }
        };
        let mut blur = Blur { image, memory, sizes: Vec::new(), views: Vec::new(), sets: Vec::new(), framebuffers: Vec::new() };
        let made = (|| -> Result<(), vk::Result> {
            dev.fns.bind_image_memory(image, memory, 0)?;
            for level in 0..levels {
                let s = vk::Extent2D { width: (size.width >> level).max(1), height: (size.height >> level).max(1) };
                let view = dev.fns.create_image_view(&vk::ImageViewCreateInfo::default().image(image)
                    .view_type(vk::ImageViewType::TYPE_2D).format(format).subresource_range(color_range(level, 1)), None)?;
                blur.views.push(view);
                blur.sets.push(renderer.descriptor(dev, view)?);
                blur.framebuffers.push(dev.fns.create_framebuffer(&vk::FramebufferCreateInfo::default()
                    .render_pass(level_pass).attachments(&[view]).width(s.width).height(s.height).layers(1), None)?);
                blur.sizes.push(s);
            }
            Ok(())
        })();
        match made {
            Ok(()) => Ok(blur),
            Err(e) => {
                blur.destroy(dev, Some(renderer.descriptors));
                Err(e)
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw(dev: &Device, renderer: &mut Renderer, chain: &mut Swapchain, queue: vk::Queue, family: u32, index: usize,
        info: &vk::PresentInfoKHR<'_>, frame: Frame, submitted: &mut Option<vk::Semaphore>) -> Result<(), vk::Result> {
    let (format, extent, image) = (chain.format, chain.extent, chain.images[index]);
    let can_copy = chain.usage.contains(vk::ImageUsageFlags::TRANSFER_SRC);
    let pass = renderer.pass(dev, format)?;
    let (render_pass, ui_pipeline, blur_pipeline) = (pass.render_pass, pass.ui, pass.blur);
    let (level_pass, down_pipeline, up_pipeline) = (pass.level_pass, pass.down, pass.up);
    if chain.blur.is_none() && !chain.no_blur && frame.blur > 0.0 {
        match make_blur(dev, renderer, level_pass, format, extent) {
            Ok(b) => chain.blur = Some(b),
            Err(e) => {
                detail!("no blur: {e:?}");
                chain.no_blur = true;
            }
        }
    }
    let blur = chain.blur.as_ref().filter(|_| frame.blur > 0.0)
        .map(|b| (b.image, b.sizes.clone(), b.sets.clone(), b.framebuffers.clone()));
    {
        let slot = chain.slot(dev, render_pass, family, index)?;
        // SAFETY: our own fence; the slot's last submission must be done before it's reused.
        unsafe {
            if slot.submitted {
                dev.fns.wait_for_fences(&[slot.fence], true, FENCE_WAIT)?;
                dev.fns.reset_fences(&[slot.fence])?;
                slot.submitted = false;
            }
        }
        for b in slot.staging.drain(..) {
            b.destroy(dev);
        }
    }
    if !renderer.retired.is_empty() {
        chain.wait_all(dev);
        for t in std::mem::take(&mut renderer.retired) {
            renderer.free_texture(dev, t);
        }
    }
    let slot = chain.slots[index].as_mut().unwrap();
    let cmd = slot.cmd;

    // SAFETY: recording and submitting our own command buffer; everything in it
    // touches only our objects and the swapchain image being presented.
    unsafe {
        dev.fns.reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty())?;
        dev.fns.begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT))?;

        // this frame's texture uploads (the font atlas, avatars)
        let mut staging = Vec::new();
        let deltas = frame.textures.0.set.iter().flat_map(|(id, list)| list.iter().map(move |d| (id, d)));
        for (id, delta) in deltas {
            let egui::ImageData::Color(img) = &delta.image;
            let size = [img.size[0] as u32, img.size[1] as u32];
            let fresh = match (delta.pos, renderer.textures.get(id)) {
                (None, Some(t)) if t.size == size => false,
                (None, _) => {
                    let t = renderer.make_texture(dev, size)?;
                    if let Some(old) = renderer.textures.insert(*id, t) {
                        renderer.retired.push(old);
                    }
                    true
                }
                (Some(_), Some(_)) => false,
                (Some(_), None) => continue,
            };
            let t = &renderer.textures[id];
            let pixels = as_bytes(&img.pixels);
            let buffer = Buffer::new(dev, pixels.len().max(4) as u64, vk::BufferUsageFlags::TRANSFER_SRC)?;
            buffer.write(0, pixels);
            let from = if fresh {
                (vk::ImageLayout::UNDEFINED, vk::PipelineStageFlags::TOP_OF_PIPE, vk::AccessFlags::empty())
            } else {
                (vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL, vk::PipelineStageFlags::FRAGMENT_SHADER, vk::AccessFlags::empty())
            };
            barrier(dev, cmd, t.image, color_range(0, 1), from,
                    (vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::PipelineStageFlags::TRANSFER, vk::AccessFlags::TRANSFER_WRITE));
            let [x, y] = delta.pos.unwrap_or([0, 0]);
            dev.fns.cmd_copy_buffer_to_image(cmd, buffer.buffer, t.image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[
                vk::BufferImageCopy::default()
                    .image_subresource(vk::ImageSubresourceLayers::default().aspect_mask(vk::ImageAspectFlags::COLOR).layer_count(1))
                    .image_offset(vk::Offset3D { x: x as i32, y: y as i32, z: 0 })
                    .image_extent(vk::Extent3D { width: size[0], height: size[1], depth: 1 }),
            ]);
            barrier(dev, cmd, t.image, color_range(0, 1),
                    (vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::PipelineStageFlags::TRANSFER, vk::AccessFlags::TRANSFER_WRITE),
                    (vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL, vk::PipelineStageFlags::FRAGMENT_SHADER, vk::AccessFlags::SHADER_READ));
            staging.push(buffer);
        }

        // the vertices: the blur's one big triangle, then egui's meshes
        let fade = (frame.blur.clamp(0.0, 1.0) * 255.0) as u8;
        let mut vertices = vec![
            Vertex { pos: [-1.0, -1.0], uv: [0.0, 0.0], color: [255, 255, 255, fade] },
            Vertex { pos: [3.0, -1.0], uv: [2.0, 0.0], color: [255, 255, 255, fade] },
            Vertex { pos: [-1.0, 3.0], uv: [0.0, 2.0], color: [255, 255, 255, fade] },
        ];
        let mut indices: Vec<u32> = Vec::new();
        let mut draws = Vec::new();
        for clipped in &frame.primitives {
            let egui::epaint::Primitive::Mesh(mesh) = &clipped.primitive else { continue };
            let Some(t) = renderer.textures.get(&mesh.texture_id) else { continue };
            if let Some(d) = add_mesh(&mut vertices, &mut indices, mesh, clipped.clip_rect, frame.ppp, extent, t.set) {
                draws.push(d);
            }
        }
        let (vbytes, ibytes) = (as_bytes(&vertices), as_bytes(&indices));
        grow(dev, &mut slot.vertices, vbytes.len() as u64, vk::BufferUsageFlags::VERTEX_BUFFER)?;
        grow(dev, &mut slot.indices, ibytes.len().max(4) as u64, vk::BufferUsageFlags::INDEX_BUFFER)?;
        slot.vertices.as_ref().unwrap().write(0, vbytes);
        slot.indices.as_ref().unwrap().write(0, ibytes);

        let whole = color_range(0, 1);
        let game_wrote = (vk::ImageLayout::PRESENT_SRC_KHR, vk::PipelineStageFlags::ALL_COMMANDS, vk::AccessFlags::MEMORY_WRITE);
        let attachment = (vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL, vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                          vk::AccessFlags::COLOR_ATTACHMENT_READ | vk::AccessFlags::COLOR_ATTACHMENT_WRITE);
        dev.fns.cmd_bind_vertex_buffers(cmd, 0, &[slot.vertices.as_ref().unwrap().buffer], &[0]);
        dev.fns.cmd_bind_index_buffer(cmd, slot.indices.as_ref().unwrap().buffer, 0, vk::IndexType::UINT32);
        let viewport = |e: vk::Extent2D| vk::Viewport {
            x: 0.0, y: 0.0, width: e.width as f32, height: e.height as f32, min_depth: 0.0, max_depth: 1.0,
        };
        if let Some((blur_image, sizes, sets, framebuffers)) = &blur {
            // the frame at half size, then the dual filter: down to the smallest level and back up,
            // each pass one fullscreen triangle sampling the level before
            barrier(dev, cmd, *blur_image, color_range(0, 1),
                    (vk::ImageLayout::UNDEFINED, vk::PipelineStageFlags::FRAGMENT_SHADER, vk::AccessFlags::empty()),
                    (vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::PipelineStageFlags::TRANSFER, vk::AccessFlags::TRANSFER_WRITE));
            barrier(dev, cmd, image, whole, game_wrote,
                    (vk::ImageLayout::TRANSFER_SRC_OPTIMAL, vk::PipelineStageFlags::TRANSFER, vk::AccessFlags::TRANSFER_READ));
            blit(dev, cmd, image, 0, extent, *blur_image, 0, sizes[0]);
            barrier(dev, cmd, *blur_image, color_range(0, 1),
                    (vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::PipelineStageFlags::TRANSFER, vk::AccessFlags::TRANSFER_WRITE),
                    (vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL, vk::PipelineStageFlags::FRAGMENT_SHADER, vk::AccessFlags::SHADER_READ));
            barrier(dev, cmd, image, whole,
                    (vk::ImageLayout::TRANSFER_SRC_OPTIMAL, vk::PipelineStageFlags::TRANSFER, vk::AccessFlags::empty()), attachment);
            let levels = sizes.len();
            let steps = (1..levels).map(|l| (down_pipeline, l - 1, l))
                .chain((0..levels.saturating_sub(1)).rev().map(|l| (up_pipeline, l + 1, l)));
            for (pipeline, from, to) in steps {
                dev.fns.cmd_begin_render_pass(cmd, &vk::RenderPassBeginInfo::default().render_pass(level_pass)
                    .framebuffer(framebuffers[to]).render_area(vk::Rect2D { offset: vk::Offset2D::default(), extent: sizes[to] }),
                    vk::SubpassContents::INLINE);
                dev.fns.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipeline);
                dev.fns.cmd_set_viewport(cmd, 0, &[viewport(sizes[to])]);
                dev.fns.cmd_set_scissor(cmd, 0, &[vk::Rect2D { offset: vk::Offset2D::default(), extent: sizes[to] }]);
                dev.fns.cmd_bind_descriptor_sets(cmd, vk::PipelineBindPoint::GRAPHICS, renderer.layout, 0, &[sets[from]], &[]);
                dev.fns.cmd_draw(cmd, 3, 1, 0, 0);
                dev.fns.cmd_end_render_pass(cmd);
            }
        } else {
            barrier(dev, cmd, image, whole, game_wrote, attachment);
        }

        dev.fns.cmd_begin_render_pass(cmd, &vk::RenderPassBeginInfo::default().render_pass(render_pass)
            .framebuffer(slot.framebuffer).render_area(vk::Rect2D { offset: vk::Offset2D::default(), extent }),
            vk::SubpassContents::INLINE);
        dev.fns.cmd_set_viewport(cmd, 0, &[viewport(extent)]);
        if let Some((_, _, sets, _)) = &blur {
            dev.fns.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, blur_pipeline);
            dev.fns.cmd_bind_descriptor_sets(cmd, vk::PipelineBindPoint::GRAPHICS, renderer.layout, 0, &[sets[0]], &[]);
            dev.fns.cmd_set_scissor(cmd, 0, &[vk::Rect2D { offset: vk::Offset2D::default(), extent }]);
            dev.fns.cmd_draw(cmd, 3, 1, 0, 0);
        }
        dev.fns.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, ui_pipeline);
        for d in &draws {
            dev.fns.cmd_bind_descriptor_sets(cmd, vk::PipelineBindPoint::GRAPHICS, renderer.layout, 0, &[d.set], &[]);
            dev.fns.cmd_set_scissor(cmd, 0, &[d.scissor]);
            dev.fns.cmd_draw_indexed(cmd, d.count, 1, d.first, d.base, 0);
        }
        dev.fns.cmd_end_render_pass(cmd);

        // a test asked for this frame as a picture
        let dump = match (&frame.dump, can_copy) {
            (Some(name), true) => {
                let buffer = Buffer::new(dev, extent.width as u64 * extent.height as u64 * 4, vk::BufferUsageFlags::TRANSFER_DST)?;
                barrier(dev, cmd, image, whole,
                        (vk::ImageLayout::PRESENT_SRC_KHR, vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                         vk::AccessFlags::COLOR_ATTACHMENT_WRITE),
                        (vk::ImageLayout::TRANSFER_SRC_OPTIMAL, vk::PipelineStageFlags::TRANSFER, vk::AccessFlags::TRANSFER_READ));
                dev.fns.cmd_copy_image_to_buffer(cmd, image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, buffer.buffer, &[
                    vk::BufferImageCopy::default()
                        .image_subresource(vk::ImageSubresourceLayers::default().aspect_mask(vk::ImageAspectFlags::COLOR).layer_count(1))
                        .image_extent(vk::Extent3D { width: extent.width, height: extent.height, depth: 1 }),
                ]);
                barrier(dev, cmd, image, whole,
                        (vk::ImageLayout::TRANSFER_SRC_OPTIMAL, vk::PipelineStageFlags::TRANSFER, vk::AccessFlags::empty()),
                        (vk::ImageLayout::PRESENT_SRC_KHR, vk::PipelineStageFlags::BOTTOM_OF_PIPE, vk::AccessFlags::empty()));
                Some((buffer, name.clone()))
            }
            (Some(_), false) => {
                note!("can't save a picture: the swapchain can't be copied from");
                None
            }
            _ => None,
        };
        dev.fns.end_command_buffer(cmd)?;

        let waits: &[vk::Semaphore] = if info.wait_semaphore_count == 0 { &[] } else {
            std::slice::from_raw_parts(info.p_wait_semaphores, info.wait_semaphore_count as usize)
        };
        let stages = vec![vk::PipelineStageFlags::ALL_COMMANDS; waits.len()];
        let signal = [slot.done];
        let cmds = [cmd];
        dev.fns.queue_submit(queue, &[vk::SubmitInfo::default().wait_semaphores(waits).wait_dst_stage_mask(&stages)
            .command_buffers(&cmds).signal_semaphores(&signal)], slot.fence)?;
        // from here the present must wait on ours: the game's semaphores are spent
        *submitted = Some(slot.done);
        slot.submitted = true;
        slot.staging = staging;
        for id in &frame.textures.0.free {
            if let Some(t) = renderer.textures.remove(id) {
                renderer.retired.push(t);
            }
        }
        if let Some((buffer, name)) = dump {
            if dev.fns.wait_for_fences(&[slot.fence], true, FENCE_WAIT).is_ok() {
                let bytes = std::slice::from_raw_parts(buffer.ptr, buffer.size as usize).to_vec();
                debug::save(&name, extent.width, extent.height, format, bytes);
            }
            buffer.destroy(dev);
        }
        Ok(())
    }
}

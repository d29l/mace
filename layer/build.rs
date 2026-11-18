//! Compiles the panel's shaders to SPIR-V, and links the layer so that the
//! Vulkan loader can never unload it from under the hooks it leaves in the game.

fn main() {
    println!("cargo:rerun-if-changed=shaders/panel.wgsl");
    let source = std::fs::read_to_string("shaders/panel.wgsl").expect("shaders/panel.wgsl");
    let module = naga::front::wgsl::parse_str(&source).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
    let info = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
        .validate(&module)
        .unwrap_or_else(|e| panic!("{e:?}"));
    let mut options = naga::back::spv::Options::default();
    // positions are written in Vulkan's clip space already
    options.flags.remove(naga::back::spv::WriterFlags::ADJUST_COORDINATE_SPACE);
    let words = naga::back::spv::write_vec(&module, &info, &options, None).expect("SPIR-V");
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("panel.spv");
    std::fs::write(out, bytes).unwrap();

    // The loader dlcloses layers in vkDestroyInstance; the SDL event filter and
    // the socket thread outlive that, so the library has to stay mapped. And
    // every symbol is bound when it loads: a missing one then means the loader
    // skips the layer, never a crash in the game later.
    println!("cargo:rustc-cdylib-link-arg=-Wl,-z,nodelete");
    println!("cargo:rustc-cdylib-link-arg=-Wl,-z,now");
}

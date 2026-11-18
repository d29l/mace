//! For the panel's tests only, each behind its own environment variable:
//! saving frames as pictures (`MACE_PANEL_DUMP=<dir>`) and a clock that moves
//! one sixtieth of a second per frame (`MACE_PANEL_TEST_CLOCK=1`), so pictures
//! come out the same every run.

use std::sync::Mutex;

use ash::vk;

static DUMP: Mutex<Option<String>> = Mutex::new(None);

fn dump_dir() -> Option<String> {
    std::env::var("MACE_PANEL_DUMP").ok().filter(|d| !d.is_empty())
}

/// A picture of the next frame was asked for (by the test's script).
pub fn request(name: String) {
    if dump_dir().is_some() {
        let clean: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
        *DUMP.lock().unwrap() = Some(clean);
    }
}

pub fn pending() -> bool {
    DUMP.lock().map(|d| d.is_some()).unwrap_or(false)
}

pub fn take() -> Option<String> {
    DUMP.lock().ok()?.take()
}

pub fn test_clock() -> bool {
    std::env::var("MACE_PANEL_TEST_CLOCK").is_ok_and(|v| v == "1")
}

/// Write a frame (raw pixels in the swapchain's format) as `<dir>/<name>.png`.
pub fn save(name: &str, width: u32, height: u32, format: vk::Format, raw: Vec<u8>) {
    let Some(dir) = dump_dir() else { return };
    let path = std::path::Path::new(&dir).join(format!("{name}.png"));
    let mut rgba = Vec::with_capacity(raw.len());
    for px in raw.as_chunks::<4>().0 {
        let v = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
        let [r, g, b] = match format {
            vk::Format::B8G8R8A8_UNORM | vk::Format::B8G8R8A8_SRGB => [px[2], px[1], px[0]],
            vk::Format::A2B10G10R10_UNORM_PACK32 => [(v >> 2) as u8, (v >> 12) as u8, (v >> 22) as u8],
            vk::Format::A2R10G10B10_UNORM_PACK32 => [(v >> 22) as u8, (v >> 12) as u8, (v >> 2) as u8],
            _ => [px[0], px[1], px[2]],
        };
        rgba.extend_from_slice(&[r, g, b, 255]);
    }
    let _ = std::thread::spawn(move || {
        let Ok(file) = std::fs::File::create(&path) else { return };
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        if let Ok(mut writer) = encoder.write_header() {
            let _ = writer.write_image_data(&rgba);
        }
        crate::note!("saved {}", path.display());
    })
    .join();
}

/// `MACE_PANEL_FAKE=<file>`: Mace's side played from a file of its messages
/// (one per line; `NOW` stands for twelve minutes ago); what the panel sends
/// goes to `<file>.sent`.
pub fn fake() -> Option<String> {
    std::env::var("MACE_PANEL_FAKE").ok().filter(|f| !f.is_empty())
}

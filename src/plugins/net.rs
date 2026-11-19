//! A plugin's web requests and pictures from the web, each on a thread of its
//! own so the host never waits on the network. Only https, only to the sites
//! the user agreed to, and no redirects (which could lead anywhere else).

use std::io::Read;
use std::time::Duration;

use gtk::gdk_pixbuf::{self, prelude::*};
use serde_json::{Value, json};

/// A response's body, at most.
pub const LARGEST: u64 = 2 << 20;
/// Pictures from the web are drawn from this many pixels square.
pub const PICTURE_SIZE: u32 = 192;

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .max_redirects(0)
        .user_agent(crate::builds::USER_AGENT)
        .build()
        .into()
}

/// The host of an https URL, or why it can't be used.
pub fn host_of(url: &str) -> Result<String, String> {
    let rest = url.strip_prefix("https://").ok_or("only https:// addresses")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return Err("no user names in addresses".into());
    }
    let host = authority.split(':').next().unwrap_or("").to_ascii_lowercase();
    if host.is_empty() {
        return Err("no site in the address".into());
    }
    Ok(host)
}

pub struct Request {
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

/// Make the request: `{ ok, status, body, headers }`, or `{ ok = false, error }`.
pub fn fetch(request: &Request) -> Value {
    // tests reach no network: this address answers with what was asked
    #[cfg(test)]
    if host_of(&request.url).is_ok_and(|h| h == "mace.test" || h.ends_with(".mace.test")) {
        return json!({ "ok": true, "status": 200, "body": format!("{} {}", request.method, request.url), "headers": {} });
    }
    let agent = agent();
    let result = match request.method.as_str() {
        "GET" | "DELETE" | "HEAD" => {
            let mut call = match request.method.as_str() {
                "GET" => agent.get(&request.url),
                "DELETE" => agent.delete(&request.url),
                _ => agent.head(&request.url),
            };
            for (name, value) in &request.headers {
                call = call.header(name, value);
            }
            call.call()
        }
        _ => {
            let mut call = match request.method.as_str() {
                "PUT" => agent.put(&request.url),
                "PATCH" => agent.patch(&request.url),
                _ => agent.post(&request.url),
            };
            for (name, value) in &request.headers {
                call = call.header(name, value);
            }
            call.send(request.body.clone().unwrap_or_default())
        }
    };
    let mut response = match result {
        Ok(response) => response,
        Err(error) => return json!({ "ok": false, "status": 0, "error": error.to_string() }),
    };
    let status = response.status().as_u16();
    let headers: serde_json::Map<String, Value> = response.headers().iter()
        .filter_map(|(k, v)| Some((k.as_str().to_string(), Value::String(v.to_str().ok()?.to_string()))))
        .collect();
    let mut body = Vec::new();
    if response.body_mut().as_reader().take(LARGEST + 1).read_to_end(&mut body).is_err() {
        return json!({ "ok": false, "status": status, "error": "the answer broke off" });
    }
    if body.len() as u64 > LARGEST {
        return json!({ "ok": false, "status": status, "error": "the answer is over 2 MB" });
    }
    json!({
        "ok": (200..300).contains(&status),
        "status": status,
        "body": String::from_utf8_lossy(&body),
        "headers": headers,
    })
}

/// A picture from the web, square (cropped to its middle), as RGBA.
pub fn picture(url: &str) -> Option<(u32, Vec<u8>)> {
    let mut response = agent().get(url).call().ok()?;
    if !response.status().is_success() {
        return None;
    }
    let mut bytes = Vec::new();
    response.body_mut().as_reader().take(LARGEST).read_to_end(&mut bytes).ok()?;
    decode(&bytes, PICTURE_SIZE)
}

pub fn decode(bytes: &[u8], size: u32) -> Option<(u32, Vec<u8>)> {
    let loader = gdk_pixbuf::PixbufLoader::new();
    loader.write(bytes).ok()?;
    loader.close().ok()?;
    let pixbuf = loader.pixbuf()?;
    let (w, h) = (pixbuf.width(), pixbuf.height());
    let side = w.min(h);
    if side <= 0 {
        return None;
    }
    let square = pixbuf.new_subpixbuf((w - side) / 2, (h - side) / 2, side, side);
    let scaled = square.scale_simple(size as i32, size as i32, gdk_pixbuf::InterpType::Bilinear)?;
    let scaled = if scaled.has_alpha() { scaled } else { scaled.add_alpha(false, 0, 0, 0).ok()? };
    let stride = scaled.rowstride() as usize;
    let pixels = scaled.read_pixel_bytes();
    let row = size as usize * 4;
    let mut rgba = Vec::with_capacity(row * size as usize);
    for y in 0..size as usize {
        rgba.extend_from_slice(&pixels[y * stride..y * stride + row]);
    }
    Some((size, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_sites() {
        assert_eq!(host_of("https://LRCLIB.net/api/get?x=1"), Ok("lrclib.net".into()));
        assert_eq!(host_of("https://i.scdn.co:443/image/ab"), Ok("i.scdn.co".into()));
        assert!(host_of("http://lrclib.net/").is_err());
        assert!(host_of("https://evil.com@lrclib.net/").is_err());
        assert!(host_of("https:///x").is_err());
        let sites = vec!["lrclib.net".to_string(), "*.scdn.co".to_string()];
        assert!(super::super::site_allowed(&sites, "lrclib.net"));
        assert!(super::super::site_allowed(&sites, "i.scdn.co"));
        assert!(!super::super::site_allowed(&sites, "scdn.co"));
        assert!(!super::super::site_allowed(&sites, "notscdn.co"));
        assert!(!super::super::site_allowed(&sites, "lrclib.net.evil.com"));
    }

    #[test]
    fn pictures_are_made_square() {
        // a 4x2 PNG: the middle 2x2 is kept
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 4, 2);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.write_header().unwrap().write_image_data(&[255u8, 0, 0].repeat(8)).unwrap();
        }
        let (size, rgba) = decode(&png, 8).unwrap();
        assert_eq!((size, rgba.len()), (8, 8 * 8 * 4));
        assert_eq!(&rgba[..4], &[255, 0, 0, 255]);
    }
}

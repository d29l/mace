// The panel's shaders. Every draw uses the same vertices (position already in
// Vulkan's clip space, texture coordinate, colour) and one texture.

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(@location(0) pos: vec2<f32>, @location(1) uv: vec2<f32>, @location(2) color: vec4<f32>) -> VertexOut {
    var out: VertexOut;
    out.position = vec4<f32>(pos, 0.0, 1.0);
    out.uv = uv;
    out.color = color;
    return out;
}

@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;

fn linear_from_gamma(c: vec3<f32>) -> vec3<f32> {
    let lower = c / vec3<f32>(12.92);
    let higher = pow((c + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return select(higher, lower, c < vec3<f32>(0.04045));
}

// The UI on a plain (UNORM) swapchain: colours and textures are premultiplied
// sRGB, blended as they are.
@fragment
fn fs_ui_gamma(in: VertexOut) -> @location(0) vec4<f32> {
    return in.color * textureSample(image, image_sampler, in.uv);
}

// The UI on an sRGB swapchain, which expects linear colour.
@fragment
fn fs_ui_linear(in: VertexOut) -> @location(0) vec4<f32> {
    let c = in.color * textureSample(image, image_sampler, in.uv);
    if (c.a <= 0.0) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(linear_from_gamma(c.rgb / c.a) * c.a, c.a);
}

// The blur, as a dual filter (the kind KWin uses): the frame is drawn into
// smaller and smaller levels (fs_down) and back up (fs_up), each pass a few
// bilinear samples around the pixel. Wide and smooth, for little work.

fn sample(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(image, image_sampler, uv, 0.0);
}

@fragment
fn fs_down(in: VertexOut) -> @location(0) vec4<f32> {
    // half a source texel times the spread, so each tap falls between four texels
    let h = vec2<f32>(1.0) / vec2<f32>(textureDimensions(image)) * 1.0;
    var sum = sample(in.uv) * 4.0;
    sum += sample(in.uv - h);
    sum += sample(in.uv + h);
    sum += sample(in.uv + vec2<f32>(h.x, -h.y));
    sum += sample(in.uv - vec2<f32>(h.x, -h.y));
    return sum / 8.0;
}

@fragment
fn fs_up(in: VertexOut) -> @location(0) vec4<f32> {
    let h = vec2<f32>(0.5) / vec2<f32>(textureDimensions(image)) * 1.5;
    var sum = sample(in.uv + vec2<f32>(-h.x * 2.0, 0.0));
    sum += sample(in.uv + vec2<f32>(-h.x, h.y)) * 2.0;
    sum += sample(in.uv + vec2<f32>(0.0, h.y * 2.0));
    sum += sample(in.uv + vec2<f32>(h.x, h.y)) * 2.0;
    sum += sample(in.uv + vec2<f32>(h.x * 2.0, 0.0));
    sum += sample(in.uv + vec2<f32>(h.x, -h.y)) * 2.0;
    sum += sample(in.uv + vec2<f32>(0.0, -h.y * 2.0));
    sum += sample(in.uv + vec2<f32>(-h.x, -h.y)) * 2.0;
    return sum / 12.0;
}

// The blurred frame (half size, smoothly scaled) over the game; the vertex
// colour's alpha says how far the panel is in.
@fragment
fn fs_blur(in: VertexOut) -> @location(0) vec4<f32> {
    return vec4<f32>(sample(in.uv).rgb, in.color.a);
}

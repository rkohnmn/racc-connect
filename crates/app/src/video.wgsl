struct VideoUniforms {
    panel_size: vec2<f32>,
    frame_number: u32,
    seed: u32,
    content_origin: vec2<f32>,
    content_size: vec2<f32>,
    _padding: vec4<u32>,
}
@group(0) @binding(0) var<uniform> video: VideoUniforms;
struct VertexOutput { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    var coords = array<vec2<f32>, 3>(vec2<f32>(0.0, 1.0), vec2<f32>(2.0, 1.0), vec2<f32>(0.0, -1.0));
    var output: VertexOutput; output.position = vec4<f32>(positions[index], 0.0, 1.0); output.uv = coords[index]; return output;
}
@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let pixel = input.uv * video.panel_size;
    let local = pixel - video.content_origin;
    let size = max(video.content_size, vec2<f32>(1.0, 1.0));
    let inside = all(local >= vec2<f32>(0.0, 0.0)) && all(local < size);
    if !inside { return vec4<f32>(0.025, 0.030, 0.045, 1.0); }
    let uv = local / size;
    let phase = f32(video.frame_number % 120u) / 120.0;
    let moving_bar = abs(uv.x - phase) < 0.035;
    let grid = fract(uv.x * 12.0) < 0.015 || fract(uv.y * 8.0) < 0.015;
    let tint = f32(video.seed % 17u) / 200.0;
    var color = vec3<f32>(0.10 + tint, 0.17 + uv.y * 0.18, 0.34 + uv.x * 0.16);
    if grid { color *= 0.72; }
    if moving_bar { color = vec3<f32>(0.54, 0.43, 0.98); }
    return vec4<f32>(color, 1.0);
}

struct VideoUniforms {
    panel_size: vec2<f32>,
    frame_number: u32,
    seed: u32,
    content_origin: vec2<f32>,
    content_size: vec2<f32>,
    source_size: vec2<u32>,
    frame_kind: u32,
    _padding0: u32,
    _padding1: vec2<u32>,
    cursor_rect: vec4<f32>,
    cursor_meta: vec4<u32>,
    cursor_blend_mode: u32,
    cursor_visible: u32,
    _cursor_padding: vec2<u32>,
}
@group(0) @binding(0) var<uniform> video: VideoUniforms;
@group(0) @binding(1) var y_plane: texture_2d<f32>;
@group(0) @binding(2) var uv_plane: texture_2d<f32>;
@group(0) @binding(3) var cursor_plane: texture_2d<f32>;
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
    var color: vec3<f32>;
    if video.frame_kind == 1u {
        let max_pixel = video.source_size - vec2<u32>(1u, 1u);
        let source_pixel = min(vec2<u32>(uv * vec2<f32>(video.source_size)), max_pixel);
        let luma_sample = textureLoad(y_plane, vec2<i32>(source_pixel), 0).r;
        let chroma_sample = textureLoad(uv_plane, vec2<i32>(source_pixel / vec2<u32>(2u, 2u)), 0).rg;
        let y = (luma_sample * 255.0 - 16.0) / 219.0;
        let cb = (chroma_sample.r * 255.0 - 128.0) / 224.0;
        let cr = (chroma_sample.g * 255.0 - 128.0) / 224.0;
        color = clamp(vec3<f32>(
            y + 1.5748 * cr,
            y - 0.1873 * cb - 0.4681 * cr,
            y + 1.8556 * cb
        ), vec3<f32>(0.0), vec3<f32>(1.0));
    } else if video.frame_kind == 2u {
        let phase = f32(video.frame_number % 120u) / 120.0;
        let moving_bar = abs(uv.x - phase) < 0.035;
        let grid = fract(uv.x * 12.0) < 0.015 || fract(uv.y * 8.0) < 0.015;
        let tint = f32(video.seed % 17u) / 200.0;
        color = vec3<f32>(0.10 + tint, 0.17 + uv.y * 0.18, 0.34 + uv.x * 0.16);
        if grid { color *= 0.72; }
        if moving_bar { color = vec3<f32>(0.54, 0.43, 0.98); }
    } else {
        color = vec3<f32>(0.025, 0.030, 0.045);
    }

    if video.cursor_visible != 0u {
        let cursor_origin = video.cursor_rect.xy;
        let cursor_size = video.cursor_rect.zw;
        let cursor_end = cursor_origin + cursor_size;
        if all(pixel >= cursor_origin) && all(pixel < cursor_end) {
            let shape_size = max(video.cursor_meta.xy, vec2<u32>(1u, 1u));
            let shape_position = vec2<u32>(floor((pixel - cursor_origin) / cursor_size * vec2<f32>(shape_size)));
            let shape_pixel = min(shape_position, shape_size - vec2<u32>(1u, 1u));
            let source = textureLoad(cursor_plane, vec2<i32>(shape_pixel), 0);
            if video.cursor_blend_mode == 0u {
                // Wire pixels are premultiplied RGBA: src + dst * (1 - alpha).
                color = source.rgb + color * (1.0 - source.a);
            } else {
                let destination_u8 = vec3<u32>(round(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)) * 255.0));
                let source_u8 = vec3<u32>(round(source.rgb * 255.0));
                if video.cursor_blend_mode == 1u {
                    if source.a == 0.0 {
                        color = vec3<f32>(source_u8) / 255.0;
                    } else {
                        color = vec3<f32>(destination_u8 ^ source_u8) / 255.0;
                    }
                } else if video.cursor_blend_mode == 2u {
                    let and_mask = u32(round(source.a * 255.0));
                    let result_u8 = (destination_u8 & vec3<u32>(and_mask)) ^ source_u8;
                    color = vec3<f32>(result_u8) / 255.0;
                }
            }
        }
    }
    return vec4<f32>(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}

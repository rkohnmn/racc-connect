struct VideoUniforms {
    panel_size: vec2<f32>,
    _padding: vec2<f32>,
}

@group(0) @binding(0) var y_plane: texture_2d<f32>;
@group(0) @binding(1) var uv_plane: texture_2d<f32>;
@group(0) @binding(2) var frame_sampler: sampler;
@group(0) @binding(3) var<uniform> video: VideoUniforms;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var coordinates = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(2.0, 1.0),
        vec2<f32>(0.0, -1.0),
    );
    var output: VertexOutput;
    output.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    output.uv = coordinates[vertex_index];
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let panel_aspect = video.panel_size.x / max(video.panel_size.y, 1.0);
    let source_aspect = 1920.0 / 1080.0;
    var uv = input.uv;
    var inside = true;

    if panel_aspect > source_aspect {
        let visible_width = source_aspect / panel_aspect;
        let margin = (1.0 - visible_width) * 0.5;
        inside = uv.x >= margin && uv.x <= 1.0 - margin;
        uv.x = (uv.x - margin) / visible_width;
    } else {
        let visible_height = panel_aspect / source_aspect;
        let margin = (1.0 - visible_height) * 0.5;
        inside = uv.y >= margin && uv.y <= 1.0 - margin;
        uv.y = (uv.y - margin) / visible_height;
    }

    if !inside {
        return vec4<f32>(0.025, 0.030, 0.045, 1.0);
    }

    let y = textureSample(y_plane, frame_sampler, uv).r - 0.0625;
    let chroma = textureSample(uv_plane, frame_sampler, uv).rg - vec2<f32>(0.5, 0.5);
    let y_scaled = 1.1643 * y;
    let red = y_scaled + 1.5960 * chroma.y;
    let green = y_scaled - 0.3918 * chroma.x - 0.8130 * chroma.y;
    let blue = y_scaled + 2.0170 * chroma.x;
    return vec4<f32>(clamp(vec3<f32>(red, green, blue), vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}

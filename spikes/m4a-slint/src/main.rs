use std::{
    cell::RefCell,
    env,
    rc::Rc,
    time::{Duration, Instant},
};

use slint::wgpu_30::wgpu;
use slint::{Timer, TimerMode};

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const CHROMA_WIDTH: u32 = WIDTH / 2;
const CHROMA_HEIGHT: u32 = HEIGHT / 2;
// Nominal 30 fps source cadence: 33.333 ms between synthetic NV12 updates.
const VIDEO_PERIOD: Duration = Duration::from_micros(33_333);
// Nominal 4 Hz telemetry cadence.
const TELEMETRY_PERIOD: Duration = Duration::from_millis(250);
const DEFAULT_DURATION_SECONDS: u64 = 60;

slint::slint! {
    export component SpikeWindow inherits Window {
        title: "M4a Slint WGPU spike";
        preferred-width: 1440px;
        preferred-height: 900px;
        min-width: 1000px;
        min-height: 640px;
        background: #1a1b20;
        in-out property <image> video-image;
        in-out property <string> telemetry-label: "Telemetry waiting for first sample";
        in-out property <string> frame-label: "Synthetic stream initializing";

        HorizontalLayout {
            spacing: 0px;

            Rectangle {
                width: 72px;
                background: #111217;
                VerticalLayout {
                    padding: 12px;
                    spacing: 12px;
                    Text { text: "RC"; color: #b4a5ff; font-size: 22px; horizontal-alignment: center; }
                    Rectangle { height: 1px; background: #34353d; }
                    Text { text: "⌂"; color: #f2f2f5; font-size: 24px; horizontal-alignment: center; }
                    Rectangle { height: 34px; background: #34353d; border-radius: 8px; }
                    Rectangle { height: 34px; background: #6356a8; border-radius: 8px; }
                    Rectangle { height: 34px; background: #34353d; border-radius: 8px; }
                    Rectangle { vertical-stretch: 1; }
                    Text { text: "LOCAL"; color: #92939d; font-size: 10px; horizontal-alignment: center; }
                }
            }

            Rectangle {
                width: 236px;
                background: #202127;
                border-color: #303139;
                border-width: 1px;
                VerticalLayout {
                    padding: 16px;
                    spacing: 10px;
                    Text { text: "WIN10-GAMING-PC"; color: #f2f2f5; font-size: 15px; }
                    Text { text: "STREAM"; color: #92939d; font-size: 11px; }
                    Rectangle {
                        height: 38px;
                        background: #34313f;
                        border-radius: 6px;
                        Text { text: "Display 1   1920 × 1080"; color: #eeeafc; font-size: 12px; vertical-alignment: center; x: 10px; }
                    }
                    Text { text: "Display 2   Offline"; color: #9a9ba4; font-size: 12px; }
                    Rectangle { height: 1px; background: #34353d; }
                    Text { text: "CONTROL"; color: #92939d; font-size: 11px; }
                    Text { text: "Remote Desktop"; color: #d7d7dd; font-size: 13px; }
                    Text { text: "Clipboard"; color: #d7d7dd; font-size: 13px; }
                    Rectangle { height: 1px; background: #34353d; }
                    Text { text: "SYSTEM"; color: #92939d; font-size: 11px; }
                    Text { text: "Performance"; color: #d7d7dd; font-size: 13px; }
                    Text { text: "Connection"; color: #d7d7dd; font-size: 13px; }
                    Text { text: "Settings"; color: #d7d7dd; font-size: 13px; }
                    Rectangle { vertical-stretch: 1; }
                    Text { text: "Local session   •   Ready"; color: #b9bac3; font-size: 12px; }
                }
            }

            VerticalLayout {
                horizontal-stretch: 1;
                spacing: 0px;
                Rectangle {
                    height: 64px;
                    background: #24252b;
                    border-color: #303139;
                    border-width: 1px;
                    VerticalLayout {
                        padding-left: 20px;
                        padding-right: 20px;
                        padding-top: 9px;
                        padding-bottom: 7px;
                        spacing: 3px;
                        Text { text: "WIN10-GAMING-PC / Display 1"; color: #f2f2f5; font-size: 17px; }
                        Text { text: root.frame-label; color: #a8a9b2; font-size: 12px; }
                    }
                }
                Rectangle {
                    vertical-stretch: 1;
                    background: #1b1c21;
                    Rectangle {
                        width: parent.width;
                        height: parent.height;
                        background: #0b0c0f;
                        border-color: #3b3c45;
                        border-width: 1px;
                        Image {
                            source: root.video-image;
                            image-fit: contain;
                            width: parent.width;
                            height: parent.height;
                        }
                        Rectangle {
                            x: 14px;
                            y: 14px;
                            width: 172px;
                            height: 30px;
                            background: #b0181b20;
                            border-radius: 6px;
                            Text { text: "SYNTHETIC • NV12 • 30 FPS"; color: #eeeeF3; font-size: 10px; horizontal-alignment: center; vertical-alignment: center; }
                        }
                    }
                }
                Rectangle {
                    height: 34px;
                    background: #24252b;
                    Text { text: "Keyboard capture: off     •     Audio: disabled     •     Synthetic GPU stream"; color: #a8a9b2; font-size: 11px; vertical-alignment: center; x: 16px; }
                }
            }

            Rectangle {
                width: 272px;
                background: #202127;
                border-color: #303139;
                border-width: 1px;
                VerticalLayout {
                    padding: 16px;
                    spacing: 9px;
                    Text { text: "SESSION"; color: #92939d; font-size: 11px; }
                    Text { text: "Connected • Direct"; color: #dfe0e6; font-size: 13px; }
                    Text { text: "RTT  8 ms     Loss  0.0%"; color: #b9bac3; font-size: 12px; }
                    Text { text: "Bitrate  6.4 Mbps"; color: #b9bac3; font-size: 12px; }
                    Rectangle { height: 1px; background: #34353d; }
                    Text { text: "HOST"; color: #92939d; font-size: 11px; }
                    Text { text: "CPU  12%"; color: #b9bac3; font-size: 12px; }
                    Text { text: "Capture  Synthetic"; color: #b9bac3; font-size: 12px; }
                    Text { text: "Encoder  Test pattern"; color: #b9bac3; font-size: 12px; }
                    Rectangle { height: 1px; background: #34353d; }
                    Text { text: "LIVE TEST METRICS"; color: #92939d; font-size: 11px; }
                    Text { text: root.telemetry-label; color: #d8d4ec; font-size: 12px; wrap: word-wrap; }
                    Rectangle { height: 1px; background: #34353d; }
                    Text { text: "EVENTS"; color: #92939d; font-size: 11px; }
                    Text { text: "GPU stream started"; color: #b9bac3; font-size: 12px; }
                    Text { text: "Telemetry tick: 4 Hz"; color: #b9bac3; font-size: 12px; }
                    Rectangle { vertical-stretch: 1; }
                    Text { text: "M4a spike • no screen capture"; color: #858690; font-size: 10px; }
                }
            }
        }
    }
}

#[derive(Default)]
struct Metrics {
    video_submitted: u64,
    telemetry_updates: u64,
    render_callbacks: u64,
    video_intervals_ms: Vec<f64>,
    video_submit_work_ms: Vec<f64>,
    render_intervals_ms: Vec<f64>,
    last_video_at: Option<Instant>,
    last_render_at: Option<Instant>,
    gpu_init_error: Option<String>,
}

struct GpuVideo {
    device: wgpu::Device,
    queue: wgpu::Queue,
    y_texture: wgpu::Texture,
    uv_texture: wgpu::Texture,
    output_texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
    y_data: Vec<u8>,
    uv_data: Vec<u8>,
    frame_index: u32,
    last_stripe_x: Option<usize>,
}

impl GpuVideo {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let y_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("synthetic-nv12-y"),
            size: wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let uv_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("synthetic-nv12-uv"),
            size: wgpu::Extent3d {
                width: CHROMA_WIDTH,
                height: CHROMA_HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rg8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let output_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("synthetic-nv12-rgba-output"),
            size: wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("nv12-yuv-to-rgba-fragment"),
            source: wgpu::ShaderSource::Wgsl(
                r#"
                @group(0) @binding(0) var y_plane: texture_2d<f32>;
                @group(0) @binding(1) var uv_plane: texture_2d<f32>;

                @vertex
                fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
                    var positions = array<vec2<f32>, 3>(
                        vec2<f32>(-1.0, -1.0),
                        vec2<f32>(3.0, -1.0),
                        vec2<f32>(-1.0, 3.0)
                    );
                    return vec4<f32>(positions[index], 0.0, 1.0);
                }

                @fragment
                fn convert(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
                    let coord = vec2<i32>(position.xy);
                    let uv_coord = vec2<i32>(coord.x / 2, coord.y / 2);
                    let y_code = textureLoad(y_plane, coord, 0).r * 255.0;
                    let uv_codes = textureLoad(uv_plane, uv_coord, 0).rg * 255.0;
                    let y = (y_code - 16.0) / 219.0;
                    let u = (uv_codes.x - 128.0) / 224.0;
                    let v = (uv_codes.y - 128.0) / 224.0;
                    let rgb = clamp(vec3<f32>(
                        y + 1.596027 * v,
                        y - 0.391762 * u - 0.812968 * v,
                        y + 2.017232 * u
                    ), vec3<f32>(0.0), vec3<f32>(1.0));
                    return vec4<f32>(rgb, 1.0);
                }
                "#
                .into(),
            ),
        });
        let targets = [Some(wgpu::ColorTargetState {
            format: wgpu::TextureFormat::Rgba8Unorm,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        })];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("nv12-to-rgba-fragment-pipeline"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("convert"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &targets,
            }),
            multiview_mask: None,
            cache: None,
        });
        let y_view = y_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let uv_view = uv_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("nv12-plane-bindings"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&y_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&uv_view),
                },
            ],
        });

        let mut video = Self {
            device: device.clone(),
            queue: queue.clone(),
            y_texture,
            uv_texture,
            output_texture,
            bind_group,
            pipeline,
            y_data: vec![0; (WIDTH * HEIGHT) as usize],
            uv_data: vec![128; (CHROMA_WIDTH * CHROMA_HEIGHT * 2) as usize],
            frame_index: 0,
            last_stripe_x: None,
        };
        video.make_test_pattern();
        video
    }

    fn make_test_pattern(&mut self) {
        let bars = [32u8, 62, 90, 120, 150, 180, 210, 235];
        for x in 0..WIDTH as usize {
            let value = bars[x * 8 / WIDTH as usize];
            for y in 0..HEIGHT as usize {
                self.y_data[y * WIDTH as usize + x] = value;
            }
        }
        let chroma_bars = [(90u8, 240u8), (54, 34), (240, 110), (166, 16)];
        for y in 0..CHROMA_HEIGHT as usize {
            for x in 0..CHROMA_WIDTH as usize {
                let i = (y * CHROMA_WIDTH as usize + x) * 2;
                let (u, v) = chroma_bars[(x / 120) % chroma_bars.len()];
                self.uv_data[i] = u;
                self.uv_data[i + 1] = v;
            }
        }
        self.update_moving_stripe();
    }

    fn update_moving_stripe(&mut self) {
        let stripe_width = 96usize;
        let new_x = (self.frame_index as usize * 37) % (WIDTH as usize - stripe_width);
        if let Some(old_x) = self.last_stripe_x {
            let luma_bars = [32u8, 62, 90, 120, 150, 180, 210, 235];
            for y in 0..HEIGHT as usize {
                let row = y * WIDTH as usize;
                for dx in 0..stripe_width {
                    let x = old_x + dx;
                    self.y_data[row + x] = luma_bars[x * 8 / WIDTH as usize];
                }
            }
            let old_chroma_x = old_x / 2;
            let chroma_bars = [(90u8, 240u8), (54, 34), (240, 110), (166, 16)];
            for y in 0..CHROMA_HEIGHT as usize {
                let row = (y * CHROMA_WIDTH as usize + old_chroma_x) * 2;
                for dx in 0..stripe_width / 2 {
                    let x = old_chroma_x + dx;
                    let (u, v) = chroma_bars[(x / 120) % chroma_bars.len()];
                    self.uv_data[row + dx * 2] = u;
                    self.uv_data[row + dx * 2 + 1] = v;
                }
            }
        }
        for y in 0..HEIGHT as usize {
            let row = y * WIDTH as usize + new_x;
            self.y_data[row..row + stripe_width].fill(235);
        }
        let new_chroma_x = new_x / 2;
        for y in 0..CHROMA_HEIGHT as usize {
            let row = (y * CHROMA_WIDTH as usize + new_chroma_x) * 2;
            self.uv_data[row..row + stripe_width].fill(128);
        }
        self.last_stripe_x = Some(new_x);
    }
    fn submit_frame(&mut self) {
        self.frame_index = self.frame_index.wrapping_add(1);
        self.update_moving_stripe();
        let y_extent = wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        };
        let uv_extent = wgpu::Extent3d {
            width: CHROMA_WIDTH,
            height: CHROMA_HEIGHT,
            depth_or_array_layers: 1,
        };
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.y_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &self.y_data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(WIDTH),
                rows_per_image: Some(HEIGHT),
            },
            y_extent,
        );
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.uv_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &self.uv_data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(WIDTH),
                rows_per_image: Some(CHROMA_HEIGHT),
            },
            uv_extent,
        );
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("nv12-frame-conversion"),
            });
        let output_view = self
            .output_texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let color_attachments = [Some(wgpu::RenderPassColorAttachment {
            view: &output_view,
            resolve_target: None,
            depth_slice: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        })];
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("convert-nv12-to-rgba"),
                color_attachments: &color_attachments,
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([encoder.finish()]);
    }
}

fn interval_stats(values: &[f64]) -> String {
    if values.is_empty() {
        return "n/a".to_string();
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let sum: f64 = sorted.iter().sum();
    let percentile = |p: f64| sorted[((sorted.len() - 1) as f64 * p).round() as usize];
    format!(
        "min {:.2} / avg {:.2} / p95 {:.2} / max {:.2} ms",
        sorted[0],
        sum / sorted.len() as f64,
        percentile(0.95),
        sorted[sorted.len() - 1]
    )
}

fn duration_seconds() -> u64 {
    env::args()
        .find_map(|arg| arg.strip_prefix("--duration-seconds=")?.parse::<u64>().ok())
        .unwrap_or(DEFAULT_DURATION_SECONDS)
        .clamp(3, 3600)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let duration = Duration::from_secs(duration_seconds());
    slint::BackendSelector::new()
        .require_wgpu_30(slint::wgpu_30::WGPUConfiguration::default())
        .select()?;

    let ui = SpikeWindow::new()?;
    let gpu_slot: Rc<RefCell<Option<GpuVideo>>> = Rc::new(RefCell::new(None));
    let metrics = Rc::new(RefCell::new(Metrics::default()));
    let gpu_for_setup = Rc::clone(&gpu_slot);
    let metrics_for_render = Rc::clone(&metrics);
    let metrics_for_setup = Rc::clone(&metrics);
    let ui_weak = ui.as_weak();
    ui.window()
        .set_rendering_notifier(move |state, graphics_api| match state {
            slint::RenderingState::RenderingSetup => {
                if let slint::GraphicsAPI::WGPU30 { device, queue, .. } = graphics_api {
                    let video = GpuVideo::new(device, queue);
                    match slint::Image::try_from(video.output_texture.clone()) {
                        Ok(image) => {
                            if let Some(ui) = ui_weak.upgrade() {
                                ui.set_video_image(image);
                            }
                            *gpu_for_setup.borrow_mut() = Some(video);
                        }
                        Err(error) => {
                            metrics_for_setup.borrow_mut().gpu_init_error =
                                Some(format!("Slint texture import failed: {error:?}"));
                        }
                    }
                } else {
                    metrics_for_setup.borrow_mut().gpu_init_error =
                        Some("Slint did not select WGPU 30".to_string());
                }
            }
            slint::RenderingState::BeforeRendering => {
                let now = Instant::now();
                let mut data = metrics_for_render.borrow_mut();
                data.render_callbacks += 1;
                if let Some(previous) = data.last_render_at.replace(now) {
                    data.render_intervals_ms
                        .push(now.duration_since(previous).as_secs_f64() * 1000.0);
                }
            }
            slint::RenderingState::RenderingTeardown => {
                gpu_for_setup.borrow_mut().take();
            }
            _ => {}
        })?;

    let started = Instant::now();
    let video_slot = Rc::clone(&gpu_slot);
    let video_metrics = Rc::clone(&metrics);
    let ui_weak_video = ui.as_weak();
    let _video_timer = Timer::default();
    _video_timer.start(TimerMode::Repeated, VIDEO_PERIOD, move || {
        if let Some(gpu) = video_slot.borrow_mut().as_mut() {
            let submit_started = Instant::now();
            gpu.submit_frame();
            let now = Instant::now();
            {
                let mut data = video_metrics.borrow_mut();
                data.video_submitted += 1;
                data.video_submit_work_ms
                    .push(now.duration_since(submit_started).as_secs_f64() * 1000.0);
                if let Some(previous) = data.last_video_at.replace(now) {
                    data.video_intervals_ms
                        .push(now.duration_since(previous).as_secs_f64() * 1000.0);
                }
            }
            if let Some(ui) = ui_weak_video.upgrade() {
                ui.set_frame_label(
                    format!(
                        "1920 × 1080 synthetic NV12 • frame {} • GPU conversion",
                        gpu.frame_index
                    )
                    .into(),
                );
                ui.window().request_redraw();
            }
        }
    });

    let telemetry_metrics = Rc::clone(&metrics);
    let ui_weak_telemetry = ui.as_weak();
    let _telemetry_timer = Timer::default();
    _telemetry_timer.start(TimerMode::Repeated, TELEMETRY_PERIOD, move || {
        let (updates, submitted, renders) = {
            let mut data = telemetry_metrics.borrow_mut();
            data.telemetry_updates += 1;
            (data.telemetry_updates, data.video_submitted, data.render_callbacks)
        };
        if let Some(ui) = ui_weak_telemetry.upgrade() {
            ui.set_telemetry_label(format!("4 Hz ticks: {updates}\nGPU submissions: {submitted}\nSlint render callbacks: {renders}").into());
        }
    });

    let finish_weak = ui.as_weak();
    let _finish_timer = Timer::default();
    _finish_timer.start(TimerMode::Repeated, Duration::from_millis(100), move || {
        if started.elapsed() >= duration {
            if let Err(error) = slint::quit_event_loop() {
                eprintln!("event loop stop failed: {error}");
            }
            if let Some(ui) = finish_weak.upgrade() {
                let _ = ui.window().hide();
            }
        }
    });

    println!("spike_started target_seconds={} video_period_us={} telemetry_period_ms={} resolution={}x{} nv12_bytes_per_frame={}", duration.as_secs(), VIDEO_PERIOD.as_micros(), TELEMETRY_PERIOD.as_millis(), WIDTH, HEIGHT, (WIDTH as u64 * HEIGHT as u64 * 3 / 2));
    ui.run()?;
    let elapsed = started.elapsed();
    let data = metrics.borrow();
    let rate = |count: u64| {
        if elapsed.as_secs_f64() > 0.0 {
            count as f64 / elapsed.as_secs_f64()
        } else {
            0.0
        }
    };
    println!(
        "spike_finished duration_seconds={:.3}",
        elapsed.as_secs_f64()
    );
    let active_interval_hz = if data.video_intervals_ms.is_empty() {
        0.0
    } else {
        1000.0 * data.video_intervals_ms.len() as f64 / data.video_intervals_ms.iter().sum::<f64>()
    };
    println!(
        "video_gpu_submitted_frames={} wall_clock_submissions_hz={:.2} active_interval_hz={:.2} intervals_ms=[{}] cpu_upload_and_submit_ms=[{}]",
        data.video_submitted,
        rate(data.video_submitted),
        active_interval_hz,
        interval_stats(&data.video_intervals_ms),
        interval_stats(&data.video_submit_work_ms)
    );
    println!(
        "slint_before_render_callbacks={} average_render_callbacks_hz={:.2} intervals_ms=[{}]",
        data.render_callbacks,
        rate(data.render_callbacks),
        interval_stats(&data.render_intervals_ms)
    );
    println!(
        "telemetry_updates={} average_updates_hz={:.2} target_hz=4",
        data.telemetry_updates,
        rate(data.telemetry_updates)
    );
    println!("presentation_note=BeforeRendering callbacks are a framework render proxy, not confirmed physical monitor presents; human visual stutter confirmation is still required");
    if let Some(error) = &data.gpu_init_error {
        println!("gpu_init_error={error}");
    }
    println!("capture_note=no desktop capture or screenshots were performed");
    Ok(())
}

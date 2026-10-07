use iced::advanced::{graphics::Viewport, mouse::Cursor};
use iced::time::Instant;
use iced::wgpu;
use iced::widget::{column, container, row, text};
use iced::{Element, Fill, Rectangle, Subscription, Task, Theme};
use std::collections::VecDeque;
use std::env;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const FRAME_PERIOD: Duration = Duration::from_nanos(1_000_000_000 / 30);
const TELEMETRY_PERIOD: Duration = Duration::from_millis(250);
const SHADER: &str = include_str!("nv12.wgsl");

fn app_theme(_: &AppState) -> Theme {
    Theme::Dark
}

fn main() -> iced::Result {
    let seconds = env::args()
        .skip(1)
        .find_map(|arg| arg.strip_prefix("--duration-secs=")?.parse::<u64>().ok())
        .or_else(|| {
            let mut args = env::args().skip(1);
            while let Some(arg) = args.next() {
                if arg == "--duration-secs" {
                    return args.next()?.parse::<u64>().ok();
                }
            }
            None
        })
        .unwrap_or(60)
        .max(1);

    let metrics = Arc::new(Metrics::default());
    let source = Arc::new(Mutex::new(SyntheticFrame::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let worker = spawn_source(Arc::clone(&source), Arc::clone(&metrics), Arc::clone(&stop));
    print_adapter_probe();
    println!(
        "measurement duration={}s target=30Hz telemetry=4Hz frame=1920x1080 NV12",
        seconds
    );

    let app_metrics = Arc::clone(&metrics);
    let app_source = Arc::clone(&source);
    let app = iced::application(
        move || AppState::new(seconds, Arc::clone(&app_metrics), Arc::clone(&app_source)),
        AppState::update,
        AppState::view,
    )
    .subscription(AppState::subscription)
    .title("Racc Connect — iced M4a spike")
    .window_size((1440.0, 900.0))
    .theme(app_theme);

    let result = app.run();
    stop.store(true, Ordering::Relaxed);
    if let Err(error) = worker.join() {
        eprintln!("source thread join failed: {error:?}");
    }
    metrics.print_summary(seconds);
    result
}

#[derive(Debug, Default)]
struct Metrics {
    started: Mutex<Option<Instant>>,
    generated: AtomicU64,
    uploaded: AtomicU64,
    uploaded_bytes: AtomicU64,
    prepare_calls: AtomicU64,
    prepare_ns: AtomicU64,
    draw_calls: AtomicU64,
    ui_frames: AtomicU64,
    telemetry_updates: AtomicU64,
    generator_intervals_ns: Mutex<VecDeque<u64>>,
    upload_intervals_ns: Mutex<VecDeque<u64>>,
    ui_intervals_ns: Mutex<VecDeque<u64>>,
    telemetry_intervals_ns: Mutex<VecDeque<u64>>,
}

impl Metrics {
    fn record_interval(bucket: &Mutex<VecDeque<u64>>, interval: Duration) {
        let mut values = bucket.lock().unwrap_or_else(|e| e.into_inner());
        if values.len() == 4096 {
            values.pop_front();
        }
        values.push_back(interval.as_nanos().min(u64::MAX as u128) as u64);
    }

    fn print_summary(&self, requested_secs: u64) {
        let elapsed = self
            .started
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|start| start.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        println!("measurement elapsed_s={elapsed:.3} requested_s={requested_secs}");
        println!("counts generated={} uploaded={} bytes={} prepare={} draw={} ui_frames={} telemetry_updates={}",
            self.generated.load(Ordering::Relaxed), self.uploaded.load(Ordering::Relaxed),
            self.uploaded_bytes.load(Ordering::Relaxed), self.prepare_calls.load(Ordering::Relaxed),
            self.draw_calls.load(Ordering::Relaxed), self.ui_frames.load(Ordering::Relaxed), self.telemetry_updates.load(Ordering::Relaxed));
        println!("rates generated_fps={:.2} uploaded_fps={:.2} ui_frames_fps={:.2} telemetry_hz={:.2} upload_MiB_s={:.2}",
            rate(self.generated.load(Ordering::Relaxed), elapsed), rate(self.uploaded.load(Ordering::Relaxed), elapsed),
            rate(self.ui_frames.load(Ordering::Relaxed), elapsed), rate(self.telemetry_updates.load(Ordering::Relaxed), elapsed),
            self.uploaded_bytes.load(Ordering::Relaxed) as f64 / elapsed.max(0.001) / (1024.0 * 1024.0));
        print_intervals("generator", &self.generator_intervals_ns);
        print_intervals("texture_upload_enqueue", &self.upload_intervals_ns);
        print_intervals("window_frames", &self.ui_intervals_ns);
        print_intervals("telemetry", &self.telemetry_intervals_ns);
        println!("limits upload timing measures CPU queue.write_texture enqueue only; window frame subscription is not physical present; no GPU timestamps or human smoothness claim");
    }
}

fn rate(count: u64, seconds: f64) -> f64 {
    count as f64 / seconds.max(0.001)
}

fn print_adapter_probe() {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapters = instance.enumerate_adapters(wgpu::Backends::all());
    println!(
        "adapter_probe source=separate_wgpu_instance count={}",
        adapters.len()
    );
    for adapter in adapters {
        let info = adapter.get_info();
        println!("adapter_probe name={:?} vendor={:#06x} device={:#06x} type={:?} backend={:?} driver={:?}",
            info.name, info.vendor, info.device, info.device_type, info.backend, info.driver);
    }
}

fn print_intervals(name: &str, bucket: &Mutex<VecDeque<u64>>) {
    let mut samples: Vec<u64> = bucket
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .copied()
        .collect();
    if samples.is_empty() {
        println!("intervals {name} samples=0");
        return;
    }
    samples.sort_unstable();
    let sum: u128 = samples.iter().map(|v| *v as u128).sum();
    let mean_ms = sum as f64 / samples.len() as f64 / 1_000_000.0;
    let p95_ms = samples[((samples.len() - 1) * 95) / 100] as f64 / 1_000_000.0;
    let max_ms = *samples.last().unwrap_or(&0) as f64 / 1_000_000.0;
    println!(
        "intervals {name} samples={} mean_ms={mean_ms:.3} p95_ms={p95_ms:.3} max_ms={max_ms:.3}",
        samples.len()
    );
}

#[derive(Debug)]
struct SyntheticFrame {
    index: u64,
    y: Vec<u8>,
    uv: Vec<u8>,
}
impl SyntheticFrame {
    fn new() -> Self {
        Self {
            index: 0,
            y: vec![0; (WIDTH * HEIGHT) as usize],
            uv: vec![128; (WIDTH * HEIGHT / 2) as usize],
        }
    }
    fn generate_next(&mut self) {
        self.index = self.index.wrapping_add(1);
        let bar_x = (self.index as u32 * 13) % WIDTH;
        for row in 0..HEIGHT {
            let row_start = (row * WIDTH) as usize;
            for x in 0..WIDTH {
                let base = 40 + ((x * 90 / WIDTH) as u8);
                self.y[row_start + x as usize] = if x.abs_diff(bar_x) < 48 { 190 } else { base };
            }
        }
        let phase = (self.index % 60) as u8;
        self.uv.fill(128u8.wrapping_add(phase / 4));
    }
}

fn spawn_source(
    source: Arc<Mutex<SyntheticFrame>>,
    metrics: Arc<Metrics>,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let start = Instant::now();
        *metrics.started.lock().unwrap_or_else(|e| e.into_inner()) = Some(start);
        let mut next = start;
        let mut previous = None;
        while !stop.load(Ordering::Relaxed) {
            next += FRAME_PERIOD;
            {
                let mut frame = source.lock().unwrap_or_else(|e| e.into_inner());
                frame.generate_next();
                metrics.generated.fetch_add(1, Ordering::Relaxed);
            }
            let now = Instant::now();
            if let Some(last) = previous {
                Metrics::record_interval(&metrics.generator_intervals_ns, now - last);
            }
            previous = Some(now);
            if now < next {
                thread::sleep(next - now);
            } else {
                next = now;
            }
        }
    })
}

#[derive(Debug, Clone)]
enum Message {
    Telemetry(Instant),
    WindowFrame(Instant),
}

struct AppState {
    started: Instant,
    duration: Duration,
    metrics: Arc<Metrics>,
    source: Arc<Mutex<SyntheticFrame>>,
    last_ui_frame: Option<Instant>,
    last_telemetry: Option<Instant>,
    telemetry_count: u64,
    exiting: bool,
}

impl AppState {
    fn new(seconds: u64, metrics: Arc<Metrics>, source: Arc<Mutex<SyntheticFrame>>) -> Self {
        let started = Instant::now();
        Self {
            started,
            duration: Duration::from_secs(seconds),
            metrics,
            source,
            last_ui_frame: None,
            last_telemetry: None,
            telemetry_count: 0,
            exiting: false,
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::WindowFrame(now) => {
                self.metrics.ui_frames.fetch_add(1, Ordering::Relaxed);
                if let Some(previous) = self.last_ui_frame {
                    Metrics::record_interval(&self.metrics.ui_intervals_ns, now - previous);
                }
                self.last_ui_frame = Some(now);
            }
            Message::Telemetry(now) => {
                self.metrics
                    .telemetry_updates
                    .fetch_add(1, Ordering::Relaxed);
                self.telemetry_count = self.telemetry_count.wrapping_add(1);
                if let Some(previous) = self.last_telemetry {
                    Metrics::record_interval(&self.metrics.telemetry_intervals_ns, now - previous);
                }
                self.last_telemetry = Some(now);
                if !self.exiting && self.started.elapsed() >= self.duration {
                    self.exiting = true;
                    return iced::exit();
                }
            }
        }
        Task::none()
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::window::frames().map(Message::WindowFrame),
            iced::time::every(TELEMETRY_PERIOD).map(|_| Message::Telemetry(Instant::now())),
        ])
    }

    fn view(&self) -> Element<'_, Message> {
        let rail = container(column![text("RC"), text("●"), text("＋")].spacing(24))
            .width(68)
            .height(Fill)
            .padding(14);
        let device_sidebar = container(
            column![
                text("RACC CONNECT").size(15),
                text("DEVICES").size(12),
                text("●  WORKSTATION"),
                text("STREAM"),
                text("  Display 1  • 1920×1080"),
                text("  Display 2  • 2560×1440"),
                text("CONTROL"),
                text("  Remote Desktop"),
                text("  Clipboard"),
                text("SYSTEM"),
                text("  Performance"),
                text("  Settings"),
                text("LOCAL SESSION"),
                text("WIN10-GAMING-PC  • Online"),
            ]
            .spacing(14),
        )
        .width(235)
        .height(Fill)
        .padding(18);

        let source = Arc::clone(&self.source);
        let metrics = Arc::clone(&self.metrics);
        let video = iced::widget::shader(VideoProgram { source, metrics })
            .width(Fill)
            .height(Fill);
        let header = container(
            row![
                text("WORKSTATION  /  Display 1").size(18),
                text("1920×1080  •  30 FPS  •  NV12 synthetic source"),
            ]
            .spacing(24),
        )
        .width(Fill)
        .padding(16);
        let workspace = container(column![header, video].spacing(12).height(Fill))
            .width(Fill)
            .height(Fill)
            .padding(14);
        let generated = self.metrics.generated.load(Ordering::Relaxed);
        let uploaded = self.metrics.uploaded.load(Ordering::Relaxed);
        let telemetry = container(
            column![
                text("SESSION").size(13),
                text("Connected  •  Direct"),
                text("RTT         8 ms"),
                text("Packet loss 0.0%"),
                text("Bitrate     6.4 Mbps"),
                text("FPS         30"),
                text("Codec       H.264"),
                text("Decoder     NV12 shader"),
                text("HOST").size(13),
                text("CPU         18%"),
                text("Capture     Synthetic"),
                text("Resolution  1920×1080"),
                text("Refresh     60 Hz"),
                text("SPIKE MEASUREMENTS").size(13),
                text(format!("Generated  {generated}")),
                text(format!("Uploaded   {uploaded}")),
                text(format!("Telemetry  {} ticks", self.telemetry_count)),
                text("EVENTS").size(13),
                text("Stream started"),
                text("Synthetic source active"),
            ]
            .spacing(12),
        )
        .width(286)
        .height(Fill)
        .padding(18);

        row![rail, device_sidebar, workspace, telemetry]
            .height(Fill)
            .into()
    }
}

struct VideoProgram {
    source: Arc<Mutex<SyntheticFrame>>,
    metrics: Arc<Metrics>,
}
impl iced::widget::shader::Program<Message> for VideoProgram {
    type State = ();
    type Primitive = VideoPrimitive;
    fn draw(&self, _state: &Self::State, _cursor: Cursor, _bounds: Rectangle) -> Self::Primitive {
        VideoPrimitive {
            source: Arc::clone(&self.source),
            metrics: Arc::clone(&self.metrics),
        }
    }
}

#[derive(Debug)]
struct VideoPrimitive {
    source: Arc<Mutex<SyntheticFrame>>,
    metrics: Arc<Metrics>,
}
impl iced::widget::shader::Primitive for VideoPrimitive {
    type Pipeline = VideoPipeline;

    fn prepare(
        &self,
        pipeline: &mut Self::Pipeline,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        _viewport: &Viewport,
    ) {
        let started = Instant::now();
        let frame = self.source.lock().unwrap_or_else(|e| e.into_inner());
        if frame.index != pipeline.last_uploaded {
            let y_layout = wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(WIDTH),
                rows_per_image: Some(HEIGHT),
            };
            let uv_layout = wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(WIDTH),
                rows_per_image: Some(HEIGHT / 2),
            };
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &pipeline.y_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &frame.y,
                y_layout,
                wgpu::Extent3d {
                    width: WIDTH,
                    height: HEIGHT,
                    depth_or_array_layers: 1,
                },
            );
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &pipeline.uv_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &frame.uv,
                uv_layout,
                wgpu::Extent3d {
                    width: WIDTH / 2,
                    height: HEIGHT / 2,
                    depth_or_array_layers: 1,
                },
            );
            pipeline.last_uploaded = frame.index;
            self.metrics.uploaded.fetch_add(1, Ordering::Relaxed);
            self.metrics
                .uploaded_bytes
                .fetch_add((frame.y.len() + frame.uv.len()) as u64, Ordering::Relaxed);
            let now = Instant::now();
            if let Some(previous) = pipeline.last_upload_at {
                Metrics::record_interval(&self.metrics.upload_intervals_ns, now - previous);
            }
            pipeline.last_upload_at = Some(now);
        }
        let mut uniform_bytes = [0u8; 16];
        uniform_bytes[0..4].copy_from_slice(&bounds.width.to_ne_bytes());
        uniform_bytes[4..8].copy_from_slice(&bounds.height.to_ne_bytes());
        queue.write_buffer(&pipeline.uniform_buffer, 0, &uniform_bytes);
        self.metrics.prepare_calls.fetch_add(1, Ordering::Relaxed);
        self.metrics.prepare_ns.fetch_add(
            started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }

    fn draw(&self, pipeline: &Self::Pipeline, pass: &mut wgpu::RenderPass<'_>) -> bool {
        pass.set_pipeline(&pipeline.render_pipeline);
        pass.set_bind_group(0, &pipeline.bind_group, &[]);
        pass.draw(0..3, 0..1);
        self.metrics.draw_calls.fetch_add(1, Ordering::Relaxed);
        true
    }
}

struct VideoPipeline {
    render_pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    y_texture: wgpu::Texture,
    uv_texture: wgpu::Texture,
    last_uploaded: u64,
    last_upload_at: Option<Instant>,
}
impl iced::widget::shader::Pipeline for VideoPipeline {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        Self::build(device, queue, format)
    }
}

impl VideoPipeline {
    fn build(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let y_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("synthetic NV12 Y plane"),
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
            label: Some("synthetic NV12 interleaved UV plane"),
            size: wgpu::Extent3d {
                width: WIDTH / 2,
                height: HEIGHT / 2,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rg8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let y_view = y_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let uv_view = uv_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("NV12 linear sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("video panel dimensions"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("NV12 planes and panel uniforms"),
            entries: &[
                texture_entry(0),
                texture_entry(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("NV12 sample bindings"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&y_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&uv_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: uniform_buffer.as_entire_binding(),
                },
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("NV12 to RGB shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("NV12 pipeline layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("NV12 panel render pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        Self {
            render_pipeline,
            bind_group,
            uniform_buffer,
            y_texture,
            uv_texture,
            last_uploaded: u64::MAX,
            last_upload_at: None,
        }
    }
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

//! Desktop UI shell driven by deterministic fake devices in `--fake` mode.
mod design;
mod focus_button;
mod tray;
mod view_model;

use focus_button::FocusableButton;

use design::{region_widths, tokens};
use iced::advanced::{graphics::Viewport, mouse::Cursor};
use iced::time::Instant as IcedInstant;
use iced::widget::{button, column, container, row, scrollable, text, tooltip};
use iced::{Background, Border, Color, Element, Fill, Rectangle, Subscription, Task, Theme};
use racc_core::{CoreHandle, DeviceId, FramePayload, FrameSource, QualityPreset};
use racc_testkit::{FakeCore, DEFAULT_FAKE_SEED};
use std::{
    env,
    sync::Arc,
    time::{Duration, Instant},
};
use tray::{NoopTrayController, TrayAction, TrayController};
use view_model::{Page, SessionOverlay, UserAction, ViewModel};

const TELEMETRY_PERIOD: Duration = Duration::from_millis(250);
const VIDEO_PERIOD: Duration = Duration::from_nanos(33_333_333);
const POLL_LIMIT: usize = 64;
const VIDEO_SHADER: &str = include_str!("video.wgsl");

fn window_title(_: &App) -> String {
    "Racc Connect".to_owned()
}
fn app_theme(_: &App) -> Theme {
    Theme::Dark
}

fn main() -> iced::Result {
    if !env::args().any(|argument| argument == "--fake") {
        eprintln!("This milestone provides the fake shell only. Run with --fake.");
        return Ok(());
    }
    let collapsed = env::args().any(|argument| argument == "--telemetry-collapsed");
    let measure = env::args().find_map(|arg| arg.strip_prefix("--measure-secs=")?.parse().ok());
    let idle = env::args().any(|argument| argument == "--fake-idle");
    iced::application(
        move || App::new(collapsed, measure, idle),
        App::update,
        App::view,
    )
    .subscription(App::subscription)
    .title(window_title)
    .theme(app_theme)
    .window(iced::window::Settings {
        size: iced::Size::new(1_440.0, 900.0),
        min_size: Some(iced::Size::new(
            tokens::MIN_WINDOW_WIDTH,
            tokens::MIN_WINDOW_HEIGHT,
        )),
        exit_on_close_request: false,
        ..Default::default()
    })
    .run()
}

#[derive(Debug, Clone)]
enum Message {
    Action(UserAction),
    TelemetryTick,
    FrameTick(IcedInstant),
    Resized(iced::Size),
    Minimized(Option<bool>),
    ReleaseCapture,
    ToggleFullscreen,
    WindowId(Option<iced::window::Id>),
    CloseRequested,
    FocusNext,
    FocusPrevious,
    FocusWidget(iced::advanced::widget::Id),
}

struct App {
    core: FakeCore,
    frame_source: Arc<dyn FrameSource>,
    model: ViewModel,
    fullscreen: bool,
    window_id: Option<iced::window::Id>,
    window_width: f32,
    last_frame_id: Option<u32>,
    frame_intervals_ms: Vec<f64>,
    last_frame_at: Option<Instant>,
    measurement_started: Instant,
    measurement_ready: bool,
    measurement_secs: Option<u64>,
    measurement_reported: bool,
    fake_idle: bool,
    tray: NoopTrayController,
}

impl App {
    fn new(collapsed: bool, measurement_secs: Option<u64>, fake_idle: bool) -> Self {
        let fake = FakeCore::new(DEFAULT_FAKE_SEED);
        let snapshot = fake.snapshot().unwrap_or_default();
        let frame_source = fake.frame_source();
        let mut model = ViewModel::new(snapshot);
        model.telemetry_sidebar_collapsed = collapsed;
        let mut tray = NoopTrayController;
        let _ = tray.available();
        let _ = tray.poll_action();
        Self {
            core: fake,
            frame_source,
            model,
            fullscreen: false,
            window_id: None,
            window_width: 1_440.0,
            last_frame_id: None,
            frame_intervals_ms: Vec::with_capacity(4_096),
            last_frame_at: None,
            measurement_started: Instant::now(),
            measurement_ready: fake_idle,
            measurement_secs,
            measurement_reported: false,
            fake_idle,
            tray,
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Action(action) => {
                let hide = matches!(&action, UserAction::SetVisible(false));
                self.apply_action(action);
                if hide {
                    if let Some(id) = self.window_id {
                        return iced::window::minimize(id, true);
                    }
                }
            }
            Message::TelemetryTick => {
                if let Some(action) = self.tray.poll_action() {
                    match action {
                        TrayAction::ShowWindow => {
                            self.apply_action(UserAction::SetVisible(true));
                            if let Some(id) = self.window_id {
                                return iced::window::minimize(id, false);
                            }
                        }
                        TrayAction::Quit => return iced::exit(),
                    }
                }
                match self.core.poll_events(POLL_LIMIT) {
                    Ok(events) => {
                        for event in events {
                            self.model.apply_event(event);
                        }
                    }
                    Err(error) => self.model.notification = Some(error.to_string()),
                }
                match self.core.snapshot() {
                    Ok(snapshot) => self.model.apply_snapshot(snapshot),
                    Err(error) => self.model.notification = Some(error.to_string()),
                }
                if self.measurement_secs.is_some_and(|limit| {
                    self.measurement_started.elapsed() >= Duration::from_secs(limit)
                }) && !self.measurement_reported
                {
                    self.measurement_reported = true;
                    self.print_measurement();
                    return iced::exit();
                }
                let mut tasks = vec![iced::window::oldest().map(Message::WindowId)];
                if let Some(id) = self.window_id {
                    tasks.push(iced::window::is_minimized(id).map(Message::Minimized));
                }
                return Task::batch(tasks);
            }
            Message::FrameTick(now) => {
                if !self.measurement_ready {
                    self.measurement_started = Instant::now();
                    self.measurement_ready = true;
                }
                let elapsed = now
                    .saturating_duration_since(self.measurement_started)
                    .as_micros()
                    .min(u128::from(u64::MAX)) as u64;
                if !self.fake_idle {
                    self.core.advance_to(elapsed);
                }
                if !self.fake_idle {
                    if let Some(frame) = self.frame_source.latest_frame() {
                        self.model.record_frame(frame.frame_id);
                        if self.last_frame_id != Some(frame.frame_id) {
                            let now = Instant::now();
                            if let Some(previous) = self.last_frame_at {
                                self.frame_intervals_ms
                                    .push((now - previous).as_secs_f64() * 1_000.0);
                                if self.frame_intervals_ms.len() > 8_192 {
                                    self.frame_intervals_ms.remove(0);
                                }
                            }
                            self.last_frame_at = Some(now);
                            self.last_frame_id = Some(frame.frame_id);
                        }
                    }
                }
            }
            Message::WindowId(id) => self.window_id = id,
            Message::Resized(size) => self.window_width = size.width,
            Message::Minimized(minimized) => {
                if minimized == Some(true) && self.model.core.visible {
                    self.apply_action(UserAction::SetVisible(false));
                } else if minimized == Some(false) && !self.model.core.visible {
                    self.apply_action(UserAction::SetVisible(true));
                }
            }
            Message::ReleaseCapture => {
                self.apply_action(UserAction::SetKeyboardCapture(false));
                self.apply_action(UserAction::SetMouseCapture(false));
            }
            Message::ToggleFullscreen => {
                self.fullscreen = !self.fullscreen;
                let mode = if self.fullscreen {
                    iced::window::Mode::Fullscreen
                } else {
                    iced::window::Mode::Windowed
                };
                return self
                    .window_id
                    .map_or_else(Task::none, |id| iced::window::set_mode(id, mode));
            }
            Message::FocusNext => return iced::widget::operation::focus_next(),
            Message::FocusPrevious => return iced::widget::operation::focus_previous(),
            Message::FocusWidget(id) => return iced::widget::operation::focus(id),
            Message::CloseRequested => {
                self.apply_action(UserAction::SetVisible(false));
                if let Some(id) = self.window_id {
                    return iced::window::minimize(id, true);
                }
            }
        }
        Task::none()
    }

    fn apply_action(&mut self, action: UserAction) {
        let commands = self.model.reduce_action(action);
        for command in commands {
            if let Err(error) = self.core.send(command) {
                self.model.notification = Some(error.to_string());
            }
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = vec![
            iced::time::every(TELEMETRY_PERIOD).map(|_| Message::TelemetryTick),
            iced::window::resize_events().map(|(_, size)| Message::Resized(size)),
            iced::event::listen_with(|event, _status, _id| match event {
                iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                    ..
                }) => Some(Message::ReleaseCapture),
                iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Tab),
                    modifiers,
                    ..
                }) => Some(if modifiers.shift() {
                    Message::FocusPrevious
                } else {
                    Message::FocusNext
                }),
                iced::Event::Window(iced::window::Event::CloseRequested) => {
                    Some(Message::CloseRequested)
                }
                _ => None,
            }),
        ];
        if !self.fake_idle && self.model.core.visible {
            subscriptions.push(iced::time::every(VIDEO_PERIOD).map(Message::FrameTick));
        }
        Subscription::batch(subscriptions)
    }

    fn view(&self) -> Element<'_, Message> {
        let widths = region_widths(
            self.window_width,
            self.model.device_sidebar_collapsed,
            self.model.telemetry_sidebar_collapsed,
        );
        row![
            self.device_rail(),
            self.device_sidebar().width(widths.device_sidebar),
            self.workspace().width(Fill),
            self.telemetry_sidebar().width(widths.telemetry)
        ]
        .height(Fill)
        .into()
    }

    fn device_rail(&self) -> Element<'_, Message> {
        let mut rail = column![
            action_button(
                "RC",
                Some(Message::Action(UserAction::Home)),
                self.model.page == Page::Home
            ),
            divider_label("DEVICES")
        ]
        .spacing(tokens::SPACE_2)
        .align_x(iced::Alignment::Center);
        for device in &self.model.core.devices {
            let action = (device.online && device.host_capable)
                .then(|| Message::Action(UserAction::SelectDevice(device.id.clone())));
            let initial = device
                .name
                .chars()
                .next()
                .unwrap_or('D')
                .to_uppercase()
                .to_string();
            let status = if device.online { "●" } else { "○" };
            let status_color = if device.online {
                tokens::ONLINE
            } else {
                tokens::OFFLINE
            };
            let badge = column![text(status).color(status_color), text(initial)]
                .spacing(tokens::SPACE_1)
                .align_x(iced::Alignment::Center);
            let selected = self.model.core.selected_device.as_ref() == Some(&device.id);
            let item = row![
                text(if selected { "▏" } else { " " }).color(if selected {
                    tokens::ACCENT
                } else {
                    tokens::RAIL
                }),
                action_button_widget(badge, action, selected, format!("device:{:?}", device.id),),
            ]
            .align_y(iced::Alignment::Center);
            rail = rail.push(tooltip(
                item,
                text(device.name.clone()),
                iced::widget::tooltip::Position::Right,
            ));
        }
        rail = rail
            .push(tooltip(
                action_button(
                    "⌕",
                    Some(Message::Action(UserAction::DiscoverDevices)),
                    false,
                ),
                text("Discover fake devices"),
                iced::widget::tooltip::Position::Right,
            ))
            .push(iced::widget::Space::new().height(Fill))
            .push(action_button(
                "Session",
                Some(Message::Action(UserAction::Session)),
                self.model.page == Page::Session,
            ));
        container(rail)
            .width(tokens::DEVICE_RAIL_WIDTH)
            .height(Fill)
            .padding(tokens::SPACE_2)
            .style(panel_style(tokens::RAIL))
            .into()
    }

    fn device_sidebar(&self) -> iced::widget::Container<'_, Message> {
        if self.model.device_sidebar_collapsed {
            return container(
                column![
                    action_button(
                        "»",
                        Some(Message::Action(UserAction::ToggleDeviceSidebar)),
                        false
                    ),
                    local_panel(&self.model)
                ]
                .spacing(tokens::SPACE_3),
            )
            .height(Fill)
            .padding(tokens::SPACE_3)
            .style(panel_style(tokens::SIDEBAR));
        }
        let selected = self.selected_device();
        let mut displays = column![section_label("STREAM")].spacing(tokens::SPACE_1);
        if let Some(device) = selected {
            for display in &device.displays {
                let is_selected = self.model.core.selected_display == Some(display.id);
                let availability = if display.available {
                    "Available"
                } else {
                    "Unavailable"
                };
                let title = format!(
                    "{}  {}×{}",
                    display.name, display.width_px, display.height_px
                );
                let info = format!(
                    "{:.0} Hz  ·  {:.0}%  ·  {availability}",
                    display.refresh_mhz as f32 / 1_000.0,
                    100.0 * display.scale_milli as f32 / 1_000.0
                );
                let item = column![text(title).size(tokens::BODY_SIZE), muted_text(&info)]
                    .spacing(tokens::SPACE_1);
                let action = display
                    .available
                    .then_some(Message::Action(UserAction::SelectDisplay(display.id)));
                displays = displays.push(action_button_widget(
                    item,
                    action,
                    is_selected,
                    format!("display:{:?}", display.id),
                ));
            }
        } else {
            displays = displays.push(muted_text("Choose an online device from the rail"));
        }
        let header = row![
            column![
                text(selected.map_or("No device selected", |device| device.name.as_str()))
                    .size(tokens::HEADER_SIZE),
                muted_text(selected.map_or("Choose a device from the rail", |device| {
                    if device.online {
                        "Online  ·  remote display host"
                    } else {
                        "Offline  ·  unavailable"
                    }
                })),
            ]
            .spacing(tokens::SPACE_1),
            iced::widget::Space::new().width(Fill),
            action_button(
                "‹",
                Some(Message::Action(UserAction::ToggleDeviceSidebar)),
                false
            ),
        ]
        .align_y(iced::Alignment::Center);
        let controls = column![
            section_label("CONTROL"),
            action_button(
                "Remote Desktop  ·  keyboard + mouse",
                Some(Message::Action(UserAction::ToggleRemoteDesktop)),
                self.model.keyboard_capture && self.model.mouse_capture
            ),
            muted_text(&format!("Clipboard  ·  {}", self.model.clipboard_status)),
            section_label("SYSTEM"),
            action_button(
                "Performance",
                Some(Message::Action(UserAction::ToggleTelemetrySidebar)),
                false
            ),
            action_button(
                "Connection",
                Some(Message::Action(UserAction::ToggleTelemetrySidebar)),
                false
            ),
            action_button(
                "Settings",
                Some(Message::Action(UserAction::Settings)),
                self.model.page == Page::Settings
            ),
        ]
        .spacing(tokens::SPACE_1);
        container(
            column![
                header,
                scrollable(column![displays, controls].spacing(tokens::SPACE_4))
                    .height(Fill)
                    .style(scroll_style),
                local_panel(&self.model)
            ]
            .spacing(tokens::SPACE_3)
            .height(Fill),
        )
        .height(Fill)
        .padding(tokens::SPACE_3)
        .style(panel_style(tokens::SIDEBAR))
    }

    fn selected_device(&self) -> Option<&racc_core::DeviceSnapshot> {
        let selected = self.model.core.selected_device.as_ref()?;
        self.model
            .core
            .devices
            .iter()
            .find(|device| &device.id == selected)
    }

    fn workspace(&self) -> iced::widget::Container<'_, Message> {
        match self.model.page {
            Page::Home => self.home_page(),
            Page::Settings => self.settings_page(),
            Page::Session => self.session_page(),
        }
    }

    fn session_page(&self) -> iced::widget::Container<'_, Message> {
        let device = self.selected_device();
        let display = device.and_then(|device| {
            self.model
                .core
                .selected_display
                .and_then(|id| device.displays.iter().find(|display| display.id == id))
        });
        let title = match (device, display) {
            (Some(device), Some(display)) => format!("{}  /  {}", device.name, display.name),
            (Some(device), None) => format!("{}  /  No display selected", device.name),
            _ => "Session workspace".to_owned(),
        };
        let subtitle = match (display, self.model.stream_dimensions) {
            (Some(display), Some((_, height))) => format!(
                "{:.0} Hz display  ·  streaming {height}p30  ·  H.264  ·  {} ms",
                display.refresh_mhz as f32 / 1_000.0,
                self.model
                    .core
                    .telemetry
                    .session
                    .last_rtt_us
                    .unwrap_or(8_000)
                    / 1_000
            ),
            (Some(display), None) => format!(
                "{}×{} display  ·  waiting for stream",
                display.width_px, display.height_px
            ),
            _ => "Choose a display from the device sidebar".to_owned(),
        };
        let header = column![
            row![
                column![text(title).size(tokens::HEADER_SIZE), muted_text(&subtitle)]
                    .spacing(tokens::SPACE_1),
                iced::widget::Space::new().width(Fill),
                action_button(
                    "Keyboard",
                    Some(Message::Action(UserAction::SetKeyboardCapture(
                        !self.model.keyboard_capture
                    ))),
                    self.model.keyboard_capture
                ),
                action_button(
                    "Disconnect",
                    Some(Message::Action(UserAction::Disconnect)),
                    false
                ),
                action_button(
                    "Fullscreen",
                    Some(Message::ToggleFullscreen),
                    self.fullscreen
                ),
                action_button(
                    "Hide",
                    Some(Message::Action(UserAction::SetVisible(false))),
                    false
                ),
            ]
            .spacing(tokens::SPACE_1)
            .align_y(iced::Alignment::Center),
            quality_controls(
                self.model.core.selected_device.clone(),
                self.model.session_quality,
                false
            ),
        ]
        .spacing(tokens::SPACE_2);
        let video: Element<'_, Message> = if self.fake_idle {
            container(muted_text(
                "Idle measurement mode  ·  no active video frames",
            ))
            .width(Fill)
            .height(Fill)
            .center_x(Fill)
            .center_y(Fill)
            .into()
        } else {
            iced::widget::shader(VideoProgram {
                source: Arc::clone(&self.frame_source),
            })
            .width(Fill)
            .height(Fill)
            .into()
        };
        let layered =
            iced::widget::stack([Element::from(video), overlay_widget(&self.model.overlay)])
                .width(Fill)
                .height(Fill);
        container(
            column![
                header,
                container(layered)
                    .width(Fill)
                    .height(Fill)
                    .style(panel_style(tokens::MAIN))
            ]
            .spacing(tokens::SPACE_3)
            .height(Fill),
        )
        .height(Fill)
        .padding(tokens::SPACE_4)
        .style(panel_style(tokens::MAIN))
    }

    fn home_page(&self) -> iced::widget::Container<'_, Message> {
        let mut devices = column![
            text("Home").size(tokens::HEADER_SIZE),
            muted_text("Known devices running the Racc host agent")
        ]
        .spacing(tokens::SPACE_3);
        for device in &self.model.core.devices {
            let status = if device.online { "Online" } else { "Offline" };
            let details = format!(
                "{:?}  ·  {status}  ·  {} display(s)",
                device.os,
                device.displays.len()
            );
            let card = row![
                column![
                    text(device.name.clone())
                        .size(tokens::BODY_SIZE)
                        .color(if device.online {
                            tokens::TEXT
                        } else {
                            tokens::MUTED
                        }),
                    muted_text(&details)
                ]
                .spacing(tokens::SPACE_1),
                iced::widget::Space::new().width(Fill),
                action_button(
                    "Connect",
                    (device.online && device.host_capable)
                        .then(|| Message::Action(UserAction::SelectDevice(device.id.clone()))),
                    false
                )
            ]
            .align_y(iced::Alignment::Center);
            devices = devices.push(
                container(card)
                    .padding(tokens::SPACE_3)
                    .style(panel_style(tokens::CARD)),
            );
        }
        container(scrollable(devices).height(Fill).style(scroll_style))
            .height(Fill)
            .padding(tokens::SPACE_4)
            .style(panel_style(tokens::MAIN))
    }

    fn settings_page(&self) -> iced::widget::Container<'_, Message> {
        let hosting = self.model.core.hosting_enabled;
        let mut allowlist = column![section_label("ALLOWLIST")].spacing(tokens::SPACE_2);
        for device in &self.model.core.devices {
            let state = if device.host_capable {
                "approved"
            } else {
                "not approved"
            };
            let entry = row![
                muted_text(&format!("{}  ·  {:?}  ·  {state}", device.name, device.os)),
                iced::widget::Space::new().width(Fill),
                action_button(
                    "Remove",
                    device
                        .host_capable
                        .then(|| Message::Action(UserAction::RemovePeer(device.id.clone()))),
                    false,
                ),
            ]
            .align_y(iced::Alignment::Center);
            allowlist = allowlist.push(entry);
        }
        if let SessionOverlay::WaitingForApproval {
            device_id,
            device_name,
        } = &self.model.overlay
        {
            allowlist = allowlist.push(
                row![
                    text(format!("Pending: {device_name}")),
                    action_button(
                        "Approve",
                        Some(Message::Action(UserAction::ApprovePeer(device_id.clone()))),
                        false
                    ),
                    action_button(
                        "Reject",
                        Some(Message::Action(UserAction::RejectPeer(device_id.clone()))),
                        false
                    )
                ]
                .spacing(tokens::SPACE_2),
            );
        }
        container(
            scrollable(
                column![
                    text("Settings").size(tokens::HEADER_SIZE),
                    section_label("HOSTING"),
                    action_button(
                        if hosting {
                            "Hosting is on  ·  Turn off"
                        } else {
                            "Hosting is off  ·  Turn on"
                        },
                        Some(Message::Action(UserAction::SetHosting(!hosting))),
                        hosting
                    ),
                    quality_controls(None, self.model.default_quality, true),
                    allowlist,
                    section_label("ABOUT"),
                    muted_text("Racc Connect  ·  Rust and native wgpu shell  ·  fake mode"),
                ]
                .spacing(tokens::SPACE_3),
            )
            .height(Fill),
        )
        .height(Fill)
        .padding(tokens::SPACE_4)
        .style(panel_style(tokens::MAIN))
    }

    fn telemetry_sidebar(&self) -> iced::widget::Container<'_, Message> {
        if self.model.telemetry_sidebar_collapsed {
            return container(action_button(
                "‹",
                Some(Message::Action(UserAction::ToggleTelemetrySidebar)),
                false,
            ))
            .height(Fill)
            .padding(tokens::SPACE_2)
            .style(panel_style(tokens::SIDEBAR));
        }
        let session = self.model.core.telemetry.session;
        let host = self.model.core.telemetry.host;
        let rtt = session
            .last_rtt_us
            .map_or_else(|| "—".to_owned(), |us| format!("{} ms", us / 1_000));
        let loss = format!("{:.2}%", session.loss_fraction * 100.0);
        let bitrate = format!("{:.1} Mbps", session.bitrate_bps as f64 / 1_000_000.0);
        let mut events = column![row![
            section_label("EVENTS"),
            iced::widget::Space::new().width(Fill),
            action_button(
                "Collapse",
                Some(Message::Action(UserAction::ToggleTelemetrySidebar)),
                false
            )
        ],]
        .spacing(tokens::SPACE_1);
        if let Some(notification) = &self.model.notification {
            events = events.push(muted_text(notification));
        }
        for event in self
            .model
            .core
            .telemetry
            .events
            .events()
            .iter()
            .rev()
            .take(8)
        {
            events = events.push(muted_text(&format!(
                "{:?}  ·  {}",
                event.kind, event.detail
            )));
        }
        container(
            column![
                section_label("SESSION"),
                telemetry_line("State", format!("{:?}", session.connection_state)),
                telemetry_line("Path", format!("{:?}", session.path)),
                telemetry_line("RTT", rtt),
                telemetry_line("Packet loss", loss),
                telemetry_line("Bitrate", bitrate),
                telemetry_line("FPS", format!("{:.1}", session.fps)),
                telemetry_line("Codec", format!("{:?}", session.codec)),
                telemetry_line("Decoder", format!("{:?}", session.decoder)),
                section_label("HOST"),
                telemetry_line("CPU", format!("{:.1}%", host.cpu_pct_x10 as f32 / 10.0)),
                telemetry_line("Capture", format!("{:?}", host.capture_backend)),
                telemetry_line("Encoder", format!("{:?}", host.encoder)),
                telemetry_line("Resolution", format!("{}×{}", host.width, host.height)),
                telemetry_line(
                    "Refresh",
                    format!("{:.0} Hz", host.refresh_mhz as f32 / 1_000.0)
                ),
                scrollable(events).height(Fill).style(scroll_style),
            ]
            .spacing(tokens::SPACE_2)
            .height(Fill),
        )
        .height(Fill)
        .padding(tokens::SPACE_3)
        .style(panel_style(tokens::SIDEBAR))
    }

    fn print_measurement(&self) {
        let mut samples = self.frame_intervals_ms.clone();
        samples.sort_by(f64::total_cmp);
        if samples.is_empty() {
            println!("m4b_measurement seconds={} telemetry_collapsed={} fake_idle={} frame_samples=0 median_ms=unavailable p95_ms=unavailable over_40ms=0 presentation_note=frame-source-update-proxy-not-physical-presents", self.measurement_secs.unwrap_or_default(), self.model.telemetry_sidebar_collapsed, self.fake_idle);
            return;
        }
        let median = samples[samples.len() / 2];
        let p95 = samples[(samples.len() - 1) * 95 / 100];
        let over_40 = samples.iter().filter(|sample| **sample > 40.0).count();
        println!("m4b_measurement seconds={} telemetry_collapsed={} fake_idle={} frame_samples={} median_ms={median:.2} p95_ms={p95:.2} over_40ms={over_40} presentation_note=frame-source-update-proxy-not-physical-presents", self.measurement_secs.unwrap_or_default(), self.model.telemetry_sidebar_collapsed, self.fake_idle, samples.len());
    }
}

fn local_panel(model: &ViewModel) -> Element<'_, Message> {
    container(
        column![
            section_label("LOCAL SESSION"),
            text(model.core.local_device_name.clone()).size(tokens::BODY_SIZE),
            muted_text(if model.core.hosting_enabled {
                "Hosting  ·  on"
            } else {
                "Viewer  ·  connected"
            }),
            row![
                action_button(
                    "Keyboard",
                    Some(Message::Action(UserAction::SetKeyboardCapture(
                        !model.keyboard_capture
                    ))),
                    model.keyboard_capture
                ),
                action_button(
                    "Mouse",
                    Some(Message::Action(UserAction::SetMouseCapture(
                        !model.mouse_capture
                    ))),
                    model.mouse_capture
                ),
            ]
            .spacing(tokens::SPACE_1),
            tooltip(
                button(muted_text("Audio  ·  not supported"))
                    .padding(tokens::SPACE_2)
                    .style(|_theme: &Theme, _status| button::Style {
                        text_color: tokens::MUTED,
                        ..Default::default()
                    }),
                text("not supported"),
                iced::widget::tooltip::Position::Top,
            ),
            action_button(
                "Settings",
                Some(Message::Action(UserAction::Settings)),
                false
            ),
        ]
        .spacing(tokens::SPACE_1),
    )
    .padding(tokens::SPACE_2)
    .style(panel_style(tokens::CARD))
    .into()
}

fn quality_controls(
    device_id: Option<DeviceId>,
    selected_quality: QualityPreset,
    is_default: bool,
) -> Element<'static, Message> {
    let heading = if is_default {
        "DEFAULT QUALITY"
    } else {
        "QUALITY"
    };
    let mut controls = row![section_label(heading)]
        .spacing(tokens::SPACE_1)
        .align_y(iced::Alignment::Center);
    for (quality, label) in [
        (QualityPreset::P480, "480p"),
        (QualityPreset::P720, "720p"),
        (QualityPreset::P1080, "1080p"),
        (QualityPreset::Auto, "Auto"),
    ] {
        let action = if is_default {
            Some(Message::Action(UserAction::SetDefaultQuality(quality)))
        } else {
            device_id
                .as_ref()
                .map(|_| Message::Action(UserAction::SetQuality(quality)))
        };
        let selected = selected_quality == quality;
        controls = controls.push(action_button(label, action, selected));
    }
    controls.into()
}

fn overlay_widget(overlay: &SessionOverlay) -> Element<'static, Message> {
    let message = match overlay {
        SessionOverlay::None => return iced::widget::Space::new().width(Fill).height(Fill).into(),
        SessionOverlay::Connecting => "Connecting…".to_owned(),
        SessionOverlay::Switching => "Switching display  ·  holding the last frame".to_owned(),
        SessionOverlay::Paused => "Video paused while the window is hidden".to_owned(),
        SessionOverlay::Reconnecting => "Connection lost  ·  reconnecting".to_owned(),
        SessionOverlay::WaitingForApproval { device_name, .. } => {
            format!("Waiting for approval from {device_name}")
        }
        SessionOverlay::Error(detail) => format!("Stream error  ·  {detail}"),
    };
    container(text(message).size(tokens::BODY_SIZE))
        .padding(tokens::SPACE_3)
        .style(panel_style(tokens::CARD))
        .center_x(Fill)
        .center_y(Fill)
        .into()
}

fn telemetry_line(label: &str, value: String) -> Element<'static, Message> {
    row![
        muted_text(label),
        iced::widget::Space::new().width(Fill),
        text(value).size(tokens::BODY_SIZE)
    ]
    .align_y(iced::Alignment::Center)
    .into()
}

fn divider_label(label: &str) -> Element<'static, Message> {
    text(label.to_owned())
        .size(tokens::SECTION_SIZE)
        .color(tokens::MUTED)
        .into()
}
fn section_label(label: &str) -> Element<'static, Message> {
    text(label.to_owned())
        .size(tokens::SECTION_SIZE)
        .color(tokens::ACCENT)
        .into()
}
fn muted_text(label: &str) -> Element<'static, Message> {
    text(label.to_owned())
        .size(tokens::SECTION_SIZE)
        .color(tokens::MUTED)
        .into()
}
#[track_caller]
fn action_button(
    label: &str,
    action: Option<Message>,
    selected: bool,
) -> Element<'static, Message> {
    let caller = std::panic::Location::caller();
    let focus_id = format!(
        "{}:{}:{}:{label}:{action:?}",
        caller.file(),
        caller.line(),
        caller.column()
    );
    action_button_widget(text(label.to_owned()), action, selected, focus_id)
}

fn action_button_widget<'a>(
    content: impl Into<Element<'a, Message>>,
    action: Option<Message>,
    selected: bool,
    focus_id: String,
) -> Element<'a, Message> {
    let accessible_action = action.clone();
    FocusableButton::wrap(
        button(content)
            .padding(tokens::SPACE_2)
            .style(move |_theme: &Theme, status| {
                let background = match status {
                    button::Status::Hovered => tokens::CARD,
                    button::Status::Pressed => tokens::ACCENT,
                    _ if selected => tokens::SELECTED,
                    _ => Color::TRANSPARENT,
                };
                button::Style {
                    background: Some(Background::Color(background)),
                    text_color: if status == button::Status::Disabled {
                        tokens::MUTED
                    } else {
                        tokens::TEXT
                    },
                    border: Border {
                        color: tokens::BORDER,
                        width: tokens::BORDER_WIDTH,
                        radius: if selected {
                            tokens::RADIUS_MEDIUM
                        } else {
                            tokens::RADIUS_SMALL
                        }
                        .into(),
                    },
                    ..Default::default()
                }
            })
            .on_press_maybe(action),
        accessible_action,
        iced::advanced::widget::Id::from(focus_id),
    )
    .into()
}

fn scroll_style(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let mut style = scrollable::default(theme, status);
    for rail in [&mut style.vertical_rail, &mut style.horizontal_rail] {
        rail.background = Some(Background::Color(tokens::MAIN));
        rail.border.color = tokens::BORDER;
        rail.scroller.background = Background::Color(tokens::MUTED);
        rail.scroller.border.color = tokens::BORDER;
    }
    style
}

fn panel_style(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_theme| container::Style {
        background: Some(Background::Color(color)),
        text_color: Some(tokens::TEXT),
        border: Border {
            color: tokens::BORDER,
            width: tokens::BORDER_WIDTH,
            radius: tokens::RADIUS_LARGE.into(),
        },
        ..Default::default()
    }
}

struct VideoProgram {
    source: Arc<dyn FrameSource>,
}
impl std::fmt::Debug for VideoProgram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VideoProgram")
    }
}
impl iced::widget::shader::Program<Message> for VideoProgram {
    type State = ();
    type Primitive = VideoPrimitive;
    fn draw(&self, _state: &Self::State, _cursor: Cursor, _bounds: Rectangle) -> Self::Primitive {
        VideoPrimitive {
            source: Arc::clone(&self.source),
        }
    }
}

struct VideoPrimitive {
    source: Arc<dyn FrameSource>,
}
impl std::fmt::Debug for VideoPrimitive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VideoPrimitive")
    }
}
impl iced::widget::shader::Primitive for VideoPrimitive {
    type Pipeline = VideoPipeline;
    fn prepare(
        &self,
        pipeline: &mut Self::Pipeline,
        _device: &iced::wgpu::Device,
        queue: &iced::wgpu::Queue,
        bounds: &Rectangle,
        _viewport: &Viewport,
    ) {
        let frame = self.source.latest_frame();
        let (width, height, frame_number, seed) =
            frame.as_ref().map_or((1_280, 720, 0_u64, 0_u64), |frame| {
                let (number, seed) = match &frame.payload {
                    FramePayload::SyntheticPattern { seed, frame_number } => (*frame_number, *seed),
                    FramePayload::Nv12(_) => (u64::from(frame.frame_id), 0),
                };
                (
                    u32::from(frame.width),
                    u32::from(frame.height),
                    number,
                    seed,
                )
            });
        let panel_width = bounds.width.round().max(1.0) as u32;
        let panel_height = bounds.height.round().max(1.0) as u32;
        let content = design::video_rect(panel_width, panel_height, width.max(1), height.max(1))
            .ok()
            .flatten();
        let (x, y, content_width, content_height) = content.map_or(
            (0.0, 0.0, panel_width as f32, panel_height as f32),
            |rect| {
                let (x, y) = rect.origin();
                let (width, height) = rect.size();
                (x as f32, y as f32, width as f32, height as f32)
            },
        );
        let values = [
            bounds.width.to_bits(),
            bounds.height.to_bits(),
            frame_number as u32,
            seed as u32,
            x.to_bits(),
            y.to_bits(),
            content_width.to_bits(),
            content_height.to_bits(),
            0,
            0,
            0,
            0,
        ];
        let mut bytes = [0_u8; 48];
        for (index, value) in values.iter().enumerate() {
            bytes[index * 4..(index + 1) * 4].copy_from_slice(&value.to_ne_bytes());
        }
        queue.write_buffer(&pipeline.uniform_buffer, 0, &bytes);
    }
    fn draw(&self, pipeline: &Self::Pipeline, pass: &mut iced::wgpu::RenderPass<'_>) -> bool {
        pass.set_pipeline(&pipeline.render_pipeline);
        pass.set_bind_group(0, &pipeline.bind_group, &[]);
        pass.draw(0..3, 0..1);
        true
    }
}

struct VideoPipeline {
    render_pipeline: iced::wgpu::RenderPipeline,
    bind_group: iced::wgpu::BindGroup,
    uniform_buffer: iced::wgpu::Buffer,
}
impl iced::widget::shader::Pipeline for VideoPipeline {
    fn new(
        device: &iced::wgpu::Device,
        _queue: &iced::wgpu::Queue,
        format: iced::wgpu::TextureFormat,
    ) -> Self {
        let uniform_buffer = device.create_buffer(&iced::wgpu::BufferDescriptor {
            label: Some("synthetic video metadata"),
            size: 48,
            usage: iced::wgpu::BufferUsages::UNIFORM | iced::wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&iced::wgpu::BindGroupLayoutDescriptor {
            label: Some("synthetic video uniforms"),
            entries: &[iced::wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: iced::wgpu::ShaderStages::FRAGMENT,
                ty: iced::wgpu::BindingType::Buffer {
                    ty: iced::wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&iced::wgpu::BindGroupDescriptor {
            label: Some("synthetic video bind group"),
            layout: &layout,
            entries: &[iced::wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });
        let shader = device.create_shader_module(iced::wgpu::ShaderModuleDescriptor {
            label: Some("synthetic video shader"),
            source: iced::wgpu::ShaderSource::Wgsl(VIDEO_SHADER.into()),
        });
        let pipeline_layout =
            device.create_pipeline_layout(&iced::wgpu::PipelineLayoutDescriptor {
                label: Some("video layout"),
                bind_group_layouts: &[&layout],
                push_constant_ranges: &[],
            });
        let render_pipeline =
            device.create_render_pipeline(&iced::wgpu::RenderPipelineDescriptor {
                label: Some("native synthetic video surface"),
                layout: Some(&pipeline_layout),
                vertex: iced::wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(iced::wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(iced::wgpu::ColorTargetState {
                        format,
                        blend: Some(iced::wgpu::BlendState::REPLACE),
                        write_mask: iced::wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: iced::wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: iced::wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            });
        Self {
            render_pipeline,
            bind_group,
            uniform_buffer,
        }
    }
}

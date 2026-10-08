//! Explicit enumeration and bounded capture probes for Windows Desktop Duplication.
//!
//! `--list` reads display metadata only. Capture requires a visible-screen acknowledgement;
//! capture modes acquire GPU frames but never read pixels back or write image files.

#[cfg(windows)]
mod windows_probe {
    use std::time::{Duration, Instant};

    use racc_capture::windows::WindowsCaptureBackend;
    use racc_capture::{CaptureBackend, CaptureEvent, CaptureParams, CursorShape, GpuFrame};
    use racc_topology::DisplayId;

    const MAX_SAMPLE_VALUES: usize = 120_000;

    pub fn run() -> Result<(), String> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        match args.first().map(String::as_str) {
            Some("--list") if args.len() == 1 => list_displays(),
            Some("--capture") if args.len() == 5 => run_capture(&args),
            Some("--sample") if args.len() == 5 || args.len() == 7 => run_sample(&args),
            _ => Err(usage()),
        }
    }

    fn list_displays() -> Result<(), String> {
        let mut backend = WindowsCaptureBackend::new();
        let displays = backend.enumerate_displays().map_err(|e| e.to_string())?;
        println!(
            "{:<10} {:<32} {:>17} {:>12} {:>15} {:>12} {:<24}",
            "DISPLAY_ID",
            "ADAPTER_MODEL",
            "ORIGIN_XY",
            "SIZE_PX",
            "REFRESH_MHZ",
            "SCALE_MILLI",
            "IDENTITY_SOURCE"
        );
        for entry in displays {
            let display = entry.display;
            let (x, y) = display.origin();
            let (width, height) = display.size();
            println!(
                "{:<10} {:<32} ({:>6},{:>6}) {:>5}x{:<5} {:>15} {:>12} {:?}",
                display.id().get(),
                truncate(&entry.adapter_model, 32),
                x,
                y,
                width,
                height,
                display.refresh_mhz(),
                display.scale_milli(),
                entry.identity_source,
            );
        }
        Ok(())
    }

    fn run_capture(args: &[String]) -> Result<(), String> {
        if args[2] != "--frames" || args[4] != "--acknowledge-visible-screen" {
            return Err(usage());
        }
        let display_id = parse_display_id(&args[1])?;
        let frame_limit = args[3]
            .parse::<u16>()
            .map_err(|_| "frame count must be an integer from 1 to 300".to_owned())?;
        if !(1..=300).contains(&frame_limit) {
            return Err("frame count must be from 1 to 300".to_owned());
        }
        acknowledge_capture();
        let mut backend = WindowsCaptureBackend::new();
        ensure_display(&mut backend, display_id)?;
        backend
            .start(display_id, CaptureParams::default())
            .map_err(|e| e.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut frames = 0u16;
        let mut cursor_shapes = 0u64;
        let mut cursor_positions = 0u64;
        println!("FRAME CAPTURE_TS_US ACQUIRE_WAIT_US COPY_SCALE_SUBMIT_US INTERVAL_US DAMAGE CURSOR_SHAPES_TOTAL CURSOR_POSITIONS_TOTAL DROPPED_FRAMES_TOTAL");
        while frames < frame_limit && Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match backend
                .poll_event(remaining.min(Duration::from_millis(16)))
                .map_err(|e| e.to_string())?
            {
                Some(CaptureEvent::Frame(frame)) => {
                    frames += 1;
                    print_frame_row(frames, &frame, cursor_shapes, cursor_positions);
                }
                Some(CaptureEvent::CursorShape(shape)) => {
                    cursor_shapes = cursor_shapes.saturating_add(1);
                    print_cursor_shape_metadata(&shape);
                }
                Some(CaptureEvent::CursorMoved(_position)) => {
                    cursor_positions = cursor_positions.saturating_add(1)
                }
                Some(CaptureEvent::AccessLost(reason)) => {
                    backend.stop();
                    return Err(format!("capture access lost: {reason:?}"));
                }
                Some(CaptureEvent::DisplayLost) => {
                    backend.stop();
                    return Err("selected display disappeared".to_owned());
                }
                Some(CaptureEvent::DeviceLost) => {
                    backend.stop();
                    return Err("D3D device was lost".to_owned());
                }
                Some(CaptureEvent::Error(kind)) => {
                    backend.stop();
                    return Err(format!("capture failed: {kind:?}"));
                }
                Some(_) | None => {}
            }
        }
        let dropped = backend.dropped_frames();
        backend.stop();
        if frames != frame_limit {
            return Err(format!(
                "received {frames} of {frame_limit} requested frames within 30 seconds; cumulative dropped frames={dropped}"
            ));
        }
        println!("received {frames} GPU frames; cumulative dropped frames={dropped}; no image data was read back or saved");
        Ok(())
    }

    fn run_sample(args: &[String]) -> Result<(), String> {
        if args[2] != "--seconds" || args[4] != "--acknowledge-visible-screen" {
            return Err(usage());
        }
        let display_id = parse_display_id(&args[1])?;
        let seconds = args[3]
            .parse::<u8>()
            .map_err(|_| "sample duration must be an integer from 1 to 60 seconds".to_owned())?;
        if !(1..=60).contains(&seconds) {
            return Err("sample duration must be from 1 to 60 seconds".to_owned());
        }
        let migrate_to = if args.len() == 7 {
            if args[5] != "--migrate-to" {
                return Err(usage());
            }
            let id = parse_display_id(&args[6])?;
            if id == display_id {
                return Err("migration target must differ from the sampled display".to_owned());
            }
            Some(id)
        } else {
            None
        };

        acknowledge_capture();
        let mut backend = WindowsCaptureBackend::new();
        let displays = backend.enumerate_displays().map_err(|e| e.to_string())?;
        if !displays
            .iter()
            .any(|entry| entry.display.id() == display_id)
        {
            return Err(format!("display id {} is not available", display_id.get()));
        }
        if let Some(target) = migrate_to {
            if !displays.iter().any(|entry| entry.display.id() == target) {
                return Err(format!(
                    "migration target {} is not available",
                    target.get()
                ));
            }
        }
        backend
            .start(display_id, CaptureParams::default())
            .map_err(|e| e.to_string())?;

        let sample_started = Instant::now();
        let deadline = sample_started + Duration::from_secs(u64::from(seconds));
        let mut frame_count = 0u64;
        let mut cursor_shapes = 0u64;
        let mut cursor_positions = 0u64;
        let mut dropped_frames = 0u64;
        let mut acquire_waits = Vec::with_capacity(4096);
        let mut copy_scale_submissions = Vec::with_capacity(4096);
        let mut frame_intervals = Vec::with_capacity(4096);
        let mut truncated_samples = false;

        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match backend
                .poll_event(remaining.min(Duration::from_millis(16)))
                .map_err(|e| e.to_string())?
            {
                Some(CaptureEvent::Frame(frame)) => {
                    frame_count = frame_count.saturating_add(1);
                    push_sample(
                        &mut acquire_waits,
                        frame.acquire_wait_us,
                        &mut truncated_samples,
                    );
                    push_sample(
                        &mut copy_scale_submissions,
                        frame.copy_scale_submit_us,
                        &mut truncated_samples,
                    );
                    push_sample(
                        &mut frame_intervals,
                        frame.frame_interval_us,
                        &mut truncated_samples,
                    );
                    dropped_frames = dropped_frames.max(frame.dropped_frames);
                }
                Some(CaptureEvent::CursorShape(shape)) => {
                    cursor_shapes = cursor_shapes.saturating_add(1);
                    print_cursor_shape_metadata(&shape);
                }
                Some(CaptureEvent::CursorMoved(_position)) => {
                    cursor_positions = cursor_positions.saturating_add(1)
                }
                Some(CaptureEvent::AccessLost(reason)) => {
                    backend.stop();
                    return Err(format!("capture access lost: {reason:?}"));
                }
                Some(CaptureEvent::DisplayLost) => {
                    backend.stop();
                    return Err("selected display disappeared".to_owned());
                }
                Some(CaptureEvent::DeviceLost) => {
                    backend.stop();
                    return Err("D3D device was lost".to_owned());
                }
                Some(CaptureEvent::Error(kind)) => {
                    backend.stop();
                    return Err(format!("capture failed: {kind:?}"));
                }
                Some(_) | None => {}
            }
        }
        let elapsed_us = duration_micros(sample_started.elapsed());
        dropped_frames = dropped_frames.max(backend.dropped_frames());
        println!(
            "sample display_id={} requested_seconds={} elapsed_us={} frames={} cursor_shapes={} cursor_positions={} dropped_frames={}{}",
            display_id.get(),
            seconds,
            elapsed_us,
            frame_count,
            cursor_shapes,
            cursor_positions,
            dropped_frames,
            if truncated_samples { " samples_truncated=true" } else { "" },
        );
        print_percentiles("acquire_wait_us", &mut acquire_waits);
        print_percentiles("copy_scale_submit_us", &mut copy_scale_submissions);
        print_percentiles("frame_interval_us", &mut frame_intervals);

        if let Some(target) = migrate_to {
            let migration_started = Instant::now();
            if let Err(error) = backend.migrate(target) {
                backend.stop();
                return Err(format!("post-sample migration failed: {error}"));
            }
            println!(
                "migration target_display_id={} elapsed_us={}",
                target.get(),
                duration_micros(migration_started.elapsed())
            );
        }
        backend.stop();
        Ok(())
    }

    fn ensure_display(backend: &mut WindowsCaptureBackend, id: DisplayId) -> Result<(), String> {
        if backend
            .enumerate_displays()
            .map_err(|e| e.to_string())?
            .iter()
            .any(|entry| entry.display.id() == id)
        {
            Ok(())
        } else {
            Err(format!("display id {} is not available", id.get()))
        }
    }

    fn parse_display_id(value: &str) -> Result<DisplayId, String> {
        let raw = value
            .parse::<u32>()
            .map_err(|_| "display id must be a nonzero integer".to_owned())?;
        DisplayId::new(raw).ok_or_else(|| "display id must be nonzero".to_owned())
    }

    fn acknowledge_capture() {
        eprintln!("Capture opt-in: this reads visible desktop frames into GPU memory. Do not run on lock/UAC screens or while displaying private content. No image is saved.");
    }

    fn push_sample(values: &mut Vec<u64>, value: u64, truncated: &mut bool) {
        if values.len() < MAX_SAMPLE_VALUES {
            values.push(value);
        } else {
            *truncated = true;
        }
    }

    fn print_percentiles(name: &str, values: &mut [u64]) {
        let p50 = percentile_us(values, 50);
        let p95 = percentile_us(values, 95);
        match (p50, p95) {
            (Some(p50), Some(p95)) => {
                println!(
                    "metric={name} samples={} p50_us={p50} p95_us={p95}",
                    values.len()
                );
            }
            _ => println!("metric={name} samples=0 p50_us=NA p95_us=NA"),
        }
    }

    fn percentile_us(values: &mut [u64], percentile: usize) -> Option<u64> {
        if values.is_empty() {
            return None;
        }
        values.sort_unstable();
        let rank = (values.len() * percentile).div_ceil(100).max(1);
        values.get(rank - 1).copied()
    }

    fn print_frame_row(
        frame_index: u16,
        frame: &GpuFrame,
        cursor_shapes: u64,
        cursor_positions: u64,
    ) {
        println!(
            "{} {} {} {} {} {:?} {} {} {}",
            frame_index,
            frame.capture_ts_us,
            frame.acquire_wait_us,
            frame.copy_scale_submit_us,
            frame.frame_interval_us,
            frame.damage,
            cursor_shapes,
            cursor_positions,
            frame.dropped_frames,
        );
    }

    fn print_cursor_shape_metadata(shape: &CursorShape) {
        println!(
            "cursor_shape width={} height={} hotspot_x={} hotspot_y={} bgra8_len={} blend_mode={:?}",
            shape.width,
            shape.height,
            shape.hotspot_x,
            shape.hotspot_y,
            shape.bgra8.len(),
            shape.blend_mode,
        );
    }

    fn duration_micros(duration: Duration) -> u64 {
        duration.as_micros().min(u128::from(u64::MAX)) as u64
    }

    fn truncate(value: &str, max_chars: usize) -> String {
        value.chars().take(max_chars).collect()
    }

    fn usage() -> String {
        "usage: capture_probe --list | --capture DISPLAY_ID --frames N --acknowledge-visible-screen (N: 1..=300) | --sample DISPLAY_ID --seconds N --acknowledge-visible-screen [--migrate-to DISPLAY_ID] (N: 1..=60)".to_owned()
    }

    #[cfg(test)]
    mod tests {
        use super::percentile_us;

        #[test]
        fn percentile_uses_nearest_rank_and_handles_empty_samples() {
            assert_eq!(percentile_us(&mut [], 50), None);
            let mut samples = [5, 1, 4, 2, 3];
            assert_eq!(percentile_us(&mut samples, 50), Some(3));
            assert_eq!(percentile_us(&mut samples, 95), Some(5));
            let mut single = [42];
            assert_eq!(percentile_us(&mut single, 95), Some(42));
        }
    }
}

#[cfg(windows)]
fn run() -> Result<(), String> {
    windows_probe::run()
}

#[cfg(not(windows))]
fn run() -> Result<(), String> {
    Err("capture_probe is available only on Windows".to_owned())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(2);
    }
}

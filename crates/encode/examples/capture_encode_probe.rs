//! Human-invoked visible capture→NV12→Media Foundation→Annex B probe.

use std::path::PathBuf;
use std::process::ExitCode;

#[cfg(windows)]
use racc_encode::{EncodeError, EncoderConfig};

#[cfg(windows)]
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("capture encode probe failed: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn main() -> ExitCode {
    eprintln!("capture encode probe is available only on Windows");
    ExitCode::FAILURE
}

#[cfg(windows)]
fn run() -> Result<(), EncodeError> {
    let mut output = PathBuf::from("capture-encode-probe.h264");
    let mut display_id = None;
    let mut frames = 90u32;
    let mut width = 1280u32;
    let mut height = 720u32;
    let mut bitrate = 4_000_000u32;
    let mut acknowledged = false;
    let mut args = std::env::args_os().skip(1);
    while let Some(argument) = args.next() {
        if argument == "--acknowledge-visible-screen" {
            acknowledged = true;
            continue;
        }
        let value = args.next().ok_or_else(|| {
            EncodeError::InvalidConfig(format!("missing value for {}", argument.to_string_lossy()))
        })?;
        match argument.to_string_lossy().as_ref() {
            "--display" => {
                let raw = parse_u32(&value, "display id")?;
                display_id = racc_topology::DisplayId::new(raw);
                if display_id.is_none() {
                    return Err(EncodeError::InvalidConfig(
                        "display id must be nonzero".to_owned(),
                    ));
                }
            }
            "--output" => output = PathBuf::from(value),
            "--frames" => frames = parse_u32(&value, "frame count")?,
            "--width" => width = parse_u32(&value, "width")?,
            "--height" => height = parse_u32(&value, "height")?,
            "--bitrate" => bitrate = parse_u32(&value, "bitrate")?,
            unknown => {
                return Err(EncodeError::InvalidConfig(format!(
                    "unknown argument {unknown}"
                )));
            }
        }
    }
    if !acknowledged {
        return Err(EncodeError::InvalidConfig(
            "visible capture requires --acknowledge-visible-screen".to_owned(),
        ));
    }
    let display_id = display_id.ok_or_else(|| {
        EncodeError::InvalidConfig(
            "--display <id> is required; list displays with capture_probe --list".to_owned(),
        )
    })?;
    if !(1..=1800).contains(&frames) {
        return Err(EncodeError::InvalidConfig(
            "frame count must be from 1 to 1800 (at most 60 seconds)".to_owned(),
        ));
    }
    let config = EncoderConfig::new(width, height, bitrate)?;
    eprintln!("Visible-screen opt-in: this reads live desktop frames into GPU memory and writes an H.264 video file. Do not run on lock/UAC screens or while private content is visible.");
    let report = racc_encode::windows_capture_probe::run_capture_encode_probe(
        &output, display_id, config, frames,
    )?;
    println!(
        "backend={:?} output={} resolution={}x{} fps={} frames={}/{} bytes={}",
        report.backend,
        report.path.display(),
        report.width,
        report.height,
        report.fps,
        report.frames_encoded,
        report.frames_requested,
        report.bytes_written
    );
    Ok(())
}

#[cfg(windows)]
fn parse_u32(value: &std::ffi::OsStr, label: &str) -> Result<u32, EncodeError> {
    value
        .to_string_lossy()
        .parse()
        .map_err(|_| EncodeError::InvalidConfig(format!("invalid {label}")))
}

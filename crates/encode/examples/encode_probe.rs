use std::path::PathBuf;
use std::process::ExitCode;

use racc_encode::probe::run_openh264_synthetic_probe;
use racc_encode::{EncodeError, EncoderConfig};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("encode probe failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), EncodeError> {
    let mut output = PathBuf::from("encode-probe.h264");
    let mut backend = String::from("openh264");
    let mut width = 1280u32;
    let mut height = 720u32;
    let mut frames = 90u32;
    let mut bitrate = 4_000_000u32;
    let mut args = std::env::args_os().skip(1);
    while let Some(argument) = args.next() {
        let value = args.next().ok_or_else(|| {
            EncodeError::InvalidConfig(format!("missing value for {}", argument.to_string_lossy()))
        })?;
        match argument.to_string_lossy().as_ref() {
            "--output" => output = PathBuf::from(value),
            "--backend" => backend = value.to_string_lossy().into_owned(),
            "--width" => width = parse_u32(&value, "width")?,
            "--height" => height = parse_u32(&value, "height")?,
            "--frames" => frames = parse_u32(&value, "frames")?,
            "--bitrate" => bitrate = parse_u32(&value, "bitrate")?,
            unknown => {
                return Err(EncodeError::InvalidConfig(format!(
                    "unknown argument {unknown}"
                )));
            }
        }
    }
    let config = EncoderConfig::new(width, height, bitrate)?;
    let report = match backend.as_str() {
        "openh264" => run_openh264_synthetic_probe(&output, config, frames)?,
        "mf" => {
            #[cfg(windows)]
            {
                racc_encode::windows::run_mf_synthetic_probe(&output, config, frames)?
            }
            #[cfg(not(windows))]
            {
                return Err(EncodeError::InvalidConfig(
                    "--backend mf is available only on Windows".to_owned(),
                ));
            }
        }
        _ => {
            return Err(EncodeError::InvalidConfig(
                "--backend must be openh264 or mf".to_owned(),
            ));
        }
    };
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
    if report.used_fallback {
        println!(
            "fallback_reason={}",
            report.fallback_reason.as_deref().unwrap_or("unspecified")
        );
    }
    Ok(())
}

fn parse_u32(value: &std::ffi::OsStr, label: &str) -> Result<u32, EncodeError> {
    value
        .to_string_lossy()
        .parse()
        .map_err(|_| EncodeError::InvalidConfig(format!("invalid {label}")))
}

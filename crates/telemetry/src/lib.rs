//! Bounded telemetry values and immutable snapshots.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod events;
mod hub;
mod rates;
mod rtt;

pub use events::{
    Event, EventKind, EventLog, EventLogError, EventLogSnapshot, EVENT_DETAIL_MAX_BYTES,
    EVENT_LOG_CAPACITY,
};
pub use hub::{
    CaptureBackendKind, CodecKind, ConnectionState, DecoderKind, EncoderKind, HostSnapshot,
    PathKind, SessionSnapshot, TelemetryError, TelemetryHub, TelemetrySnapshot,
};
pub use rates::{
    RateSnapshot, RateWindow, RateWindowError, DEFAULT_RATE_BUCKETS, DEFAULT_RATE_BUCKET_WIDTH_US,
    MAX_RATE_BUCKETS,
};
pub use rtt::{
    PingRecordOutcome, PingTracker, PingTrackerSnapshot, RttEstimator, RttEstimatorError,
    RttSnapshot, DEFAULT_RTT_MIN_WINDOW_US, MAX_OUTSTANDING_PINGS, MAX_RTT_SAMPLES,
};

#[cfg(test)]
mod property_tests;

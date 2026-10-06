//! Deterministic network impairment and real loopback helpers for transport tests.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod impairment;
#[cfg(test)]
mod loopback;
mod proxy;
pub mod sim;
mod soak;

#[cfg(test)]
pub(crate) static TIMING_SENSITIVE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub use impairment::{
    DeliveredDatagram, ImpairmentProfile, ImpairmentStats, SimulatedNetwork,
    DEFAULT_SIMULATION_QUEUE_BYTES, PROBABILITY_SCALE,
};
pub use proxy::{LoopbackUdpProxy, ProxyStats};
pub use soak::{
    run_stream_soak, run_stream_soak_with_options, run_stream_soak_with_trace,
    run_stream_soak_with_trace_and_options, StreamSoakMetrics, StreamSoakOptions, StreamTier,
};

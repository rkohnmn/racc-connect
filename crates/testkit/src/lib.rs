//! Deterministic network impairment and real loopback helpers for transport tests.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod impairment;
#[cfg(test)]
mod loopback;
mod proxy;
mod soak;

#[cfg(test)]
pub(crate) static TIMING_SENSITIVE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub use impairment::{
    DeliveredDatagram, ImpairmentProfile, ImpairmentStats, SimulatedNetwork,
    DEFAULT_SIMULATION_QUEUE_BYTES, PROBABILITY_SCALE,
};
pub use proxy::{LoopbackUdpProxy, ProxyStats};
pub use soak::{run_stream_soak, StreamSoakMetrics, StreamTier};

//! Raw Windows service and session-helper bindings for `racc-host-agent`.
//!
//! All unsafe Windows API calls and handle ownership stay in this module.

pub mod local_ipc;
mod service;
mod system_metrics;

pub use local_ipc::{
    connect_local_ipc_client, connect_local_ipc_transport, local_pipe_security_descriptor_sddl,
    run_local_ipc_server, LOCAL_IPC_PIPE,
};
pub use service::{run_service_dispatcher, start_stop_event_listener};
pub use system_metrics::SystemCpuSampler;

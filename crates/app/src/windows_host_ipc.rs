//! App-side Windows client for the host-agent's local named pipe.
//!
//! Keep the pipe name aligned with the listener in
//! crates/host-agent/src/windows/local_ipc.rs.

use racc_core::ipc::IpcClient;
use std::fs::{File, OpenOptions};
use std::io;

/// Local helper pipe shared with the Windows host-agent.
pub const LOCAL_IPC_PIPE: &str = r"\\.\pipe\racc-connect-host";

/// Opens the local pipe from the UI process; callers must do this off the UI thread.
pub fn connect_local_host_agent() -> io::Result<IpcClient<File>> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(LOCAL_IPC_PIPE)
        .map(IpcClient::new)
}

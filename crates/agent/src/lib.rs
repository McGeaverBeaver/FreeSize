//! The device side of the FreeSize Hub, shared by the command-line agent and
//! the app: pairing, reports, polling for commands, and deciding on them.

pub mod client;
pub mod commands;
pub mod estimate;
pub mod guard;
#[cfg(feature = "install")]
pub mod install;
pub mod quarantine;
pub mod report;
pub mod state;
pub mod transport;
pub mod update;

pub use client::{AgentError, AgentResult, Client, hello, move_to, pair};
pub use state::{AgentState, Approver};
#[cfg(feature = "ureq")]
pub use transport::UreqTransport;
pub use transport::{Transport, Trust};

/// Seconds since 1970.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The host name, for naming a device.
pub fn hostname() -> String {
    if let Ok(name) = std::env::var("COMPUTERNAME")
        && !name.is_empty()
    {
        return name;
    }
    if let Ok(name) = std::fs::read_to_string("/etc/hostname")
        && !name.trim().is_empty()
    {
        return name.trim().to_string();
    }
    std::env::var("HOSTNAME").unwrap_or_else(|_| "this computer".into())
}

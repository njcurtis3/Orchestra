//! The host side of the link between a host and its phones, in the core so the desktop and
//! `orchestrad` serve the same one. Nothing listens until a caller starts it with at least
//! one explicit address. The pieces: Noise handshakes (`noise`), message framing (`frame`),
//! key material (`keys`), the paired clients (`clients`), the pairing window (`pairing`),
//! the listener (`server`) and one thread per connection (`conn`). See
//! `protocol/README.md` "What P3 decided".

pub mod addr;
pub mod clients;
mod conn;
pub mod frame;
pub mod keys;
pub mod noise;
pub mod pairing;
pub mod server;

#[cfg(test)]
mod vectors;

pub use server::{
    BindError, LinkCallbacks, LinkError, LinkHost, LinkLog, PendingPairing, validate_binds,
};

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use crate::session::store::StorageStatus;

/// Caps and timeouts. The defaults are the product's; a test shortens them.
#[derive(Debug, Clone)]
pub struct LinkLimits {
    /// Open connections, handshaking or established.
    pub max_connections: usize,
    /// Handshake attempts per source address per minute.
    pub handshakes_per_minute: u32,
    /// In-flight calls per connection; one more is answered `failed` ("busy").
    pub max_calls_per_connection: usize,
    /// Events a connection may fall behind before it is closed.
    pub event_queue: usize,
    /// Closed when nothing arrives for this long. Clients ping within 60 seconds.
    pub idle_timeout: Duration,
    /// Closed when a write is stuck for this long.
    pub write_timeout: Duration,
    /// How long a client may take to finish the version exchange and the handshake.
    pub handshake_timeout: Duration,
    /// How long the host has to confirm or reject a pairing request.
    pub confirm_timeout: Duration,
    /// How long a pairing window stays open.
    pub pairing_ttl: Duration,
}

impl Default for LinkLimits {
    fn default() -> Self {
        Self {
            max_connections: 16,
            handshakes_per_minute: 10,
            max_calls_per_connection: 8,
            event_queue: 1024,
            idle_timeout: Duration::from_secs(120),
            write_timeout: Duration::from_secs(30),
            handshake_timeout: Duration::from_secs(10),
            confirm_timeout: pairing::PAIRING_TTL,
            pairing_ttl: pairing::PAIRING_TTL,
        }
    }
}

/// What a host needs to serve the link. No secret lives in it: the host key comes from a
/// `KeyProvider`.
#[derive(Debug, Clone)]
pub struct LinkConfig {
    /// Every address to listen on, explicitly. Empty means off: nothing is opened.
    pub binds: Vec<SocketAddr>,
    /// Shown to a phone in `welcome` and in the pairing offer.
    pub host_name: String,
    /// Holds `host_key` (with the default provider) and `clients.json`.
    pub link_dir: PathBuf,
    /// Sent in each snapshot and returned by `storage_status`.
    pub storage: StorageStatus,
    pub limits: LinkLimits,
}

impl LinkConfig {
    /// Off until `binds` is filled.
    pub fn new(host_name: impl Into<String>, link_dir: impl Into<PathBuf>, storage: StorageStatus) -> Self {
        Self {
            binds: Vec::new(),
            host_name: host_name.into(),
            link_dir: link_dir.into(),
            storage,
            limits: LinkLimits::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_config_listens_on_nothing() {
        let c = LinkConfig::new("Desk", "link", StorageStatus::ok());
        assert!(c.binds.is_empty());
        assert_eq!(c.limits.max_connections, 16);
        assert_eq!(c.limits.idle_timeout, Duration::from_secs(120));
    }
}

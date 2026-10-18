//! The pairing window and the offer a QR carries. A window holds one random 32 byte secret,
//! which is the IKpsk2 psk. It lasts five minutes and is single use: it closes when a client
//! proves it knows the secret, which is when that client's first transport message decrypts.

use std::fmt;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::keys::{KeyError, Psk, PublicKey, to_hex};
use crate::session::now_ms;

pub const PAIRING_TTL: Duration = Duration::from_secs(300);
pub const OFFER_VERSION: u64 = 1;

pub struct PairingWindow {
    secret: Psk,
    opened: Instant,
    ttl: Duration,
    used: bool,
}

impl PairingWindow {
    pub fn open(ttl: Duration) -> Result<Self, KeyError> {
        Self::open_at(Instant::now(), ttl)
    }

    pub fn open_at(now: Instant, ttl: Duration) -> Result<Self, KeyError> {
        Ok(Self { secret: Psk::generate()?, opened: now, ttl, used: false })
    }

    pub fn is_open_at(&self, now: Instant) -> bool {
        !self.used && now.saturating_duration_since(self.opened) < self.ttl
    }

    pub fn is_open(&self) -> bool {
        self.is_open_at(Instant::now())
    }

    pub fn remaining_at(&self, now: Instant) -> Duration {
        if self.used {
            return Duration::ZERO;
        }
        self.ttl.saturating_sub(now.saturating_duration_since(self.opened))
    }

    /// A copy of the psk for one handshake, or `None` once the window has closed.
    pub fn psk_at(&self, now: Instant) -> Option<Psk> {
        self.is_open_at(now).then(|| Psk::from_bytes(*self.secret.expose()))
    }

    pub fn psk(&self) -> Option<Psk> {
        self.psk_at(Instant::now())
    }

    /// Close the window for good. True for the one caller that found it open.
    pub fn consume_at(&mut self, now: Instant) -> bool {
        if !self.is_open_at(now) {
            return false;
        }
        self.used = true;
        true
    }

    pub fn consume(&mut self) -> bool {
        self.consume_at(Instant::now())
    }

    pub fn offer(&self, host_name: &str, host_key: &PublicKey, addrs: &[SocketAddr]) -> PairingOffer {
        let remaining = self.remaining_at(Instant::now());
        PairingOffer {
            v: OFFER_VERSION,
            host_id: host_key.host_id(),
            host_name: host_name.to_string(),
            addrs: addrs.iter().map(SocketAddr::to_string).collect(),
            host_key: host_key.to_hex(),
            secret: to_hex(self.secret.expose()),
            expires_at: now_ms() + remaining.as_millis() as u64,
        }
    }
}

impl fmt::Debug for PairingWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairingWindow").field("open", &self.is_open()).finish_non_exhaustive()
    }
}

/// What the QR encodes, as JSON. It carries the secret, so `Debug` hides it; show it to the
/// user once, in the QR, and nowhere else.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingOffer {
    pub v: u64,
    pub host_id: String,
    pub host_name: String,
    /// `ip:port` each.
    pub addrs: Vec<String>,
    /// The host's static public key, hex.
    pub host_key: String,
    /// The 32 byte pairing secret, hex.
    pub secret: String,
    /// Milliseconds since the Unix epoch.
    pub expires_at: u64,
}

impl PairingOffer {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

impl fmt::Debug for PairingOffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairingOffer")
            .field("host_id", &self.host_id)
            .field("host_name", &self.host_name)
            .field("secret", &"<redacted>")
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::super::keys::{SecretKey, from_hex};
    use super::*;

    #[test]
    fn pairing_window_expires() {
        let t0 = Instant::now();
        let w = PairingWindow::open_at(t0, Duration::from_secs(300)).unwrap();
        assert!(w.is_open_at(t0));
        assert!(w.is_open_at(t0 + Duration::from_secs(299)));
        assert!(!w.is_open_at(t0 + Duration::from_secs(300)));
        assert!(w.psk_at(t0 + Duration::from_secs(301)).is_none());
        assert_eq!(w.remaining_at(t0 + Duration::from_secs(100)), Duration::from_secs(200));
        assert_eq!(w.remaining_at(t0 + Duration::from_secs(900)), Duration::ZERO);
    }

    #[test]
    fn pairing_window_is_single_use() {
        let t0 = Instant::now();
        let mut w = PairingWindow::open_at(t0, PAIRING_TTL).unwrap();
        assert!(w.psk_at(t0).is_some());
        assert!(w.consume_at(t0));
        assert!(!w.consume_at(t0), "the second claim finds it closed");
        assert!(!w.is_open_at(t0));
        assert!(w.psk_at(t0).is_none());
        // An expired window cannot be consumed either.
        let mut late = PairingWindow::open_at(t0, Duration::from_secs(1)).unwrap();
        assert!(!late.consume_at(t0 + Duration::from_secs(2)));
    }

    #[test]
    fn each_window_has_a_fresh_secret() {
        let a = PairingWindow::open(PAIRING_TTL).unwrap();
        let b = PairingWindow::open(PAIRING_TTL).unwrap();
        assert_ne!(a.psk().unwrap().expose(), b.psk().unwrap().expose());
        assert_ne!(a.psk().unwrap().expose(), &[0u8; 32]);
    }

    #[test]
    fn offer_has_the_documented_fields() {
        let host = SecretKey::generate().unwrap().public();
        let w = PairingWindow::open(PAIRING_TTL).unwrap();
        let addrs: Vec<SocketAddr> = vec!["127.0.0.1:7777".parse().unwrap()];
        let offer = w.offer("Desk", &host, &addrs);
        assert_eq!(offer.v, 1);
        assert_eq!(offer.host_id, host.host_id());
        assert_eq!(offer.addrs, vec!["127.0.0.1:7777"]);
        assert_eq!(PublicKey::from_hex(&offer.host_key), Some(host));
        assert_eq!(from_hex::<32>(&offer.secret), Some(*w.psk().unwrap().expose()));
        let now = now_ms();
        assert!(offer.expires_at > now + 290_000 && offer.expires_at <= now + 300_000);
        let json: serde_json::Value = serde_json::from_str(&offer.to_json()).unwrap();
        let mut keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["addrs", "expires_at", "host_id", "host_key", "host_name", "secret", "v"]);
        assert_eq!(serde_json::from_str::<PairingOffer>(&offer.to_json()).unwrap(), offer);
    }

    #[test]
    fn offer_and_window_debug_hide_the_secret() {
        let host = SecretKey::generate().unwrap().public();
        let w = PairingWindow::open(PAIRING_TTL).unwrap();
        let offer = w.offer("Desk", &host, &[]);
        let shown = format!("{offer:?} {offer:#?} {w:?}");
        assert!(!shown.contains(&offer.secret));
        assert!(!shown.contains(&offer.secret[..8]));
    }
}

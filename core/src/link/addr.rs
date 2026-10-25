//! The pure address rules both hosts share: which IPv4 addresses may ever be offered as a
//! bind, what kind each is, and the one port range. No I/O; adapter enumeration stays with
//! the caller.

use std::net::Ipv4Addr;

use serde::Serialize;

pub const DEFAULT_PORT: u16 = 47800;
pub const MIN_PORT: u32 = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Loopback,
    Lan,
    Tailscale,
    Other,
}

/// Which kind of address this is, or `None` when it must never be offered: the unspecified
/// address, link-local, multicast, broadcast, and any loopback other than 127.0.0.1.
pub fn classify(ip: Ipv4Addr) -> Option<Kind> {
    if ip == Ipv4Addr::LOCALHOST {
        return Some(Kind::Loopback);
    }
    if ip.is_unspecified() || ip.is_loopback() || ip.is_link_local() || ip.is_multicast() || ip.is_broadcast() {
        return None;
    }
    let o = ip.octets();
    // 100.64.0.0/10 is the carrier-grade NAT range Tailscale hands out.
    if o[0] == 100 && (o[1] & 0b1100_0000) == 64 {
        return Some(Kind::Tailscale);
    }
    if ip.is_private() { Some(Kind::Lan) } else { Some(Kind::Other) }
}

/// Why `port` may not be used, or `None` when it is in 1024..=65535. The one place the range
/// lives: `validate` refuses a save with it and the link refuses to start on a saved port it fails.
pub fn port_error(port: u32) -> Option<String> {
    if (MIN_PORT..=65535).contains(&port) {
        None
    } else {
        Some("the port must be between 1024 and 65535".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_kinds() {
        let k = |s: &str| classify(s.parse().unwrap());
        assert_eq!(k("127.0.0.1"), Some(Kind::Loopback));
        assert_eq!(k("127.0.0.2"), None);
        assert_eq!(k("0.0.0.0"), None);
        assert_eq!(k("169.254.1.1"), None);
        assert_eq!(k("100.64.0.1"), Some(Kind::Tailscale));
        assert_eq!(k("100.128.0.1"), Some(Kind::Other));
        assert_eq!(k("192.168.0.9"), Some(Kind::Lan));
        assert_eq!(k("8.8.8.8"), Some(Kind::Other));
    }

    #[test]
    fn port_range() {
        for port in [0, 1, 1023, 65536] {
            assert!(port_error(port).is_some(), "port {port}");
        }
        for port in [1024, DEFAULT_PORT as u32, 65535] {
            assert!(port_error(port).is_none(), "port {port}");
        }
    }
}

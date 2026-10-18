//! `<app data>/link/config.json`: whether the link is on, which addresses it listens on and
//! on which port. Missing means off. A file with a schema this build does not know, or one
//! that does not parse, turns the link off, is left exactly as it is and makes the settings
//! read-only, the rule `clients.json` follows. A port outside 1024-65535 is a fully understood
//! file with one bad value: it loads as saved, stays editable and is a start error (see
//! `port_error`), not read-only. No secret lives here.

use std::fs::{self, File};
use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SCHEMA: u64 = 1;
pub const DEFAULT_PORT: u16 = 47800;
pub const MIN_PORT: u32 = 1024;
const FILE: &str = "config.json";
const MAX_BINDS: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub schema: u64,
    pub enabled: bool,
    pub port: u16,
    /// Bare IPv4 addresses; the one port is shared by every bind.
    pub binds: Vec<String>,
    pub firewall_notice_seen: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            enabled: false,
            port: DEFAULT_PORT,
            binds: vec!["127.0.0.1".to_string()],
            firewall_notice_seen: false,
        }
    }
}

/// Why the settings cannot be changed. The file is not touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOnly {
    Newer,
    Unreadable,
}

impl ReadOnly {
    pub fn message(self) -> &'static str {
        match self {
            ReadOnly::Newer => "written by a newer Orchestra",
            ReadOnly::Unreadable => "could not be read",
        }
    }
}

/// Read the file. Missing is the defaults; anything unreadable is the defaults (off) plus the
/// reason the settings must not be saved over it.
pub fn load(dir: &Path) -> (Settings, Option<ReadOnly>) {
    let text = match fs::read_to_string(dir.join(FILE)) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return (Settings::default(), None),
        Err(_) => return (Settings::default(), Some(ReadOnly::Unreadable)),
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return (Settings::default(), Some(ReadOnly::Unreadable));
    };
    match value.get("schema").and_then(Value::as_u64) {
        Some(SCHEMA) => {}
        Some(_) => return (Settings::default(), Some(ReadOnly::Newer)),
        None => return (Settings::default(), Some(ReadOnly::Unreadable)),
    }
    match serde_json::from_value::<Settings>(value) {
        Ok(s) => (s, None),
        Err(_) => (Settings::default(), Some(ReadOnly::Unreadable)),
    }
}

/// Write a sibling temp file, flush it to disk, then rename over the target.
pub fn save(dir: &Path, settings: &Settings) -> io::Result<()> {
    let text = serde_json::to_string_pretty(settings).map_err(|_| io::Error::other("settings did not serialize"))?;
    fs::create_dir_all(dir)?;
    let path = dir.join(FILE);
    let tmp = path.with_extension("json.tmp");
    let mut f = File::create(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, &path)
}

// ---- addresses ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Loopback,
    Lan,
    Tailscale,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Interface {
    pub ip: String,
    pub kind: Kind,
    /// The adapter's name, or "This computer only" for loopback.
    pub name: String,
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

/// 127.0.0.1 first, then the usable IPv4 addresses of `adapters` (name, address), each once.
pub fn interfaces_from(adapters: &[(String, Ipv4Addr)]) -> Vec<Interface> {
    let mut out = vec![Interface {
        ip: Ipv4Addr::LOCALHOST.to_string(),
        kind: Kind::Loopback,
        name: "This computer only".to_string(),
    }];
    for (name, ip) in adapters {
        let Some(kind) = classify(*ip) else { continue };
        if kind == Kind::Loopback || out.iter().any(|i| i.ip == ip.to_string()) {
            continue;
        }
        out.push(Interface { ip: ip.to_string(), kind, name: name.chars().take(64).collect() });
    }
    out
}

/// This computer's addresses right now.
pub fn interfaces() -> Vec<Interface> {
    let adapters: Vec<(String, Ipv4Addr)> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|a| match a.ip() {
            std::net::IpAddr::V4(v4) => Some((a.name, v4)),
            std::net::IpAddr::V6(_) => None,
        })
        .collect();
    interfaces_from(&adapters)
}

/// Split saved binds into those present now and those to skip, kept as saved text. A bind
/// that does not parse, is unspecified, or is not on this computer any more is skipped.
pub fn split_binds(saved: &[String], current: &[Interface]) -> (Vec<Ipv4Addr>, Vec<String>) {
    let mut present = Vec::new();
    let mut missing = Vec::new();
    for text in saved {
        match text.parse::<Ipv4Addr>() {
            Ok(ip) if classify(ip).is_some() && current.iter().any(|i| i.ip == text.as_str()) => {
                if !present.contains(&ip) {
                    present.push(ip);
                }
            }
            _ => missing.push(text.clone()),
        }
    }
    (present, missing)
}

pub fn socket_addrs(ips: &[Ipv4Addr], port: u16) -> Vec<SocketAddr> {
    ips.iter().map(|ip| SocketAddr::from((*ip, port))).collect()
}

// ---- a change asked for by the UI ----

/// Why `port` may not be used, or `None` when it is in 1024..=65535. The one place the range
/// lives: `validate` refuses a save with it and the link refuses to start on a saved port it fails.
pub fn port_error(port: u32) -> Option<String> {
    if (MIN_PORT..=65535).contains(&port) {
        None
    } else {
        Some("the port must be between 1024 and 65535".to_string())
    }
}

/// The firewall notice must be shown before a first non-loopback enable.
pub const FIREWALL_FIRST: &str = "firewall_notice_required";

/// Check a requested change against the current settings and the addresses that exist, and
/// return what to save. Never accepts 0.0.0.0, `::` or their mapped forms, a port below 1024
/// or above 65535, or an address that is not on this computer.
pub fn validate(
    prev: &Settings,
    enabled: bool,
    binds: &[String],
    port: u32,
    firewall_notice_seen: bool,
    current: &[Interface],
) -> Result<Settings, String> {
    if let Some(why) = port_error(port) {
        return Err(why);
    }
    if binds.len() > MAX_BINDS {
        return Err("too many addresses".to_string());
    }
    let mut clean: Vec<String> = Vec::new();
    for text in binds {
        let ip: Ipv4Addr = text.parse().map_err(|_| "that is not an IPv4 address".to_string())?;
        if ip.is_unspecified() {
            return Err("an address that listens on every interface is not allowed".to_string());
        }
        if classify(ip).is_none() || !current.iter().any(|i| i.ip == ip.to_string()) {
            return Err(format!("{ip} is not on this computer right now"));
        }
        let canonical = ip.to_string();
        if !clean.contains(&canonical) {
            clean.push(canonical);
        }
    }
    if enabled && clean.is_empty() {
        return Err("pick at least one address".to_string());
    }
    let exposes = clean.iter().any(|b| b != "127.0.0.1");
    let seen = prev.firewall_notice_seen || firewall_notice_seen;
    if enabled && exposes && !seen {
        return Err(FIREWALL_FIRST.to_string());
    }
    Ok(Settings { schema: SCHEMA, enabled, port: port as u16, binds: clean, firewall_notice_seen: seen })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);

    fn scratch() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "orchestra-linkcfg-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn lan() -> Vec<Interface> {
        interfaces_from(&[
            ("Wi-Fi".into(), "192.168.1.20".parse().unwrap()),
            ("Tailscale".into(), "100.101.102.103".parse().unwrap()),
            ("Ethernet".into(), "169.254.9.9".parse().unwrap()),
        ])
    }

    #[test]
    fn link_config_missing_file_is_off_with_defaults() {
        let dir = scratch();
        let (s, ro) = load(&dir);
        assert_eq!(ro, None);
        assert!(!s.enabled);
        assert_eq!(s.port, 47800);
        assert_eq!(s.binds, vec!["127.0.0.1".to_string()]);
        assert!(!s.firewall_notice_seen);
    }

    #[test]
    fn link_config_round_trips_and_leaves_no_temp() {
        let dir = scratch();
        let s = Settings { enabled: true, port: 50000, binds: vec!["192.168.1.20".into()], ..Settings::default() };
        save(&dir, &s).unwrap();
        assert_eq!(load(&dir), (s, None));
        let names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("config.json")]);
        // Saving again replaces it.
        let t = Settings { enabled: false, ..Settings::default() };
        save(&dir, &t).unwrap();
        assert_eq!(load(&dir).0, t);
    }

    #[test]
    fn link_config_unknown_schema_and_garbage_are_off_and_left_alone() {
        let dir = scratch();
        let newer = r#"{"schema":2,"enabled":true,"port":1,"binds":[],"firewall_notice_seen":true,"extra":1}"#;
        fs::write(dir.join("config.json"), newer).unwrap();
        let (s, ro) = load(&dir);
        assert_eq!((s.enabled, ro), (false, Some(ReadOnly::Newer)));
        assert_eq!(fs::read_to_string(dir.join("config.json")).unwrap(), newer);
        for bad in ["not json", "[]", r#"{"enabled":true}"#, r#"{"schema":1,"enabled":true,"port":70000,"binds":[],"firewall_notice_seen":false}"#,
            r#"{"schema":1,"enabled":true,"port":47800,"binds":[],"firewall_notice_seen":false,"surprise":1}"#]
        {
            fs::write(dir.join("config.json"), bad).unwrap();
            let (s, ro) = load(&dir);
            assert_eq!((s.enabled, ro), (false, Some(ReadOnly::Unreadable)), "{bad}");
            assert_eq!(fs::read_to_string(dir.join("config.json")).unwrap(), bad);
        }
    }

    #[test]
    fn link_config_port_range_is_1024_to_65535() {
        for port in [0u32, 22, 1023] {
            assert!(port_error(port).is_some(), "port {port}");
        }
        for port in [1024u32, 65535] {
            assert!(port_error(port).is_none(), "port {port}");
        }
        let prev = Settings::default();
        let cur = lan();
        let bind = vec!["127.0.0.1".to_string()];
        let cur = [cur, interfaces_from(&[("lo".into(), "127.0.0.1".parse().unwrap())])].concat();
        for port in [0u32, 22, 1023] {
            assert!(validate(&prev, true, &bind, port, false, &cur).is_err(), "port {port}");
        }
        for port in [1024u32, 65535] {
            assert!(validate(&prev, true, &bind, port, false, &cur).is_ok(), "port {port}");
        }
    }

    #[test]
    fn link_config_low_port_on_disk_loads_editable_and_left_alone() {
        for port in [0u32, 22, 1023, 1024, 65535] {
            let dir = scratch();
            let text = format!(
                r#"{{"schema":1,"enabled":true,"port":{port},"binds":["127.0.0.1"],"firewall_notice_seen":false}}"#
            );
            fs::write(dir.join(FILE), &text).unwrap();
            let (settings, read_only) = load(&dir);
            assert_eq!(settings.port as u32, port);
            assert!(read_only.is_none(), "port {port} must stay editable");
            assert_eq!(fs::read_to_string(dir.join(FILE)).unwrap(), text, "port {port}: file untouched");
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn link_config_classifies_addresses() {
        let k = |s: &str| classify(s.parse().unwrap());
        assert_eq!(k("127.0.0.1"), Some(Kind::Loopback));
        assert_eq!(k("127.0.0.2"), None);
        assert_eq!(k("100.64.0.1"), Some(Kind::Tailscale));
        assert_eq!(k("100.127.255.254"), Some(Kind::Tailscale));
        assert_eq!(k("100.128.0.1"), Some(Kind::Other));
        assert_eq!(k("100.63.255.255"), Some(Kind::Other));
        assert_eq!(k("10.1.2.3"), Some(Kind::Lan));
        assert_eq!(k("172.16.0.1"), Some(Kind::Lan));
        assert_eq!(k("172.32.0.1"), Some(Kind::Other));
        assert_eq!(k("192.168.0.9"), Some(Kind::Lan));
        assert_eq!(k("8.8.8.8"), Some(Kind::Other));
        assert_eq!(k("169.254.1.1"), None);
        assert_eq!(k("0.0.0.0"), None);
        assert_eq!(k("224.0.0.1"), None);
        assert_eq!(k("255.255.255.255"), None);
    }

    #[test]
    fn link_config_interfaces_list_loopback_first_and_skip_link_local() {
        let list = lan();
        let ips: Vec<&str> = list.iter().map(|i| i.ip.as_str()).collect();
        assert_eq!(ips, vec!["127.0.0.1", "192.168.1.20", "100.101.102.103"]);
        assert_eq!(list[0].kind, Kind::Loopback);
        assert_eq!(list[1].kind, Kind::Lan);
        assert_eq!(list[2].kind, Kind::Tailscale);
        // The same address on two adapters is listed once, and 127.0.0.1 is not repeated.
        let twice = interfaces_from(&[
            ("a".into(), "10.0.0.5".parse().unwrap()),
            ("b".into(), "10.0.0.5".parse().unwrap()),
            ("lo".into(), "127.0.0.1".parse().unwrap()),
        ]);
        assert_eq!(twice.len(), 2);
    }

    #[test]
    fn link_config_validate_refuses_unspecified_and_mapped_addresses() {
        let prev = Settings::default();
        let cur = lan();
        for bad in ["0.0.0.0", "::", "::ffff:0.0.0.0", "::1", "::ffff:127.0.0.1", "", "localhost", "192.168.1.20:47800", "1.2.3.4.5"] {
            let r = validate(&prev, true, &[bad.to_string()], 47800, true, &cur);
            assert!(r.is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn link_config_validate_checks_port_range_and_current_addresses() {
        let prev = Settings::default();
        let cur = lan();
        let ok = ["127.0.0.1".to_string()];
        for port in [0u32, 1, 80, 1023, 65536, 70000, u32::MAX] {
            assert!(validate(&prev, true, &ok, port, false, &cur).is_err(), "port {port}");
        }
        for port in [1024u32, 47800, 65535] {
            assert!(validate(&prev, true, &ok, port, false, &cur).is_ok(), "port {port}");
        }
        // Not on this computer, link-local and an unlisted loopback are refused.
        for ip in ["192.168.7.7", "169.254.9.9", "127.0.0.2", "8.8.8.8"] {
            assert!(validate(&prev, true, &[ip.to_string()], 47800, true, &cur).is_err(), "{ip}");
        }
        assert!(validate(&prev, true, &[], 47800, false, &cur).is_err(), "enabled needs an address");
        assert!(validate(&prev, false, &[], 47800, false, &cur).is_ok(), "off needs none");
    }

    #[test]
    fn link_config_validate_enforces_the_firewall_notice_order() {
        let prev = Settings::default();
        let cur = lan();
        let lan_bind = ["192.168.1.20".to_string()];
        let first = validate(&prev, true, &lan_bind, 47800, false, &cur);
        assert_eq!(first, Err(FIREWALL_FIRST.to_string()));
        let seen = validate(&prev, true, &lan_bind, 47800, true, &cur).unwrap();
        assert!(seen.firewall_notice_seen && seen.enabled);
        // Loopback only never needs it, and saving it does not claim it was shown.
        let local = validate(&prev, true, &["127.0.0.1".to_string()], 47800, false, &cur).unwrap();
        assert!(!local.firewall_notice_seen);
        // Once seen it stays seen, even if a later request leaves the flag false.
        let after = Settings { firewall_notice_seen: true, ..prev };
        assert!(validate(&after, true, &lan_bind, 47800, false, &cur).unwrap().firewall_notice_seen);
        // Disabled never asks.
        assert!(validate(&Settings::default(), false, &lan_bind, 47800, false, &cur).is_ok());
    }

    #[test]
    fn link_config_missing_binds_are_skipped_not_forgotten() {
        let cur = lan();
        let saved = vec!["192.168.1.20".to_string(), "10.9.9.9".to_string(), "0.0.0.0".to_string(), "junk".to_string()];
        let (present, missing) = split_binds(&saved, &cur);
        assert_eq!(present, vec!["192.168.1.20".parse::<Ipv4Addr>().unwrap()]);
        assert_eq!(missing, vec!["10.9.9.9".to_string(), "0.0.0.0".to_string(), "junk".to_string()]);
        let addrs = socket_addrs(&present, 47800);
        assert_eq!(addrs, vec!["192.168.1.20:47800".parse::<SocketAddr>().unwrap()]);
    }
}

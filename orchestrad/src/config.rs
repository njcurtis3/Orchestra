//! The config file: projects, listen addresses and port.
//!
//! Pure parse and validate; the only I/O is reading the one path. A missing file is a valid
//! config (listens on nothing, no projects). Anything else wrong is a `ConfigError` naming the
//! field, and the daemon refuses to start on it: unlike the desktop's read-only fallback,
//! nobody is at a window to fix it.

use std::collections::HashSet;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Component, Path, PathBuf};

use orchestra_core::link::LinkLimits;
use orchestra_core::link::addr::{DEFAULT_PORT, classify, port_error};
use serde::Deserialize;

pub const SCHEMA: i64 = 1;
pub const MAX_BINDS: usize = 16;
/// The product's cap; the file may only lower it.
pub const MAX_CONNECTIONS_CAP: i64 = 16;
const MAX_HOST_NAME: usize = 64;
/// Longest piece of file content an error message repeats back.
const MAX_ECHO: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub host_name: Option<String>,
    pub port: u16,
    pub binds: Vec<Ipv4Addr>,
    pub max_connections: Option<usize>,
    pub projects: Vec<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self { host_name: None, port: DEFAULT_PORT, binds: Vec::new(), max_connections: None, projects: Vec::new() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// The file exists but could not be read.
    Io(String),
    /// Not TOML, or a field of the wrong type or an unknown field (the message names it).
    Parse(String),
    /// A value that parsed but is not allowed.
    Invalid { field: String, reason: String },
}

impl ConfigError {
    fn invalid(field: impl Into<String>, reason: impl Into<String>) -> Self {
        ConfigError::Invalid { field: field.into(), reason: reason.into() }
    }
}

/// `s` cut to `MAX_ECHO` characters, so a huge value in the file cannot flood the log.
fn clip(s: &str) -> String {
    if s.chars().count() <= MAX_ECHO {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(MAX_ECHO).collect::<String>())
    }
}

/// A toml error as one line. `to_string` (unlike `message`) keeps the key path, so a
/// wrong-typed value names its field. Bounded, as it can carry a user-written key.
fn parse_error(e: toml::de::Error) -> ConfigError {
    let one_line = e.to_string().split_whitespace().collect::<Vec<_>>().join(" ");
    let mut msg: String = one_line.chars().take(4 * MAX_ECHO).collect();
    if msg.len() < one_line.len() {
        msg.push_str("...");
    }
    ConfigError::Parse(msg)
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "could not read the config file: {e}"),
            ConfigError::Parse(e) => write!(f, "the config file does not parse: {e}"),
            ConfigError::Invalid { field, reason } => write!(f, "config {field}: {reason}"),
        }
    }
}

impl std::error::Error for ConfigError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    host_name: Option<String>,
    #[serde(default)]
    link: RawLink,
    #[serde(default)]
    limits: RawLimits,
    #[serde(default)]
    projects: Vec<RawProject>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawLink {
    port: Option<i64>,
    #[serde(default)]
    binds: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawLimits {
    max_connections: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProject {
    path: String,
}

/// Read the file at `path`. Missing is the default config; every other failure is an error.
pub fn load(path: &Path) -> Result<Config, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(ConfigError::Io(e.kind().to_string())),
    }
}

pub fn parse(text: &str) -> Result<Config, ConfigError> {
    let mut table: toml::Table = text.parse().map_err(parse_error)?;
    // The schema is checked before the rest so a newer file is reported as such, not as
    // a pile of unknown fields.
    match table.remove("schema") {
        Some(toml::Value::Integer(SCHEMA)) => {}
        Some(_) => return Err(ConfigError::invalid("schema", format!("only schema {SCHEMA} is known"))),
        None => return Err(ConfigError::invalid("schema", format!("missing; this build reads schema {SCHEMA}"))),
    }
    let raw: Raw = table.try_into().map_err(parse_error)?;
    validate(raw)
}

fn validate(raw: Raw) -> Result<Config, ConfigError> {
    let host_name = match raw.host_name {
        None => None,
        Some(n) => {
            let n = n.trim().to_string();
            if n.is_empty() || n.chars().count() > MAX_HOST_NAME || n.chars().any(char::is_control) {
                return Err(ConfigError::invalid("host_name", "must be 1-64 characters with no control characters"));
            }
            Some(n)
        }
    };

    let port = match raw.link.port {
        None => DEFAULT_PORT,
        Some(p) => {
            let p = u32::try_from(p).unwrap_or(0);
            if let Some(reason) = port_error(p) {
                return Err(ConfigError::invalid("link.port", reason));
            }
            p as u16
        }
    };

    if raw.link.binds.len() > MAX_BINDS {
        return Err(ConfigError::invalid("link.binds", format!("at most {MAX_BINDS} addresses")));
    }
    let mut binds = Vec::new();
    let mut seen = HashSet::new();
    for (i, text) in raw.link.binds.iter().enumerate() {
        let field = format!("link.binds[{i}]");
        let ip: IpAddr = text
            .trim()
            .parse()
            .map_err(|_| ConfigError::invalid(&field, format!("{:?} is not an IP address", clip(text))))?;
        let v4 = match ip {
            IpAddr::V4(v4) => v4,
            IpAddr::V6(v6) => {
                return Err(match v6.to_ipv4_mapped() {
                    Some(m) if m.is_unspecified() => {
                        ConfigError::invalid(&field, "listens on every interface; name the one to use")
                    }
                    _ => ConfigError::invalid(&field, "IPv4 addresses only"),
                });
            }
        };
        if v4.is_unspecified() {
            return Err(ConfigError::invalid(&field, "listens on every interface; name the one to use"));
        }
        if classify(v4).is_none() {
            return Err(ConfigError::invalid(&field, format!("{v4} cannot be offered as an address")));
        }
        if seen.insert(v4) {
            binds.push(v4);
        }
    }

    let max_connections = match raw.limits.max_connections {
        None => None,
        Some(n) if (1..=MAX_CONNECTIONS_CAP).contains(&n) => Some(n as usize),
        Some(_) => {
            return Err(ConfigError::invalid(
                "limits.max_connections",
                format!("must be 1-{MAX_CONNECTIONS_CAP}; the file can lower the cap, not raise it"),
            ));
        }
    };

    let mut projects = Vec::new();
    for (i, p) in raw.projects.iter().enumerate() {
        let field = format!("projects[{i}].path");
        // A leading '/' counts everywhere: the daemon is Linux-only but the tests run anywhere.
        let path = PathBuf::from(&p.path);
        if !(path.is_absolute() || p.path.starts_with('/')) {
            return Err(ConfigError::invalid(&field, "must be an absolute path"));
        }
        if path.components().any(|c| c == Component::ParentDir) {
            return Err(ConfigError::invalid(&field, "must not contain '..'"));
        }
        if p.path.contains('\0') {
            return Err(ConfigError::invalid(&field, "must not contain a NUL byte"));
        }
        if !projects.contains(&path) {
            projects.push(path);
        }
    }

    Ok(Config { host_name, port, binds, max_connections, projects })
}

/// The listen addresses for `LinkConfig::binds`. The caller still runs core `validate_binds`.
pub fn to_link_binds(config: &Config) -> Vec<SocketAddr> {
    config.binds.iter().map(|ip| SocketAddr::new(IpAddr::V4(*ip), config.port)).collect()
}

/// The product's limits, lowered where the file asks.
pub fn to_link_limits(config: &Config) -> LinkLimits {
    let mut limits = LinkLimits::default();
    if let Some(n) = config.max_connections {
        limits.max_connections = limits.max_connections.min(n);
    }
    limits
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestra_core::link::validate_binds;

    fn with_binds(binds: &str) -> String {
        format!("schema = 1\n[link]\nbinds = [{binds}]\n")
    }

    fn field_of(r: Result<Config, ConfigError>) -> String {
        match r.unwrap_err() {
            ConfigError::Invalid { field, .. } => field,
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn missing_file_listens_on_nothing() {
        let c = load(Path::new("/definitely/not/here/orchestrad.toml")).unwrap();
        assert_eq!(c, Config::default());
        assert!(to_link_binds(&c).is_empty());
        assert!(c.projects.is_empty());
    }

    #[test]
    fn a_full_file_parses() {
        let c = parse(
            "schema = 1\nhost_name = \"Box\"\n[link]\nport = 5000\nbinds = [\"127.0.0.1\", \"100.64.0.5\", \"127.0.0.1\"]\n\
             [limits]\nmax_connections = 4\n[[projects]]\npath = \"/srv/a\"\n",
        )
        .unwrap();
        assert_eq!(c.host_name.as_deref(), Some("Box"));
        let binds = to_link_binds(&c);
        assert_eq!(binds.len(), 2);
        assert_eq!(binds[1].to_string(), "100.64.0.5:5000");
        assert!(validate_binds(&binds).is_ok());
        assert_eq!(to_link_limits(&c).max_connections, 4);
        assert_eq!(c.projects, vec![PathBuf::from("/srv/a")]);
    }

    #[test]
    fn refuses_unspecified() {
        assert_eq!(field_of(parse(&with_binds("\"0.0.0.0\""))), "link.binds[0]");
        assert_eq!(field_of(parse(&with_binds("\"::\""))), "link.binds[0]");
    }

    #[test]
    fn refuses_mapped_unspecified() {
        let e = parse(&with_binds("\"::ffff:0.0.0.0\"")).unwrap_err().to_string();
        assert!(e.contains("link.binds[0]") && e.contains("every interface"), "{e}");
    }

    #[test]
    fn refuses_ipv6() {
        assert_eq!(field_of(parse(&with_binds("\"::1\""))), "link.binds[0]");
        assert_eq!(field_of(parse(&with_binds("\"fe80::1\""))), "link.binds[0]");
    }

    #[test]
    fn refuses_link_local_and_multicast() {
        for a in ["169.254.1.1", "224.0.0.1", "255.255.255.255", "127.0.0.2"] {
            assert_eq!(field_of(parse(&with_binds(&format!("\"{a}\"")))), "link.binds[0]", "{a}");
        }
        assert_eq!(field_of(parse(&with_binds("\"nope\""))), "link.binds[0]");
    }

    #[test]
    fn too_many_binds_is_an_error() {
        let many = (1..=17).map(|i| format!("\"10.0.0.{i}\"")).collect::<Vec<_>>().join(",");
        assert_eq!(field_of(parse(&with_binds(&many))), "link.binds");
    }

    #[test]
    fn port_out_of_range_is_an_error() {
        for p in ["0", "80", "1023", "65536", "-1"] {
            assert_eq!(field_of(parse(&format!("schema = 1\n[link]\nport = {p}\n"))), "link.port", "{p}");
        }
        assert_eq!(parse("schema = 1\n[link]\nport = 65535\n").unwrap().port, 65535);
    }

    #[test]
    fn unknown_schema_is_an_error() {
        assert_eq!(field_of(parse("schema = 2\nfuture = true\n")), "schema");
        assert_eq!(field_of(parse("host_name = \"x\"\n")), "schema");
    }

    #[test]
    fn unknown_field_is_an_error() {
        for text in [
            "schema = 1\nbogus = 1\n",
            "schema = 1\n[link]\nbogus = 1\n",
            "schema = 1\n[[projects]]\npath = \"/a\"\nx = 1\n",
        ] {
            let e = parse(text).unwrap_err();
            assert!(matches!(e, ConfigError::Parse(ref m) if m.contains("unknown field")), "{e:?}");
        }
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(matches!(parse("schema = = ="), Err(ConfigError::Parse(_))));
    }

    #[test]
    fn relative_project_path_is_an_error() {
        assert_eq!(field_of(parse("schema = 1\n[[projects]]\npath = \"work/a\"\n")), "projects[0].path");
    }

    #[test]
    fn dotdot_project_path_is_an_error() {
        assert_eq!(field_of(parse("schema = 1\n[[projects]]\npath = \"/srv/../etc\"\n")), "projects[0].path");
    }

    #[test]
    fn max_connections_cannot_raise_the_cap() {
        for n in ["0", "17", "-3"] {
            assert_eq!(
                field_of(parse(&format!("schema = 1\n[limits]\nmax_connections = {n}\n"))),
                "limits.max_connections"
            );
        }
        let c = parse("schema = 1\n[limits]\nmax_connections = 16\n").unwrap();
        assert_eq!(to_link_limits(&c).max_connections, 16);
        assert_eq!(to_link_limits(&Config::default()).max_connections, 16);
    }

    #[test]
    fn a_wrong_type_names_its_field() {
        let cases = [
            ("schema = 1
host_name = 5
", "host_name"),
            ("schema = 1
[link]
port = \"5000\"
", "link.port"),
            ("schema = 1
[link]
binds = \"127.0.0.1\"
", "link.binds"),
            ("schema = 1
[limits]
max_connections = \"4\"
", "limits.max_connections"),
            ("schema = 1
[[projects]]
path = 5
", "path"),
            ("schema = 1
link = 5
", "link"),
        ];
        for (text, field) in cases {
            match parse(text) {
                Err(ConfigError::Parse(m)) => assert!(m.contains(field), "{text:?} -> {m}"),
                other => panic!("{text:?} -> {other:?}"),
            }
        }
    }

    #[test]
    fn echoed_input_is_truncated() {
        let huge = "x".repeat(10_000);
        let bind = parse(&with_binds(&format!("\"{huge}\""))).unwrap_err().to_string();
        assert!(bind.len() < 300, "{}", bind.len());
        let key = parse(&format!("schema = 1
{huge} = 1
")).unwrap_err().to_string();
        assert!(key.len() < 600, "{}", key.len());
    }

    #[test]
    fn duplicate_projects_collapse_and_nul_is_refused() {
        let c = parse("schema = 1
[[projects]]
path = \"/a\"
[[projects]]
path = \"/a\"
").unwrap();
        assert_eq!(c.projects.len(), 1);
        assert_eq!(field_of(parse("schema = 1
[[projects]]
path = \"/a\\u0000b\"
")), "projects[0].path");
    }

    #[test]
    fn host_name_is_checked() {
        assert_eq!(field_of(parse("schema = 1\nhost_name = \"  \"\n")), "host_name");
    }
}

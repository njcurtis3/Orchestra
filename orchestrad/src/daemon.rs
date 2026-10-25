//! The service run loop and the commands that talk to it.
//!
//! The rules (what to refuse, which paths to use, what `status` says, how arguments parse)
//! are pure and run everywhere. The daemon and the control-socket client are Linux only.
//! Nothing here logs an offer, a reply or a config: the pairing secret travels only from the
//! link host to the control socket to the terminal that asked.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use orchestra_core::link::addr::Kind;

use crate::control;
use crate::preflight::Preflight;

/// Exit code of `status` and the other client commands when no daemon answers.
pub const EXIT_NOT_RUNNING: i32 = 3;
/// Where the unit's `RuntimeDirectory=orchestrad` puts the socket, for a client run without
/// `$RUNTIME_DIRECTORY` (plain `sudo -u orchestra orchestrad pair`).
pub const DEFAULT_SOCKET: &str = "/run/orchestrad/control.sock";
pub const DEFAULT_CONFIG: &str = "/etc/orchestrad/config.toml";
/// Pairing requests held for `pair` to pick up; more are left to time out.
const MAX_QUEUED: usize = 16;
/// How long one `NextPairingRequest` waits before answering "none yet", so a client can
/// notice Ctrl-C or an expired window between calls.
#[cfg(target_os = "linux")]
const QUEUE_WAIT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Run { config: Option<PathBuf>, state_dir: Option<PathBuf> },
    Status { socket: Option<PathBuf> },
    Pair { socket: Option<PathBuf> },
    Clients { socket: Option<PathBuf> },
    Revoke { id: String, socket: Option<PathBuf> },
    Help,
    Version,
}

pub const USAGE: &str = "usage: orchestrad <command>

commands:
  run [--config PATH] [--state-dir PATH]   run the host (the systemd unit does this)
  status [--socket PATH]                   show the running host; exit 3 if none is running
  pair [--socket PATH]                     show a pairing QR code and confirm a phone
  clients [--socket PATH]                  list paired phones
  revoke ID [--socket PATH]                remove a paired phone
  --help, --version

The client commands find the host through --socket, else $RUNTIME_DIRECTORY, else
$XDG_RUNTIME_DIR/orchestrad, else /run/orchestrad/control.sock. Run them as the host's user.";

/// Parse the arguments after the program name. The error is a one-line usage message.
pub fn parse_args(args: &[String]) -> Result<Command, String> {
    let Some((name, rest)) = args.split_first() else {
        return Err("no command given".to_string());
    };
    let mut flags = Flags::default();
    let mut positional = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let arg = rest[i].as_str();
        let takes_value = match arg {
            "--config" => Some(&mut flags.config),
            "--state-dir" => Some(&mut flags.state_dir),
            "--socket" => Some(&mut flags.socket),
            _ => None,
        };
        match takes_value {
            Some(slot) => {
                i += 1;
                let value = rest.get(i).ok_or_else(|| format!("{arg} needs a value"))?;
                if slot.replace(PathBuf::from(value)).is_some() {
                    return Err(format!("{arg} given twice"));
                }
            }
            None if arg.starts_with("--") => return Err(format!("unknown option {arg}")),
            None => positional.push(arg.to_string()),
        }
        i += 1;
    }
    let only = |allowed: &[&str]| -> Result<(), String> {
        for (name, set) in [("--config", flags.config.is_some()), ("--state-dir", flags.state_dir.is_some()), ("--socket", flags.socket.is_some())] {
            if set && !allowed.contains(&name) {
                return Err(format!("{name} does not apply to `{}`", args[0]));
            }
        }
        Ok(())
    };
    let no_positional = |positional: &[String]| match positional.first() {
        Some(p) => Err(format!("unexpected argument {p}")),
        None => Ok(()),
    };
    match name.as_str() {
        "run" => {
            only(&["--config", "--state-dir"])?;
            no_positional(&positional)?;
            Ok(Command::Run { config: flags.config, state_dir: flags.state_dir })
        }
        "status" | "pair" | "clients" => {
            only(&["--socket"])?;
            no_positional(&positional)?;
            let socket = flags.socket;
            Ok(match name.as_str() {
                "status" => Command::Status { socket },
                "pair" => Command::Pair { socket },
                _ => Command::Clients { socket },
            })
        }
        "revoke" => {
            only(&["--socket"])?;
            match positional.as_slice() {
                [id] => Ok(Command::Revoke { id: id.clone(), socket: flags.socket }),
                [] => Err("revoke needs a client id (see `orchestrad clients`)".to_string()),
                _ => Err("revoke takes one client id".to_string()),
            }
        }
        "--help" | "-h" | "help" => Ok(Command::Help),
        "--version" | "-V" => Ok(Command::Version),
        other => Err(format!("unknown command {other}")),
    }
}

#[derive(Default)]
struct Flags {
    config: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    socket: Option<PathBuf>,
}

/// Why the daemon will not start, or `None`. Root is refused because agents run as this
/// user and need no more than a service account; PID 1 because the daemon runs no reaper
/// for the processes agents leave behind.
pub fn refuse_start(euid: u32, pid: u32) -> Option<&'static str> {
    if euid == 0 {
        return Some("refusing to run as root: run as an unprivileged user (the unit sets User=orchestra)");
    }
    if pid == 1 {
        return Some("refusing to run as PID 1: orchestrad does not reap child processes; use systemd, or `docker run --init`");
    }
    None
}

/// `--state-dir`, else the first entry of `$STATE_DIRECTORY` (systemd may list several),
/// else the fallback (core's XDG rule). Only absolute paths count from the environment.
pub fn resolve_state_dir(flag: Option<&Path>, state_directory: Option<&str>, fallback: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(path) = flag {
        return Some(path.to_path_buf());
    }
    state_directory
        .and_then(|v| v.split(':').next())
        .filter(|v| v.starts_with('/'))
        .map(PathBuf::from)
        .or(fallback)
}

/// The control socket: the rules in `control::socket_path`, then the unit's own directory.
pub fn resolve_socket(flag: Option<&Path>, runtime_directory: Option<&str>, xdg_runtime_dir: Option<&str>) -> PathBuf {
    control::socket_path(flag, runtime_directory, xdg_runtime_dir).unwrap_or_else(|| PathBuf::from(DEFAULT_SOCKET))
}

/// The name a phone sees: the config's, else the machine's, else "Orchestra".
pub fn host_name(configured: Option<&str>, system: Option<&str>) -> String {
    let clean = |s: &str| s.chars().filter(|c| !c.is_control()).take(64).collect::<String>().trim().to_string();
    [configured, system].into_iter().flatten().map(clean).find(|s| !s.is_empty()).unwrap_or_else(|| "Orchestra".to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    NotAFolder,
    NotOwned,
}

impl std::fmt::Display for Skip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Skip::NotAFolder => "not a folder",
            Skip::NotOwned => "not owned by the service user",
        })
    }
}

/// A configured project is added only if it is a folder owned by the user running the daemon.
pub fn project_verdict(is_dir: bool, owner: Option<u32>, euid: u32) -> Result<(), Skip> {
    if !is_dir {
        return Err(Skip::NotAFolder);
    }
    if owner != Some(euid) {
        return Err(Skip::NotOwned);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectState {
    Active,
    /// Added at start, and the folder is gone now.
    Missing,
    Skipped(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectLine {
    pub path: String,
    pub state: ProjectState,
}

/// Everything `status` prints.
#[derive(Debug, Clone)]
pub struct StatusView {
    pub version: &'static str,
    pub pid: u32,
    pub state_dir: String,
    /// `None` when storage is fine, else the reason saving is off.
    pub storage_problem: Option<String>,
    /// Empty when the link is off.
    pub listening: Vec<(SocketAddr, Option<Kind>)>,
    pub paired: Option<usize>,
    pub pairing_open: bool,
    pub preflight: Preflight,
    pub projects: Vec<ProjectLine>,
    pub sessions: Vec<(String, usize)>,
}

fn kind_name(kind: Option<Kind>) -> &'static str {
    match kind {
        Some(Kind::Loopback) => "loopback",
        Some(Kind::Lan) => "lan",
        Some(Kind::Tailscale) => "tailscale",
        Some(Kind::Other) => "other",
        None => "unclassified",
    }
}

pub fn format_status(v: &StatusView) -> String {
    let mut out = String::new();
    out.push_str(&format!("orchestrad {} (pid {})\n", v.version, v.pid));
    out.push_str(&format!("state dir: {}\n", v.state_dir));
    match &v.storage_problem {
        None => out.push_str("storage: ok\n"),
        Some(why) => out.push_str(&format!("storage: NOT SAVING ({why})\n")),
    }
    if v.listening.is_empty() {
        out.push_str("link: off, listening on nothing (set link.binds in the config to turn it on)\n");
    } else {
        out.push_str("link: listening on\n");
        for (addr, kind) in &v.listening {
            out.push_str(&format!("  {addr} ({})\n", kind_name(*kind)));
        }
    }
    match v.paired {
        Some(n) => out.push_str(&format!("paired phones: {n}\n")),
        None => out.push_str("paired phones: -\n"),
    }
    out.push_str(&format!("pairing: {}\n", if v.pairing_open { "open" } else { "closed" }));
    out.push_str(&format!("claude: {}\n", v.preflight.summary()));
    if v.projects.is_empty() {
        out.push_str("projects: none configured\n");
    } else {
        out.push_str("projects:\n");
        for p in &v.projects {
            let state = match &p.state {
                ProjectState::Active => "ok".to_string(),
                ProjectState::Missing => "MISSING".to_string(),
                ProjectState::Skipped(why) => format!("skipped, {why}"),
            };
            out.push_str(&format!("  {} ({state})\n", crate::log::clean(&p.path)));
        }
    }
    if v.sessions.is_empty() {
        out.push_str("sessions: none\n");
    } else {
        let counts: Vec<String> = v.sessions.iter().map(|(s, n)| format!("{s} {n}")).collect();
        out.push_str(&format!("sessions: {}\n", counts.join(", ")));
    }
    out
}

/// Pairing requests waiting for `pair` to pick up.
pub struct PairQueue<T> {
    items: Mutex<std::collections::VecDeque<T>>,
    ready: Condvar,
}

impl<T> Default for PairQueue<T> {
    fn default() -> Self {
        Self { items: Mutex::new(std::collections::VecDeque::new()), ready: Condvar::new() }
    }
}

impl<T> PairQueue<T> {
    fn items(&self) -> std::sync::MutexGuard<'_, std::collections::VecDeque<T>> {
        self.items.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// False when the queue is full and the request was dropped.
    pub fn push(&self, item: T) -> bool {
        let mut items = self.items();
        if items.len() >= MAX_QUEUED {
            return false;
        }
        items.push_back(item);
        self.ready.notify_one();
        true
    }

    /// The oldest waiting item, waiting up to `timeout` for one.
    pub fn next(&self, timeout: Duration) -> Option<T> {
        let deadline = std::time::Instant::now() + timeout;
        let mut items = self.items();
        loop {
            if let Some(item) = items.pop_front() {
                return Some(item);
            }
            let left = deadline.checked_duration_since(std::time::Instant::now())?;
            items = self.ready.wait_timeout(items, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    pub fn clear(&self) {
        self.items().clear();
    }
}

/// Milliseconds since the epoch as `YYYY-MM-DD` (UTC).
pub fn utc_date(ms: u64) -> String {
    let days = (ms / 86_400_000) as i64;
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(target_os = "linux")]
pub use linux::{client, run};

#[cfg(target_os = "linux")]
mod linux {
    use std::io::{BufRead, IsTerminal, Write};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use orchestra_core::host::open_host;
    use orchestra_core::link::addr::classify;
    use orchestra_core::link::keys::FileKeyProvider;
    use orchestra_core::link::{LinkCallbacks, LinkConfig, LinkHost, PendingPairing, validate_binds};
    use orchestra_core::notify;
    use orchestra_core::session::Sessions;
    use orchestra_core::session::process;
    use orchestra_core::session::store::StorageStatus;

    use super::*;
    use crate::config::{self, Config};
    use crate::control::{ClientInfo, Handler, Pending, Reply, Request};
    use crate::{log, preflight, qr};

    /// A signal gets this long to finish the shutdown before the process gives up.
    const SHUTDOWN_BUDGET: Duration = Duration::from_secs(12);
    /// How long to wait for the link to close its connections. A phone stuck in a write can
    /// hold `LinkHost::stop` for up to 30 s; by then the state is saved and agents are killed,
    /// so the stop is still clean.
    const LINK_STOP_WAIT: Duration = Duration::from_secs(6);

    fn euid() -> u32 {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() }
    }

    fn signal_set() -> libc::sigset_t {
        // SAFETY: the set is initialised by sigemptyset before use; the calls only fill it.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGTERM);
            libc::sigaddset(&mut set, libc::SIGINT);
            set
        }
    }

    /// Block SIGTERM and SIGINT in this thread. Called before any thread is spawned, so every
    /// thread inherits it and only the one in `wait_signal` ever sees them.
    fn block_signals() -> libc::sigset_t {
        let set = signal_set();
        // SAFETY: `set` is a valid initialised set; the old mask is not wanted.
        unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) };
        set
    }

    fn wait_signal(set: &libc::sigset_t) {
        let mut sig = 0;
        // SAFETY: `set` is valid and `sig` is a valid out pointer.
        unsafe { libc::sigwait(set, &mut sig) };
    }

    fn system_host_name() -> Option<String> {
        let mut buf = [0u8; 256];
        // SAFETY: the buffer is valid for the length passed.
        let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
        if rc != 0 {
            return None;
        }
        let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        Some(String::from_utf8_lossy(&buf[..end]).into_owned())
    }

    fn make_private_dir(path: &Path) -> std::io::Result<()> {
        if path.is_dir() {
            return Ok(());
        }
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(path)
    }

    /// The running host as the control socket sees it.
    struct Daemon {
        sessions: Arc<Sessions>,
        link: Option<Arc<LinkHost>>,
        state_dir: PathBuf,
        storage: StorageStatus,
        /// What became of each configured project at start.
        configured: Vec<(PathBuf, Option<Skip>)>,
        queue: Arc<PairQueue<PendingPairing>>,
        pairing_ttl: Duration,
        pairing_until: Mutex<Option<Instant>>,
    }

    impl Daemon {
        fn pairing_open(&self) -> bool {
            self.pairing_until.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|t| Instant::now() < t)
        }

        fn set_pairing(&self, open: bool) {
            let until = open.then(|| Instant::now() + self.pairing_ttl);
            *self.pairing_until.lock().unwrap_or_else(|e| e.into_inner()) = until;
        }

        fn close_pairing(&self) {
            if let Some(link) = &self.link {
                link.close_pairing();
            }
            self.set_pairing(false);
            self.queue.clear();
        }

        fn status_view(&self) -> StatusView {
            let mut counts: Vec<(String, usize)> = Vec::new();
            for s in self.sessions.list_sessions() {
                let name = serde_json::to_string(&s.status).unwrap_or_default().trim_matches('"').to_string();
                match counts.iter_mut().find(|(n, _)| *n == name) {
                    Some((_, c)) => *c += 1,
                    None => counts.push((name, 1)),
                }
            }
            counts.sort();
            let listening = self
                .link
                .as_ref()
                .map(|l| {
                    l.local_addrs()
                        .into_iter()
                        .map(|a| {
                            let kind = match a.ip() {
                                std::net::IpAddr::V4(v4) => classify(v4),
                                _ => None,
                            };
                            (a, kind)
                        })
                        .collect()
                })
                .unwrap_or_default();
            let projects = self
                .configured
                .iter()
                .map(|(path, skip)| ProjectLine {
                    path: path.display().to_string(),
                    state: match skip {
                        Some(why) => ProjectState::Skipped(why.to_string()),
                        None if path.is_dir() => ProjectState::Active,
                        None => ProjectState::Missing,
                    },
                })
                .collect();
            StatusView {
                version: env!("CARGO_PKG_VERSION"),
                pid: std::process::id(),
                state_dir: self.state_dir.display().to_string(),
                storage_problem: (!self.storage.ok).then(|| self.storage.reason.unwrap_or("unknown").to_string()),
                listening,
                paired: self.link.as_ref().and_then(|l| l.list_clients().ok()).map(|c| c.len()),
                pairing_open: self.pairing_open(),
                preflight: preflight::check_system(),
                projects,
                sessions: counts,
            }
        }
    }

    fn error(message: impl std::fmt::Display) -> Reply {
        Reply::Error { message: message.to_string() }
    }

    impl Handler for Daemon {
        fn handle(&self, request: Request) -> Reply {
            if request == Request::Status {
                return Reply::Status { text: format_status(&self.status_view()) };
            }
            let Some(link) = &self.link else {
                return error("the link is off: set link.binds in the config file");
            };
            match request {
                Request::Status => unreachable!("answered above"),
                Request::OpenPairing => {
                    self.queue.clear();
                    match link.open_pairing() {
                        Ok(offer) => {
                            self.set_pairing(true);
                            log::notice("pairing window opened");
                            Reply::Offer { offer: offer.to_json() }
                        }
                        Err(e) => error(e),
                    }
                }
                Request::NextPairingRequest => {
                    let pending = self.queue.next(QUEUE_WAIT).map(|p| Pending {
                        id: p.id,
                        client_name: p.client_name,
                        fingerprint: p.fingerprint,
                    });
                    Reply::Pending { pending }
                }
                Request::Confirm { id } => match link.confirm_pairing(&id, None) {
                    Ok(_) => Reply::Ok,
                    Err(e) => error(e),
                },
                Request::Reject { id } => match link.reject_pairing(&id) {
                    Ok(()) => Reply::Ok,
                    Err(e) => error(e),
                },
                Request::ClosePairing => {
                    self.close_pairing();
                    log::notice("pairing window closed");
                    Reply::Ok
                }
                Request::ListClients => match link.list_clients() {
                    Ok(clients) => Reply::Clients {
                        clients: clients
                            .into_iter()
                            .map(|c| ClientInfo {
                                id: c.id,
                                name: c.name,
                                scopes: c.scopes.iter().map(|s| s.as_str().to_string()).collect(),
                                paired_at: c.paired_at,
                            })
                            .collect(),
                    },
                    Err(e) => error(e),
                },
                Request::Revoke { id } => match link.revoke(&id) {
                    Ok(()) => Reply::Ok,
                    Err(e) => error(e),
                },
            }
        }
    }

    /// Decide which configured projects are served: folders owned by this user, judged on
    /// the real path (symlinks resolved, so a link cannot be retargeted after the check).
    /// Returns what became of each configured path and the accepted real paths.
    fn vet_projects(config: &Config, euid: u32) -> (Vec<(PathBuf, Option<Skip>)>, Vec<PathBuf>) {
        let mut out = Vec::new();
        let mut accepted = Vec::new();
        for path in &config.projects {
            let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
            let meta = std::fs::metadata(&real).ok();
            match project_verdict(meta.as_ref().is_some_and(|m| m.is_dir()), meta.as_ref().map(|m| m.uid()), euid) {
                Ok(()) => {
                    if !accepted.contains(&real) {
                        accepted.push(real);
                    }
                    out.push((path.clone(), None));
                }
                Err(why) => {
                    log::warn(&format!("project {} skipped: {why}", path.display()));
                    out.push((path.clone(), Some(why)));
                }
            }
        }
        (out, accepted)
    }

    /// Start the host and serve until SIGTERM or SIGINT. The return value is the exit code.
    pub fn run(config_path: Option<PathBuf>, state_flag: Option<PathBuf>) -> i32 {
        log::init(std::env::var("JOURNAL_STREAM").ok().as_deref());
        let euid = euid();
        if let Some(why) = refuse_start(euid, std::process::id()) {
            log::error(why);
            return 1;
        }
        // Before the first thread exists, so none of them takes the signals.
        let signals = block_signals();

        let config_path = config_path.unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG));
        let config = match config::load(&config_path) {
            Ok(c) => c,
            Err(e) => {
                log::error(&format!("{} ({e})", config_path.display()));
                return 1;
            }
        };
        let state_dir = resolve_state_dir(
            state_flag.as_deref(),
            std::env::var("STATE_DIRECTORY").ok().as_deref(),
            orchestra_core::paths::state_dir(),
        );
        let Some(state_dir) = state_dir else {
            log::error("no state directory: pass --state-dir, or set STATE_DIRECTORY or HOME");
            return 1;
        };
        let link_dir = state_dir.join("link");
        if let Err(e) = make_private_dir(&state_dir).and_then(|()| make_private_dir(&link_dir)) {
            log::error(&format!("cannot create the state directory {}: {}", state_dir.display(), e.kind()));
            return 1;
        }

        let preflight = preflight::check_system();
        for problem in preflight.problems() {
            log::error(problem);
        }

        // open_host restores every project ever saved, so the config's verdict goes in with
        // it: the rest are hidden (not deleted) before any background work can start there.
        let (mut configured, accepted) = vet_projects(&config, euid);
        let (sessions, storage) = open_host(
            Some(&state_dir),
            // Link subscribers get events from Sessions directly; nobody else listens.
            Box::new(|_| {}),
            Box::new(process::spawn),
            Box::new(|project, notice| {
                log::info(&log::clean(&notify::body(project, notice.kind)));
            }),
            Some(&accepted),
        );
        if !storage.ok {
            log::error(&format!("state is not being saved ({})", storage.reason.unwrap_or("unknown")));
        }
        let hidden = sessions.hidden_project_count();
        if hidden > 0 {
            log::warn(&format!("{hidden} saved project(s) are not in the config and are hidden"));
        }
        for path in &accepted {
            match sessions.add_project(path) {
                Ok(p) => log::info(&format!("project {} ready", log::clean(&p.name))),
                Err(e) => {
                    log::warn(&format!("project {} skipped: {}", path.display(), log::clean(&e)));
                    for (c, skip) in configured.iter_mut() {
                        if std::fs::canonicalize(&*c).is_ok_and(|r| &r == path) {
                            *skip = Some(Skip::NotAFolder);
                        }
                    }
                }
            }
        }

        let queue: Arc<PairQueue<PendingPairing>> = Arc::new(PairQueue::default());
        let mut link_config = LinkConfig::new(
            host_name(config.host_name.as_deref(), system_host_name().as_deref()),
            &link_dir,
            storage.clone(),
        );
        link_config.binds = config::to_link_binds(&config);
        link_config.limits = config::to_link_limits(&config);
        let pairing_ttl = link_config.limits.pairing_ttl;
        let link = if link_config.binds.is_empty() {
            log::notice("no link.binds in the config: the phone link is off and nothing is listening");
            None
        } else {
            let queue = queue.clone();
            let callbacks = LinkCallbacks {
                on_pairing_request: Box::new(move |pending| {
                    if !queue.push(pending) {
                        log::warn("too many pairing requests waiting; one was dropped");
                    }
                }),
                log: Box::new(|event| {
                    let (level, text) = log::describe(&event);
                    log::log(level, &text);
                }),
            };
            let host = Arc::new(LinkHost::new(
                sessions.clone(),
                link_config.clone(),
                Box::new(FileKeyProvider::new(&link_dir)),
                callbacks,
            ));
            let started = validate_binds(&link_config.binds)
                .map_err(|e| e.to_string())
                .and_then(|()| host.start().map_err(|e| e.to_string()));
            if let Err(e) = started {
                log::error(&format!("the phone link did not start: {e}"));
                sessions.shutdown();
                return 1;
            }
            Some(host)
        };

        let daemon = Arc::new(Daemon {
            sessions: sessions.clone(),
            link: link.clone(),
            state_dir: state_dir.clone(),
            storage,
            configured,
            queue,
            pairing_ttl,
            pairing_until: Mutex::new(None),
        });
        let socket = resolve_socket(
            None,
            std::env::var("RUNTIME_DIRECTORY").ok().as_deref(),
            std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        );
        let server = match control::serve(&socket, daemon.clone()) {
            Ok(s) => s,
            Err(e) => {
                log::error(&format!(
                    "cannot open the control socket {} ({}): set RUNTIME_DIRECTORY to a directory this user owns",
                    socket.display(),
                    e.kind()
                ));
                if let Some(link) = &link {
                    link.stop();
                }
                sessions.shutdown();
                return 1;
            }
        };
        log::notice(&format!("orchestrad {} ready; control socket {}", env!("CARGO_PKG_VERSION"), socket.display()));

        wait_signal(&signals);
        log::notice("stopping");
        // Hard stop, inside the unit's TimeoutStopSec=15, for a hang in the steps below.
        std::thread::spawn(|| {
            std::thread::sleep(SHUTDOWN_BUDGET);
            log::error("shutdown did not finish in time; exiting");
            std::process::exit(1);
        });
        daemon.close_pairing();
        drop(server);
        // Sessions first: it saves the final state and kills the agents, and does not
        // depend on any phone. Then the link, which a slow phone may hold up.
        sessions.shutdown();
        if let Some(link) = link {
            let (done, wait) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                link.stop();
                let _ = done.send(());
            });
            if wait.recv_timeout(LINK_STOP_WAIT).is_err() {
                log::warn("a phone connection was slow to close; exiting without waiting for it");
            }
        }
        log::notice("stopped");
        0
    }

    fn not_running(socket: &Path) -> i32 {
        eprintln!("orchestrad is not running (nothing answers on {})", socket.display());
        EXIT_NOT_RUNNING
    }

    /// One request. `Err` is the exit code, after saying what went wrong.
    fn ask(socket: &Path, request: &Request, timeout: Option<Duration>) -> Result<Reply, i32> {
        use std::io::ErrorKind::{ConnectionRefused, NotFound};
        match control::request(socket, request, timeout) {
            Ok(Reply::Error { message }) => {
                eprintln!("orchestrad: {}", log::clean(&message));
                Err(1)
            }
            Ok(reply) => Ok(reply),
            Err(e) if matches!(e.kind(), NotFound | ConnectionRefused) => Err(not_running(socket)),
            Err(e) => {
                eprintln!("orchestrad: cannot talk to {}: {}", socket.display(), e.kind());
                Err(1)
            }
        }
    }

    const SHORT: Option<Duration> = Some(Duration::from_secs(10));

    fn socket_for(flag: Option<PathBuf>) -> PathBuf {
        resolve_socket(
            flag.as_deref(),
            std::env::var("RUNTIME_DIRECTORY").ok().as_deref(),
            std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        )
    }

    /// Run a client command and return its exit code.
    pub fn client(command: Command) -> i32 {
        match command {
            Command::Status { socket } => {
                let socket = socket_for(socket);
                match ask(&socket, &Request::Status, SHORT) {
                    Ok(Reply::Status { text }) => {
                        print!("{text}");
                        0
                    }
                    Ok(_) => unexpected(),
                    Err(code) => code,
                }
            }
            Command::Clients { socket } => {
                let socket = socket_for(socket);
                match ask(&socket, &Request::ListClients, SHORT) {
                    Ok(Reply::Clients { clients }) => {
                        if clients.is_empty() {
                            println!("no paired phones");
                        }
                        for c in clients {
                            println!(
                                "{}  {}  [{}]  paired {}",
                                c.id,
                                log::clean(&c.name),
                                c.scopes.join(","),
                                utc_date(c.paired_at)
                            );
                        }
                        0
                    }
                    Ok(_) => unexpected(),
                    Err(code) => code,
                }
            }
            Command::Revoke { id, socket } => {
                let socket = socket_for(socket);
                match ask(&socket, &Request::Revoke { id: id.clone() }, SHORT) {
                    Ok(Reply::Ok) => {
                        println!("revoked {}", log::clean(&id));
                        0
                    }
                    Ok(_) => unexpected(),
                    Err(code) => code,
                }
            }
            Command::Pair { socket } => pair(&socket_for(socket)),
            Command::Run { .. } | Command::Help | Command::Version => unreachable!("not a client command"),
        }
    }

    fn unexpected() -> i32 {
        eprintln!("orchestrad: the daemon sent an unexpected reply");
        1
    }

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    /// Ctrl-C and SIGTERM close the pairing window and exit.
    fn watch_for_interrupt(socket: PathBuf) {
        let set = signal_set();
        let _ = std::thread::Builder::new().name("interrupt".into()).spawn(move || {
            wait_signal(&set);
            let _ = control::request(&socket, &Request::ClosePairing, Some(Duration::from_secs(3)));
            eprintln!("\npairing closed");
            std::process::exit(130);
        });
    }

    fn pair(socket: &Path) -> i32 {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            eprintln!("orchestrad pair needs a terminal: it draws a QR code and asks you to confirm the phone");
            return 1;
        }
        // Blocked here, before the watcher thread exists, so only the watcher sees them.
        let _ = block_signals();
        let offer = match ask(socket, &Request::OpenPairing, SHORT) {
            Ok(Reply::Offer { offer }) => offer,
            Ok(_) => return unexpected(),
            Err(code) => return code,
        };
        let expires_at = serde_json::from_str::<serde_json::Value>(&offer)
            .ok()
            .and_then(|v| v.get("expires_at").and_then(serde_json::Value::as_u64));
        let code = match qr::render(&offer) {
            Ok(code) => code,
            Err(e) => {
                let _ = ask(socket, &Request::ClosePairing, SHORT);
                eprintln!("orchestrad: {e}");
                return 1;
            }
        };
        drop(offer);
        watch_for_interrupt(socket.to_path_buf());
        println!("{code}");
        println!("Scan this in the Orchestra app. The code is drawn light on dark: use a dark terminal.");
        println!("It carries the pairing secret; keep this screen private. Ctrl-C closes pairing.");
        println!("Waiting for a phone...");
        loop {
            if expires_at.is_some_and(|t| now_ms() >= t) {
                eprintln!("the pairing window expired; run `orchestrad pair` again");
                return 1;
            }
            let pending = match ask(socket, &Request::NextPairingRequest, SHORT) {
                Ok(Reply::Pending { pending: Some(p) }) => p,
                Ok(Reply::Pending { pending: None }) => continue,
                Ok(_) => return unexpected(),
                Err(code) => return code,
            };
            println!();
            println!("A phone wants to pair.");
            println!("  name:        {}", log::clean(&pending.client_name));
            println!("  fingerprint: {}", log::clean(&pending.fingerprint));
            print!("Pair this phone? [y/N] ");
            let _ = std::io::stdout().flush();
            let mut answer = String::new();
            let yes = std::io::stdin().lock().read_line(&mut answer).is_ok()
                && matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes");
            if yes {
                match ask(socket, &Request::Confirm { id: pending.id }, SHORT) {
                    Ok(_) => {
                        println!("paired.");
                        let _ = ask(socket, &Request::ClosePairing, SHORT);
                        return 0;
                    }
                    // `ask` has said why (the phone may have given up); keep waiting.
                    Err(EXIT_NOT_RUNNING) => return EXIT_NOT_RUNNING,
                    Err(_) => continue,
                }
            }
            match ask(socket, &Request::Reject { id: pending.id }, SHORT) {
                Ok(_) => println!("rejected."),
                Err(EXIT_NOT_RUNNING) => return EXIT_NOT_RUNNING,
                Err(_) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn root_and_pid_one_are_refused() {
        assert!(refuse_start(0, 4242).unwrap().contains("root"));
        assert!(refuse_start(1000, 1).unwrap().contains("--init"));
        assert!(refuse_start(0, 1).is_some());
        assert_eq!(refuse_start(1000, 4242), None);
    }

    #[test]
    fn run_takes_config_and_state_dir() {
        assert_eq!(
            parse_args(&args(&["run", "--config", "/c.toml", "--state-dir", "/s"])),
            Ok(Command::Run { config: Some("/c.toml".into()), state_dir: Some("/s".into()) })
        );
        assert_eq!(parse_args(&args(&["run"])), Ok(Command::Run { config: None, state_dir: None }));
    }

    #[test]
    fn every_client_command_takes_socket() {
        let s = Some(PathBuf::from("/x.sock"));
        for (name, want) in [
            ("status", Command::Status { socket: s.clone() }),
            ("pair", Command::Pair { socket: s.clone() }),
            ("clients", Command::Clients { socket: s.clone() }),
        ] {
            assert_eq!(parse_args(&args(&[name, "--socket", "/x.sock"])), Ok(want));
        }
        assert_eq!(
            parse_args(&args(&["revoke", "c1", "--socket", "/x.sock"])),
            Ok(Command::Revoke { id: "c1".into(), socket: s.clone() })
        );
        assert_eq!(
            parse_args(&args(&["revoke", "--socket", "/x.sock", "c1"])),
            Ok(Command::Revoke { id: "c1".into(), socket: s })
        );
    }

    #[test]
    fn bad_arguments_say_why() {
        assert!(parse_args(&[]).is_err());
        assert!(parse_args(&args(&["frobnicate"])).unwrap_err().contains("frobnicate"));
        assert!(parse_args(&args(&["status", "--config", "/c"])).unwrap_err().contains("--config"));
        assert!(parse_args(&args(&["run", "--socket", "/s"])).is_err());
        assert!(parse_args(&args(&["run", "--config"])).unwrap_err().contains("needs a value"));
        assert!(parse_args(&args(&["revoke"])).is_err());
        assert!(parse_args(&args(&["revoke", "a", "b"])).is_err());
        assert!(parse_args(&args(&["status", "extra"])).is_err());
        assert!(parse_args(&args(&["status", "--socket", "/a", "--socket", "/b"])).is_err());
        assert_eq!(parse_args(&args(&["--version"])), Ok(Command::Version));
        assert_eq!(parse_args(&args(&["--help"])), Ok(Command::Help));
    }

    #[test]
    fn socket_resolution_order() {
        let flag = PathBuf::from("/flag.sock");
        assert_eq!(resolve_socket(Some(&flag), Some("/run/x"), Some("/xdg")), flag);
        assert_eq!(resolve_socket(None, Some("/run/x:/run/y"), Some("/xdg")), PathBuf::from("/run/x/control.sock"));
        assert_eq!(resolve_socket(None, None, Some("/xdg")), PathBuf::from("/xdg/orchestrad/control.sock"));
        assert_eq!(resolve_socket(None, None, None), PathBuf::from("/run/orchestrad/control.sock"));
        assert_eq!(resolve_socket(None, Some("relative"), None), PathBuf::from("/run/orchestrad/control.sock"));
    }

    #[test]
    fn state_dir_resolution_order() {
        let fallback = Some(PathBuf::from("/home/u/.local/state/orchestra"));
        assert_eq!(resolve_state_dir(Some(Path::new("/flag")), Some("/env"), fallback.clone()), Some("/flag".into()));
        assert_eq!(resolve_state_dir(None, Some("/var/lib/orchestra:/other"), fallback.clone()), Some("/var/lib/orchestra".into()));
        assert_eq!(resolve_state_dir(None, Some("rel"), fallback.clone()), fallback);
        assert_eq!(resolve_state_dir(None, None, None), None);
    }

    #[test]
    fn host_name_prefers_the_config() {
        assert_eq!(host_name(Some("Box"), Some("sys")), "Box");
        assert_eq!(host_name(None, Some("sys")), "sys");
        assert_eq!(host_name(Some("  "), Some("")), "Orchestra");
        assert_eq!(host_name(None, None), "Orchestra");
    }

    #[test]
    fn projects_need_a_folder_the_service_user_owns() {
        assert_eq!(project_verdict(true, Some(1000), 1000), Ok(()));
        assert_eq!(project_verdict(false, None, 1000), Err(Skip::NotAFolder));
        assert_eq!(project_verdict(true, Some(0), 1000), Err(Skip::NotOwned));
        assert_eq!(project_verdict(true, None, 1000), Err(Skip::NotOwned));
    }

    fn view() -> StatusView {
        StatusView {
            version: "0.1.0",
            pid: 42,
            state_dir: "/var/lib/orchestra".into(),
            storage_problem: None,
            listening: vec![("127.0.0.1:47800".parse().unwrap(), Some(Kind::Loopback))],
            paired: Some(2),
            pairing_open: true,
            preflight: Preflight { claude_on_path: true, sign_in: Some(crate::preflight::SignIn::OAuthToken) },
            projects: vec![
                ProjectLine { path: "/srv/a".into(), state: ProjectState::Active },
                ProjectLine { path: "/srv/b".into(), state: ProjectState::Missing },
                ProjectLine { path: "/srv/c".into(), state: ProjectState::Skipped("not a folder".into()) },
            ],
            sessions: vec![("idle".into(), 2), ("working".into(), 1)],
        }
    }

    #[test]
    fn status_lists_everything() {
        let text = format_status(&view());
        for want in [
            "orchestrad 0.1.0 (pid 42)",
            "state dir: /var/lib/orchestra",
            "storage: ok",
            "127.0.0.1:47800 (loopback)",
            "paired phones: 2",
            "pairing: open",
            "claude: ok (claude on PATH; CLAUDE_CODE_OAUTH_TOKEN is set)",
            "/srv/a (ok)",
            "/srv/b (MISSING)",
            "/srv/c (skipped, not a folder)",
            "sessions: idle 2, working 1",
        ] {
            assert!(text.contains(want), "missing {want:?} in:\n{text}");
        }
    }

    #[test]
    fn status_with_no_binds_says_nothing_listens() {
        let mut v = view();
        v.listening.clear();
        v.paired = None;
        v.pairing_open = false;
        v.preflight = Preflight { claude_on_path: false, sign_in: None };
        v.storage_problem = Some("unreadable".into());
        v.sessions.clear();
        v.projects.clear();
        let text = format_status(&v);
        assert!(text.contains("listening on nothing"), "{text}");
        assert!(text.contains("pairing: closed") && text.contains("NOT SAVING (unreadable)"), "{text}");
        assert!(text.contains("claude setup-token"), "{text}");
        assert!(text.contains("sessions: none") && text.contains("projects: none configured"), "{text}");
    }

    #[test]
    fn the_queue_hands_out_in_order_and_gives_up() {
        let q: PairQueue<u32> = PairQueue::default();
        assert!(q.push(1) && q.push(2));
        assert_eq!(q.next(Duration::from_millis(10)), Some(1));
        assert_eq!(q.next(Duration::from_millis(10)), Some(2));
        assert_eq!(q.next(Duration::from_millis(10)), None);
        for i in 0..MAX_QUEUED as u32 {
            assert!(q.push(i));
        }
        assert!(!q.push(99));
        q.clear();
        assert_eq!(q.next(Duration::ZERO), None);
    }

    #[test]
    fn dates_are_utc() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(951_782_400_000), "2000-02-29");
        assert_eq!(utc_date(1_798_761_600_000), "2027-01-01");
    }
}

//! `LinkHost`: the listener side of the link. It binds only the addresses it is given,
//! rate-limits handshakes per source, and hands each accepted socket to `conn`. Pairing is
//! driven from outside: `open_pairing` makes the offer a QR carries, and a request that
//! proves the secret waits until `confirm_pairing` or `reject_pairing`.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::LinkConfig;
use super::clients::{Client, ClientStore, DEFAULT_SCOPES, StoreError};
use super::conn::{self, IdCache, IdCaches, LiveFlags};
use super::keys::{KeyError, KeyProvider, Psk, PublicKey, SecretKey};
use super::pairing::{PairingOffer, PairingWindow};
use crate::session::{Sessions, now_ms};
use crate::wire::Scope;

/// How often an accept thread looks at its stop flag.
const ACCEPT_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindError {
    /// `0.0.0.0` or `::`: every interface at once. Not allowed; name the interface.
    Unspecified(SocketAddr),
}

impl fmt::Display for BindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BindError::Unspecified(a) => write!(f, "{a} listens on every interface; name the one to use"),
        }
    }
}

impl std::error::Error for BindError {}

/// Refuse an unspecified address before any socket is touched.
pub fn validate_binds(binds: &[SocketAddr]) -> Result<(), BindError> {
    match binds.iter().find(|a| a.ip().is_unspecified()) {
        Some(a) => Err(BindError::Unspecified(*a)),
        None => Ok(()),
    }
}

#[derive(Debug)]
pub enum LinkError {
    Bind(BindError),
    Io(std::io::ErrorKind),
    Keys(KeyError),
    Clients(StoreError),
    AlreadyRunning,
    NotRunning,
    /// Nothing listens, so there is no address to put in an offer.
    NoAddress,
    /// No pairing request with that id is waiting.
    UnknownPairing,
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinkError::Bind(e) => e.fmt(f),
            LinkError::Io(kind) => write!(f, "could not listen ({kind})"),
            LinkError::Keys(e) => e.fmt(f),
            LinkError::Clients(e) => e.fmt(f),
            LinkError::AlreadyRunning => f.write_str("the link is already running"),
            LinkError::NotRunning => f.write_str("the link is not running"),
            LinkError::NoAddress => f.write_str("the link has no address to offer"),
            LinkError::UnknownPairing => f.write_str("no such pairing request"),
        }
    }
}

impl std::error::Error for LinkError {}

impl From<StoreError> for LinkError {
    fn from(e: StoreError) -> Self {
        LinkError::Clients(e)
    }
}

/// What the host's log hears. Method names and client ids only: never an argument, a frame
/// or a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkLog {
    Listening { addr: SocketAddr },
    /// A connection was dropped before any handshake: too many, or too often from one source.
    Refused { ip: IpAddr },
    PairingRequested { id: String },
    Paired { client_id: String },
    Forbidden { client_id: String, method: &'static str },
    Revoked { client_id: String },
    /// An event queue overflowed and the connection was closed.
    Lagging { client_id: String },
}

/// A client that proved the pairing secret and waits for the host's say-so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPairing {
    pub id: String,
    pub client_name: String,
    /// `PublicKey::fingerprint` of the client's key: what the host shows to compare.
    pub fingerprint: String,
}

/// Called from connection threads: keep both quick and never call back into `LinkHost`
/// from them (post to another thread instead).
pub struct LinkCallbacks {
    pub on_pairing_request: Box<dyn Fn(PendingPairing) + Send + Sync>,
    pub log: Box<dyn Fn(LinkLog) + Send + Sync>,
}

impl LinkCallbacks {
    pub fn silent() -> Self {
        Self { on_pairing_request: Box::new(|_| {}), log: Box::new(|_| {}) }
    }
}

pub(crate) enum Decision {
    Confirmed(Client),
    Rejected,
}

pub(crate) struct PendingEntry {
    pub(crate) key: PublicKey,
    pub(crate) name: String,
    pub(crate) tx: SyncSender<Decision>,
}

/// Handshake attempts per source in the last minute.
#[derive(Default)]
struct RateLimiter {
    hits: HashMap<IpAddr, VecDeque<Instant>>,
}

impl RateLimiter {
    const WINDOW: Duration = Duration::from_secs(60);
    const MAX_SOURCES: usize = 1024;

    fn allow(&mut self, ip: IpAddr, now: Instant, per_minute: u32) -> bool {
        if self.hits.len() >= Self::MAX_SOURCES && !self.hits.contains_key(&ip) {
            self.hits.retain(|_, q| q.back().is_some_and(|t| now.saturating_duration_since(*t) < Self::WINDOW));
            if self.hits.len() >= Self::MAX_SOURCES {
                return false;
            }
        }
        let q = self.hits.entry(ip).or_default();
        while q.front().is_some_and(|t| now.saturating_duration_since(*t) >= Self::WINDOW) {
            q.pop_front();
        }
        if q.len() >= per_minute as usize {
            return false;
        }
        q.push_back(now);
        true
    }
}

/// State shared by the accept threads and every connection.
pub(crate) struct Shared {
    pub(crate) sessions: Arc<Sessions>,
    pub(crate) config: LinkConfig,
    pub(crate) callbacks: Arc<LinkCallbacks>,
    pub(crate) host_key: SecretKey,
    pub(crate) host_pub: PublicKey,
    store: Mutex<ClientStore>,
    window: Mutex<Option<PairingWindow>>,
    pending: Mutex<HashMap<String, PendingEntry>>,
    next_pending: AtomicU64,
    live: LiveFlags,
    id_caches: IdCaches,
    limiter: Mutex<RateLimiter>,
    connections: AtomicUsize,
    stop: AtomicBool,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    pub(crate) fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    pub(crate) fn log(&self, entry: LinkLog) {
        (self.callbacks.log)(entry);
    }

    pub(crate) fn pairing_open(&self) -> bool {
        lock(&self.window).as_ref().is_some_and(PairingWindow::is_open)
    }

    pub(crate) fn pairing_psk(&self) -> Option<Psk> {
        lock(&self.window).as_ref().and_then(PairingWindow::psk)
    }

    /// Close the window for the one caller that finds it open.
    pub(crate) fn consume_pairing(&self) -> bool {
        lock(&self.window).as_mut().is_some_and(PairingWindow::consume)
    }

    pub(crate) fn paired_keys(&self) -> Vec<PublicKey> {
        lock(&self.store).keys()
    }

    pub(crate) fn client_by_key(&self, key: &PublicKey) -> Option<Client> {
        lock(&self.store).find_by_key(key).cloned()
    }

    pub(crate) fn client_by_id(&self, id: &str) -> Option<Client> {
        lock(&self.store).get(id).cloned()
    }

    pub(crate) fn next_pending_id(&self) -> String {
        format!("p{}", self.next_pending.fetch_add(1, Ordering::SeqCst) + 1)
    }

    pub(crate) fn add_pending(&self, id: String, entry: PendingEntry) {
        lock(&self.pending).insert(id, entry);
    }

    pub(crate) fn remove_pending(&self, id: &str) {
        lock(&self.pending).remove(id);
    }

    /// Note a live connection for this client and tell the one it replaces to go.
    pub(crate) fn register_live(&self, client_id: &str) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        if let Some(old) = lock(&self.live).insert(client_id.to_string(), Arc::clone(&flag)) {
            old.store(true, Ordering::SeqCst);
        }
        flag
    }

    pub(crate) fn unregister_live(&self, client_id: &str, flag: &Arc<AtomicBool>) {
        let mut live = lock(&self.live);
        if live.get(client_id).is_some_and(|f| Arc::ptr_eq(f, flag)) {
            live.remove(client_id);
        }
    }

    pub(crate) fn id_cache(&self, client_id: &str) -> MappedCache<'_> {
        MappedCache { guard: lock(&self.id_caches), key: client_id.to_string() }
    }

    fn connection_closed(&self) {
        self.connections.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A client's `IdCache`, locked for one call: `begin`, `finish` and `forget` go through it.
pub(crate) struct MappedCache<'a> {
    guard: MutexGuard<'a, HashMap<String, IdCache>>,
    key: String,
}

impl MappedCache<'_> {
    pub(crate) fn begin(&mut self, id: &str) -> conn::Begin {
        self.guard.entry(self.key.clone()).or_default().begin(id)
    }

    pub(crate) fn finish(&mut self, id: &str, result: Result<serde_json::Value, crate::wire::ErrorBody>) {
        self.guard.entry(self.key.clone()).or_default().finish(id, result);
    }

    pub(crate) fn forget(&mut self, id: &str) {
        self.guard.entry(self.key.clone()).or_default().forget(id);
    }
}

struct Running {
    shared: Arc<Shared>,
    addrs: Vec<SocketAddr>,
    accept: Vec<JoinHandle<()>>,
}

pub struct LinkHost {
    sessions: Arc<Sessions>,
    config: LinkConfig,
    keys: Box<dyn KeyProvider>,
    callbacks: Arc<LinkCallbacks>,
    running: Mutex<Option<Running>>,
}

impl LinkHost {
    /// Nothing is read, created or opened until `start`.
    pub fn new(sessions: Arc<Sessions>, config: LinkConfig, keys: Box<dyn KeyProvider>, callbacks: LinkCallbacks) -> Self {
        Self { sessions, config, keys, callbacks: Arc::new(callbacks), running: Mutex::new(None) }
    }

    /// Bind every address in `config.binds` and start serving. An empty list opens nothing,
    /// reads no file and returns an empty list. Otherwise returns the addresses actually
    /// bound (a port of 0 resolves). A bad address, a key or clients file that cannot be
    /// used, or a failed bind starts nothing.
    pub fn start(&self) -> Result<Vec<SocketAddr>, LinkError> {
        let mut running = lock(&self.running);
        if running.is_some() {
            return Err(LinkError::AlreadyRunning);
        }
        validate_binds(&self.config.binds).map_err(LinkError::Bind)?;
        if self.config.binds.is_empty() {
            return Ok(Vec::new());
        }
        let host_key = self.keys.load_or_create().map_err(LinkError::Keys)?;
        let store = ClientStore::open(&self.config.link_dir)?;
        let mut listeners = Vec::new();
        for addr in &self.config.binds {
            let listener = TcpListener::bind(addr).map_err(|e| LinkError::Io(e.kind()))?;
            listener.set_nonblocking(true).map_err(|e| LinkError::Io(e.kind()))?;
            listeners.push(listener);
        }
        let addrs: Vec<SocketAddr> = listeners.iter().filter_map(|l| l.local_addr().ok()).collect();
        let shared = Arc::new(Shared {
            sessions: Arc::clone(&self.sessions),
            config: self.config.clone(),
            callbacks: Arc::clone(&self.callbacks),
            host_pub: host_key.public(),
            host_key,
            store: Mutex::new(store),
            window: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            next_pending: AtomicU64::new(0),
            live: Mutex::new(HashMap::new()),
            id_caches: Mutex::new(HashMap::new()),
            limiter: Mutex::new(RateLimiter::default()),
            connections: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
            threads: Mutex::new(Vec::new()),
        });
        let accept = listeners
            .into_iter()
            .map(|listener| {
                let shared = Arc::clone(&shared);
                std::thread::Builder::new().name("link-accept".into()).spawn(move || accept_loop(&shared, &listener))
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| LinkError::Io(e.kind()))?;
        for addr in &addrs {
            shared.log(LinkLog::Listening { addr: *addr });
        }
        *running = Some(Running { shared, addrs: addrs.clone(), accept });
        Ok(addrs)
    }

    /// Close the listeners and every connection and wait for their threads. Pending pairing
    /// requests are dropped. A stopped host may be started again.
    pub fn stop(&self) {
        let Some(run) = lock(&self.running).take() else {
            return;
        };
        run.shared.stop.store(true, Ordering::SeqCst);
        for h in run.accept {
            let _ = h.join();
        }
        // The accept threads are done, so no connection thread is added from here on.
        let conns: Vec<_> = lock(&run.shared.threads).drain(..).collect();
        for h in conns {
            let _ = h.join();
        }
        lock(&run.shared.pending).clear();
        *lock(&run.shared.window) = None;
    }

    pub fn is_running(&self) -> bool {
        lock(&self.running).is_some()
    }

    pub fn local_addrs(&self) -> Vec<SocketAddr> {
        lock(&self.running).as_ref().map(|r| r.addrs.clone()).unwrap_or_default()
    }

    fn shared(&self) -> Result<Arc<Shared>, LinkError> {
        lock(&self.running).as_ref().map(|r| Arc::clone(&r.shared)).ok_or(LinkError::NotRunning)
    }

    /// Open a pairing window (replacing any open one) and return what the QR carries.
    pub fn open_pairing(&self) -> Result<PairingOffer, LinkError> {
        let shared = self.shared()?;
        let addrs = self.local_addrs();
        if addrs.is_empty() {
            return Err(LinkError::NoAddress);
        }
        let window = PairingWindow::open(shared.config.limits.pairing_ttl).map_err(LinkError::Keys)?;
        let offer = window.offer(&shared.config.host_name, &shared.host_pub, &addrs);
        *lock(&shared.window) = Some(window);
        Ok(offer)
    }

    pub fn close_pairing(&self) {
        if let Ok(shared) = self.shared() {
            *lock(&shared.window) = None;
        }
    }

    /// Accept a waiting request: store the client with `scopes` (or read and answer when
    /// `None`) and let its connection carry on.
    pub fn confirm_pairing(&self, id: &str, scopes: Option<&[Scope]>) -> Result<Client, LinkError> {
        let shared = self.shared()?;
        let entry = lock(&shared.pending).remove(id).ok_or(LinkError::UnknownPairing)?;
        let granted = scopes.unwrap_or(&DEFAULT_SCOPES);
        let client = lock(&shared.store).add(&entry.name, entry.key, granted, now_ms())?;
        if entry.tx.send(Decision::Confirmed(client.clone())).is_err() {
            // Its connection gave up first: do not keep a client that never got a welcome.
            let _ = lock(&shared.store).revoke(&client.id);
            return Err(LinkError::UnknownPairing);
        }
        Ok(client)
    }

    /// Refuse a waiting request. Its socket closes with no message.
    pub fn reject_pairing(&self, id: &str) -> Result<(), LinkError> {
        let shared = self.shared()?;
        let entry = lock(&shared.pending).remove(id).ok_or(LinkError::UnknownPairing)?;
        let _ = entry.tx.send(Decision::Rejected);
        Ok(())
    }

    pub fn list_clients(&self) -> Result<Vec<Client>, LinkError> {
        Ok(lock(&self.shared()?.store).list().to_vec())
    }

    /// Takes effect on live connections at their next call.
    pub fn set_scopes(&self, client_id: &str, scopes: &[Scope]) -> Result<(), LinkError> {
        Ok(lock(&self.shared()?.store).set_scopes(client_id, scopes)?)
    }

    /// Delete the client's key. A live connection gets `error{revoked}` and is closed.
    pub fn revoke(&self, client_id: &str) -> Result<(), LinkError> {
        let shared = self.shared()?;
        lock(&shared.store).revoke(client_id)?;
        lock(&shared.id_caches).remove(client_id);
        Ok(())
    }
}

impl Drop for LinkHost {
    fn drop(&mut self) {
        self.stop();
    }
}

fn accept_loop(shared: &Arc<Shared>, listener: &TcpListener) {
    while !shared.stopped() {
        match listener.accept() {
            Ok((stream, peer)) => {
                let limits = &shared.config.limits;
                let allowed = lock(&shared.limiter).allow(peer.ip(), Instant::now(), limits.handshakes_per_minute);
                let room = shared.connections.fetch_add(1, Ordering::SeqCst) < limits.max_connections;
                if !allowed || !room {
                    shared.connections.fetch_sub(1, Ordering::SeqCst);
                    shared.log(LinkLog::Refused { ip: peer.ip() });
                    drop(stream);
                    continue;
                }
                let conn_shared = Arc::clone(shared);
                let spawned = std::thread::Builder::new().name("link-conn".into()).spawn(move || {
                    conn::serve(&conn_shared, stream);
                    conn_shared.connection_closed();
                });
                match spawned {
                    Ok(handle) => {
                        let mut threads = lock(&shared.threads);
                        threads.retain(|h| !h.is_finished());
                        threads.push(handle);
                    }
                    Err(_) => shared.connection_closed(),
                }
            }
            Err(_) => std::thread::sleep(ACCEPT_POLL),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::keys::FileKeyProvider;
    use crate::session::process::Proc;
    use crate::session::store::StorageStatus;
    use crate::session::transcript::TranscriptStore;

    fn sa(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn unspecified_addresses_are_refused_before_any_socket_call() {
        for bad in ["0.0.0.0:7000", "0.0.0.0:0", "[::]:7000", "[::]:0"] {
            assert_eq!(validate_binds(&[sa(bad)]), Err(BindError::Unspecified(sa(bad))), "{bad}");
        }
        // One bad address among good ones refuses the lot.
        assert!(validate_binds(&[sa("127.0.0.1:0"), sa("0.0.0.0:0")]).is_err());
        assert!(validate_binds(&[sa("127.0.0.1:0"), sa("192.168.1.5:7000"), sa("[::1]:0")]).is_ok());
        assert!(validate_binds(&[]).is_ok());
    }

    fn host(binds: Vec<SocketAddr>) -> (LinkHost, std::path::PathBuf) {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("orchestra-server-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&dir);
        let sessions = Sessions::with_writer(
            Box::new(|_| {}),
            Box::new(|_, _, _| -> Result<Arc<dyn Proc>, String> { Err("no process".into()) }),
            Box::new(|_, _| {}),
            None,
            TranscriptStore::memory(),
            dir.join("worktrees"),
        );
        let mut config = LinkConfig::new("Test", dir.join("link"), StorageStatus::ok());
        config.binds = binds;
        let keys = Box::new(FileKeyProvider::new(&config.link_dir));
        (LinkHost::new(sessions, config, keys, LinkCallbacks::silent()), dir)
    }

    #[test]
    fn start_refuses_an_unspecified_bind_and_opens_nothing() {
        let (h, dir) = host(vec![sa("127.0.0.1:0"), sa("0.0.0.0:0")]);
        assert!(matches!(h.start(), Err(LinkError::Bind(_))));
        assert!(!h.is_running());
        assert!(!dir.join("link").exists(), "no key was created");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_binds_start_reads_and_creates_nothing() {
        let (h, dir) = host(vec![]);
        assert_eq!(h.start().unwrap(), vec![]);
        assert!(!h.is_running());
        assert!(!dir.join("link").exists());
        assert!(matches!(h.open_pairing(), Err(LinkError::NotRunning)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn start_binds_loopback_reports_the_port_and_stops_cleanly() {
        let (h, dir) = host(vec![sa("127.0.0.1:0")]);
        let addrs = h.start().unwrap();
        assert_eq!(addrs.len(), 1);
        assert_ne!(addrs[0].port(), 0);
        assert!(matches!(h.start(), Err(LinkError::AlreadyRunning)));
        assert!(std::net::TcpStream::connect(addrs[0]).is_ok());
        h.stop();
        assert!(!h.is_running());
        assert!(std::net::TcpStream::connect(addrs[0]).is_err());
        // It can start again.
        assert_eq!(h.start().unwrap().len(), 1);
        h.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_clients_file_with_an_unknown_schema_stops_the_link_from_starting() {
        let (h, dir) = host(vec![sa("127.0.0.1:0")]);
        std::fs::create_dir_all(dir.join("link")).unwrap();
        std::fs::write(dir.join("link").join("clients.json"), r#"{"schema":9,"clients":[]}"#).unwrap();
        assert!(matches!(h.start(), Err(LinkError::Clients(StoreError::UnknownSchema(9)))));
        assert!(!h.is_running());
        assert_eq!(std::fs::read_to_string(dir.join("link").join("clients.json")).unwrap(), r#"{"schema":9,"clients":[]}"#);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rate_limiter_allows_n_per_minute_per_source() {
        let mut rl = RateLimiter::default();
        let (a, b): (IpAddr, IpAddr) = ("127.0.0.1".parse().unwrap(), "127.0.0.2".parse().unwrap());
        let t0 = Instant::now();
        for _ in 0..3 {
            assert!(rl.allow(a, t0, 3));
        }
        assert!(!rl.allow(a, t0 + Duration::from_secs(30), 3));
        assert!(rl.allow(b, t0, 3), "another source is unaffected");
        assert!(rl.allow(a, t0 + Duration::from_secs(61), 3), "the window slides");
    }
}

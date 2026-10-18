//! The phone link as the desktop runs it: state, lifecycle and the IPC commands.
//!
//! Rules this file keeps:
//! - The private host key never leaves the key provider, and the pairing secret reaches the
//!   webview only inside the QR image (`PairingView` has no secret field).
//! - Start, stop and reconfigure run on the blocking pool, serialised by `lifecycle`. No
//!   other lock is held across `LinkHost::stop`, which can wait for connection threads.
//! - The callbacks the host calls only push to a list and emit an event. They never call
//!   back into `LinkHost`: `stop()` from a connection thread would join itself.
//! - Errors are short strings with no paths and no key material.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use orchestra_core::link::clients::{Client, ClientStore};
use orchestra_core::link::keys::KeyProvider;
use orchestra_core::link::pairing::PairingOffer;
use orchestra_core::link::{LinkCallbacks, LinkConfig, LinkError, LinkHost, LinkLimits, LinkLog, PendingPairing};
use orchestra_core::wire::Scope;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::State;

use crate::commands::AppSessions;
use crate::link_config::{self, Interface, ReadOnly, Settings};
use crate::qr;
use crate::session::store::StorageStatus;

pub type LinkRef = Arc<LinkState>;
/// Sends a named event to the webview.
pub type Emit = Arc<dyn Fn(&str, Value) + Send + Sync>;
/// Picks the host key provider for a link folder. The product passes `keyring::provider_for`.
pub type KeyFactory = Arc<dyn Fn(&Path) -> Box<dyn KeyProvider> + Send + Sync>;

const EV_PAIRING_REQUEST: &str = "link-pairing-request";
const EV_STATUS: &str = "link-status-changed";
const EV_DEVICES: &str = "link-devices-changed";
const MAX_PENDING: usize = 16;

// ---- what the webview sees ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinkStatus {
    /// False when there is no app data folder to keep the link's files in.
    pub available: bool,
    pub os: &'static str,
    pub enabled: bool,
    pub running: bool,
    /// `ip:port` each, as bound.
    pub addrs: Vec<String>,
    pub port: u16,
    /// The saved addresses (bare IPs).
    pub binds: Vec<String>,
    /// Saved addresses skipped at start because they are not on this computer now.
    pub missing: Vec<String>,
    pub error: Option<String>,
    pub config_readonly: Option<String>,
    pub firewall_notice_seen: bool,
}

/// What the QR dialog needs. The offer's secret is inside `qr_svg` as pixels and nowhere
/// else: there is no secret field here and no way to add one without a test failing.
#[derive(Serialize)]
pub struct PairingView {
    pub qr_svg: String,
    pub host_name: String,
    pub addrs: Vec<String>,
    pub expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingView {
    pub id: String,
    pub client_name: String,
    pub fingerprint: String,
    /// Milliseconds since the Unix epoch.
    pub expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceView {
    pub id: String,
    pub name: String,
    pub fingerprint: String,
    pub scopes: Vec<ScopeArg>,
    pub paired_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScopeArg {
    Read,
    Answer,
    Control,
    Land,
}

impl From<ScopeArg> for Scope {
    fn from(s: ScopeArg) -> Scope {
        match s {
            ScopeArg::Read => Scope::Read,
            ScopeArg::Answer => Scope::Answer,
            ScopeArg::Control => Scope::Control,
            ScopeArg::Land => Scope::Land,
        }
    }
}

impl From<Scope> for ScopeArg {
    fn from(s: Scope) -> ScopeArg {
        match s {
            Scope::Read => ScopeArg::Read,
            Scope::Answer => ScopeArg::Answer,
            Scope::Control => ScopeArg::Control,
            Scope::Land => ScopeArg::Land,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetConfigArgs {
    pub enabled: bool,
    pub binds: Vec<String>,
    pub port: u32,
    pub firewall_notice_seen: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdArgs {
    pub id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmArgs {
    pub id: String,
    pub scopes: Vec<ScopeArg>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopesArgs {
    pub client_id: String,
    pub scopes: Vec<ScopeArg>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevokeArgs {
    pub client_id: String,
}

fn device_of(c: &Client) -> DeviceView {
    DeviceView {
        id: c.id.clone(),
        name: c.name.clone(),
        fingerprint: c.public_key.fingerprint(),
        scopes: c.scopes.iter().copied().map(ScopeArg::from).collect(),
        paired_at: c.paired_at,
    }
}

/// The QR dialog's data for an offer. The offer's JSON is drawn and dropped here.
fn view_of(offer: &PairingOffer) -> Result<PairingView, String> {
    let qr_svg = qr::svg_of(&offer.to_json())?;
    Ok(PairingView {
        qr_svg,
        host_name: offer.host_name.clone(),
        addrs: offer.addrs.clone(),
        expires_at: offer.expires_at,
    })
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The computer's name as shown to a phone, from the environment; no file or API is read.
pub fn host_name_from_env() -> String {
    let raw = std::env::var(if cfg!(windows) { "COMPUTERNAME" } else { "HOSTNAME" }).unwrap_or_default();
    let clean: String = raw.chars().filter(|c| !c.is_control()).take(64).collect();
    let clean = clean.trim().to_string();
    if clean.is_empty() { "Orchestra".to_string() } else { clean }
}

/// Short, path-free text for a start failure.
fn start_error_text(e: &LinkError, port: u16) -> String {
    use std::io::ErrorKind::{AddrInUse, AddrNotAvailable, PermissionDenied};
    match e {
        LinkError::Io(AddrInUse) => format!("Port {port} is already in use. Pick another port."),
        LinkError::Io(AddrNotAvailable) => "One of the saved addresses is not available.".to_string(),
        LinkError::Io(PermissionDenied) => format!("Not allowed to listen on port {port}."),
        LinkError::Keys(_) => "Could not open the host key.".to_string(),
        other => other.to_string(),
    }
}

// ---- state ----

struct Slot {
    host: Option<Arc<LinkHost>>,
    settings: Settings,
    read_only: Option<ReadOnly>,
    error: Option<String>,
    missing: Vec<String>,
    addrs: Vec<String>,
}

pub struct LinkState {
    sessions: AppSessions,
    storage: StorageStatus,
    /// `<app data>/link`; `None` when there is no app data folder.
    link_dir: Option<PathBuf>,
    host_name: String,
    keys: KeyFactory,
    emit: Emit,
    /// Serialises start, stop, reconfigure and offline edits of the clients file.
    lifecycle: Mutex<()>,
    slot: Mutex<Slot>,
    /// Leaf lock: held only to push, prune or copy.
    pending: Arc<Mutex<Vec<PendingView>>>,
}

impl LinkState {
    pub fn new(
        sessions: AppSessions,
        storage: StorageStatus,
        link_dir: Option<PathBuf>,
        host_name: String,
        keys: KeyFactory,
        emit: Emit,
    ) -> Arc<Self> {
        let (settings, read_only) = match &link_dir {
            Some(dir) => link_config::load(dir),
            None => (Settings::default(), None),
        };
        Arc::new(Self {
            sessions,
            storage,
            link_dir,
            host_name,
            keys,
            emit,
            lifecycle: Mutex::new(()),
            slot: Mutex::new(Slot { host: None, settings, read_only, error: None, missing: Vec::new(), addrs: Vec::new() }),
            pending: Arc::new(Mutex::new(Vec::new())),
        })
    }

    pub fn status(&self) -> LinkStatus {
        let slot = lock(&self.slot);
        LinkStatus {
            available: self.link_dir.is_some(),
            os: if cfg!(windows) {
                "windows"
            } else if cfg!(target_os = "macos") {
                "macos"
            } else {
                "linux"
            },
            enabled: slot.settings.enabled,
            running: slot.host.is_some(),
            addrs: slot.addrs.clone(),
            port: slot.settings.port,
            binds: slot.settings.binds.clone(),
            missing: slot.missing.clone(),
            error: slot.error.clone(),
            config_readonly: slot.read_only.map(|r| r.message().to_string()),
            firewall_notice_seen: slot.settings.firewall_notice_seen,
        }
    }

    fn emit_status(&self) {
        if let Ok(v) = serde_json::to_value(self.status()) {
            (self.emit)(EV_STATUS, v);
        }
    }

    fn emit_devices(&self) {
        if let Ok(devices) = self.devices() {
            if let Ok(v) = serde_json::to_value(devices) {
                (self.emit)(EV_DEVICES, v);
            }
        }
    }

    /// What the host calls from its threads. Pushes and emits; never touches `LinkHost`
    /// (a refresh of the device list is handed to a short thread of its own).
    fn callbacks(self: &Arc<Self>) -> LinkCallbacks {
        let pending = Arc::clone(&self.pending);
        let emit = Arc::clone(&self.emit);
        let weak: Weak<LinkState> = Arc::downgrade(self);
        let confirm_ms = LinkLimits::default().confirm_timeout.as_millis() as u64;
        LinkCallbacks {
            on_pairing_request: Box::new(move |p: PendingPairing| {
                let view = PendingView {
                    id: p.id,
                    client_name: p.client_name,
                    fingerprint: p.fingerprint,
                    expires_at: now_ms() + confirm_ms,
                };
                {
                    let mut list = lock(&pending);
                    list.push(view.clone());
                    if list.len() > MAX_PENDING {
                        list.remove(0);
                    }
                }
                if let Ok(v) = serde_json::to_value(&view) {
                    emit(EV_PAIRING_REQUEST, v);
                }
            }),
            // Ids and method names only, by the host's own rule.
            log: Box::new(move |entry: LinkLog| {
                eprintln!("link: {entry:?}");
                if matches!(entry, LinkLog::Paired { .. } | LinkLog::Revoked { .. }) {
                    let weak = weak.clone();
                    let _ = std::thread::Builder::new().name("link-refresh".into()).spawn(move || {
                        if let Some(state) = weak.upgrade() {
                            state.emit_devices();
                        }
                    });
                }
            }),
        }
    }

    // ---- lifecycle ----

    /// Stop the running host, then start one for the saved settings if they ask for it.
    /// The caller holds `lifecycle`. Blocks; call it off the main thread.
    fn apply(self: &Arc<Self>) {
        let old = lock(&self.slot).host.take();
        if let Some(old) = old {
            old.stop();
        }
        lock(&self.pending).clear();
        let (settings, read_only) = {
            let slot = lock(&self.slot);
            (slot.settings.clone(), slot.read_only)
        };
        let mut error = None;
        let mut missing = Vec::new();
        let mut addrs = Vec::new();
        let mut started = None;
        if settings.enabled && read_only.is_none() {
            let (present, skipped) = link_config::split_binds(&settings.binds, &link_config::interfaces());
            missing = skipped;
            match (&self.link_dir, present.is_empty()) {
                (None, _) => error = Some("The link needs an app data folder, and there is none.".to_string()),
                (_, true) => error = Some("None of the saved addresses is on this computer right now.".to_string()),
                (Some(dir), false) => {
                    let mut config = LinkConfig::new(self.host_name.clone(), dir.clone(), self.storage.clone());
                    config.binds = link_config::socket_addrs(&present, settings.port);
                    let host = Arc::new(LinkHost::new(
                        Arc::clone(&self.sessions),
                        config,
                        (self.keys)(dir),
                        self.callbacks(),
                    ));
                    match host.start() {
                        Ok(bound) => {
                            addrs = bound.iter().map(|a| a.to_string()).collect();
                            started = Some(host);
                        }
                        Err(e) => error = Some(start_error_text(&e, settings.port)),
                    }
                }
            }
        }
        {
            let mut slot = lock(&self.slot);
            slot.host = started;
            slot.error = error;
            slot.missing = missing;
            slot.addrs = addrs;
        }
        self.emit_status();
    }

    /// Start the link in the background if the saved settings ask for it.
    pub fn auto_start(self: &Arc<Self>) {
        if !lock(&self.slot).settings.enabled {
            return;
        }
        let state = Arc::clone(self);
        let _ = std::thread::Builder::new().name("link-start".into()).spawn(move || {
            let _guard = lock(&state.lifecycle);
            state.apply();
        });
    }

    pub fn set_config(self: &Arc<Self>, args: &SetConfigArgs) -> Result<LinkStatus, String> {
        let _guard = lock(&self.lifecycle);
        let Some(dir) = &self.link_dir else {
            return Err("The link needs an app data folder, and there is none.".to_string());
        };
        let (prev, read_only) = {
            let slot = lock(&self.slot);
            (slot.settings.clone(), slot.read_only)
        };
        if let Some(r) = read_only {
            return Err(format!("The link settings are read-only: the file was {}.", r.message()));
        }
        let new = link_config::validate(
            &prev,
            args.enabled,
            &args.binds,
            args.port,
            args.firewall_notice_seen,
            &link_config::interfaces(),
        )?;
        let running = lock(&self.slot).host.is_some();
        if new == prev && running == new.enabled {
            return Ok(self.status());
        }
        link_config::save(dir, &new).map_err(|_| "Could not save the link settings.".to_string())?;
        lock(&self.slot).settings = new;
        self.apply();
        Ok(self.status())
    }

    /// Stop the host, waiting at most `wait`. For app exit: the stop runs on its own thread
    /// and is abandoned if it is slow, because the process is going away.
    pub fn shutdown(&self, wait: Duration) {
        let Some(host) = lock(&self.slot).host.take() else { return };
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new().name("link-exit".into()).spawn(move || {
            host.stop();
            let _ = tx.send(());
        });
        if spawned.is_ok() {
            let _ = rx.recv_timeout(wait);
        }
    }

    fn host(&self) -> Option<Arc<LinkHost>> {
        lock(&self.slot).host.clone()
    }

    // ---- pairing ----

    pub fn open_pairing(&self) -> Result<PairingView, String> {
        let host = self.host().ok_or_else(|| "The link is not running.".to_string())?;
        if !self.pending().is_empty() {
            return Err("Answer the waiting request first.".to_string());
        }
        let offer = host.open_pairing().map_err(|e| e.to_string())?;
        match view_of(&offer) {
            Ok(view) => Ok(view),
            Err(e) => {
                host.close_pairing();
                Err(e)
            }
        }
    }

    pub fn close_pairing(&self) {
        if let Some(host) = self.host() {
            host.close_pairing();
        }
    }

    /// Requests still waiting; expired ones are dropped on the way.
    pub fn pending(&self) -> Vec<PendingView> {
        let now = now_ms();
        let mut list = lock(&self.pending);
        list.retain(|p| p.expires_at > now);
        list.clone()
    }

    fn forget_pending(&self, id: &str) {
        lock(&self.pending).retain(|p| p.id != id);
    }

    pub fn confirm(&self, id: &str, scopes: &[ScopeArg]) -> Result<DeviceView, String> {
        if scopes.is_empty() {
            return Err("Pick at least one thing the device may do.".to_string());
        }
        let host = self.host().ok_or_else(|| "The link is not running.".to_string())?;
        let granted: Vec<Scope> = scopes.iter().copied().map(Scope::from).collect();
        let result = host.confirm_pairing(id, Some(&granted));
        // The host took or lost the request either way: it cannot be answered again.
        self.forget_pending(id);
        let client = result.map_err(|e| e.to_string())?;
        self.emit_devices();
        Ok(device_of(&client))
    }

    pub fn reject(&self, id: &str) -> Result<(), String> {
        let host = self.host().ok_or_else(|| "The link is not running.".to_string())?;
        let result = host.reject_pairing(id);
        self.forget_pending(id);
        result.map_err(|e| e.to_string())
    }

    // ---- devices ----

    /// Run `f` on the clients through the running host, or, with the link off, on the file
    /// itself under the lifecycle lock so a lost phone can be removed without opening a socket.
    fn with_clients<T>(
        &self,
        online: impl FnOnce(&LinkHost) -> Result<T, LinkError>,
        offline: impl FnOnce(&mut ClientStore) -> Result<T, orchestra_core::link::clients::StoreError>,
    ) -> Result<T, String> {
        if let Some(host) = self.host() {
            return online(&host).map_err(|e| e.to_string());
        }
        let _guard = lock(&self.lifecycle);
        // Start and stop need the lifecycle lock, so the answer cannot change from here.
        if let Some(host) = self.host() {
            return online(&host).map_err(|e| e.to_string());
        }
        let Some(dir) = &self.link_dir else {
            return Err("The link needs an app data folder, and there is none.".to_string());
        };
        let mut store = ClientStore::open(dir).map_err(|e| e.to_string())?;
        offline(&mut store).map_err(|e| e.to_string())
    }

    pub fn devices(&self) -> Result<Vec<DeviceView>, String> {
        let clients = self.with_clients(|h| h.list_clients(), |s| Ok(s.list().to_vec()))?;
        Ok(clients.iter().map(device_of).collect())
    }

    pub fn set_scopes(&self, client_id: &str, scopes: &[ScopeArg]) -> Result<(), String> {
        if scopes.is_empty() {
            return Err("Pick at least one thing the device may do.".to_string());
        }
        let granted: Vec<Scope> = scopes.iter().copied().map(Scope::from).collect();
        self.with_clients(|h| h.set_scopes(client_id, &granted), |s| s.set_scopes(client_id, &granted))?;
        self.emit_devices();
        Ok(())
    }

    pub fn revoke(&self, client_id: &str) -> Result<(), String> {
        self.with_clients(|h| h.revoke(client_id), |s| s.revoke(client_id))?;
        self.emit_devices();
        Ok(())
    }
}

// ---- commands ----

fn join_error<T>(r: Result<Result<T, String>, tauri::Error>) -> Result<T, String> {
    r.map_err(|_| "link task failed".to_string())?
}

/// Cheap: copies a few fields.
#[tauri::command]
pub fn link_status(link: State<'_, LinkRef>) -> LinkStatus {
    link.status()
}

/// Async because it asks the OS for its adapters.
#[tauri::command]
pub async fn link_interfaces() -> Result<Vec<Interface>, String> {
    join_error(tauri::async_runtime::spawn_blocking(|| Ok(link_config::interfaces())).await)
}

/// Async: stopping and starting the link waits for threads and sockets.
#[tauri::command]
pub async fn link_set_config(args: SetConfigArgs, link: State<'_, LinkRef>) -> Result<LinkStatus, String> {
    let link = Arc::clone(link.inner());
    join_error(tauri::async_runtime::spawn_blocking(move || link.set_config(&args)).await)
}

#[tauri::command]
pub async fn link_open_pairing(link: State<'_, LinkRef>) -> Result<PairingView, String> {
    let link = Arc::clone(link.inner());
    join_error(tauri::async_runtime::spawn_blocking(move || link.open_pairing()).await)
}

#[tauri::command]
pub async fn link_close_pairing(link: State<'_, LinkRef>) -> Result<(), String> {
    let link = Arc::clone(link.inner());
    join_error(tauri::async_runtime::spawn_blocking(move || Ok(link.close_pairing())).await)
}

/// Cheap: a copy of a short list under a leaf lock.
#[tauri::command]
pub fn link_pending(link: State<'_, LinkRef>) -> Vec<PendingView> {
    link.pending()
}

#[tauri::command]
pub async fn link_confirm_pairing(args: ConfirmArgs, link: State<'_, LinkRef>) -> Result<DeviceView, String> {
    let link = Arc::clone(link.inner());
    join_error(tauri::async_runtime::spawn_blocking(move || link.confirm(&args.id, &args.scopes)).await)
}

#[tauri::command]
pub async fn link_reject_pairing(args: IdArgs, link: State<'_, LinkRef>) -> Result<(), String> {
    let link = Arc::clone(link.inner());
    join_error(tauri::async_runtime::spawn_blocking(move || link.reject(&args.id)).await)
}

/// Async: with the link off this reads the clients file.
#[tauri::command]
pub async fn link_list_devices(link: State<'_, LinkRef>) -> Result<Vec<DeviceView>, String> {
    let link = Arc::clone(link.inner());
    join_error(tauri::async_runtime::spawn_blocking(move || link.devices()).await)
}

#[tauri::command]
pub async fn link_set_scopes(args: ScopesArgs, link: State<'_, LinkRef>) -> Result<(), String> {
    let link = Arc::clone(link.inner());
    join_error(tauri::async_runtime::spawn_blocking(move || link.set_scopes(&args.client_id, &args.scopes)).await)
}

#[tauri::command]
pub async fn link_revoke(args: RevokeArgs, link: State<'_, LinkRef>) -> Result<(), String> {
    let link = Arc::clone(link.inner());
    join_error(tauri::async_runtime::spawn_blocking(move || link.revoke(&args.client_id)).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestra_core::link::keys::FileKeyProvider;
    use orchestra_core::session::Sessions;
    use orchestra_core::session::transcript::TranscriptStore;
    use serde_json::json;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);

    struct Rig {
        state: Arc<LinkState>,
        dir: PathBuf,
        events: Arc<Mutex<Vec<(String, Value)>>>,
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            self.state.shutdown(Duration::from_secs(5));
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn rig_with_dir(with_dir: bool, prepare: impl FnOnce(&Path)) -> Rig {
        let root = std::env::temp_dir().join(format!("orchestra-linkstate-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&root);
        let link_dir = root.join("link");
        std::fs::create_dir_all(&link_dir).unwrap();
        prepare(&link_dir);
        let sessions = Sessions::with_writer(
            Box::new(|_| {}),
            Box::new(|_, _, _| Err("no processes in tests".to_string())),
            Box::new(|_, _| {}),
            None,
            TranscriptStore::memory(),
            root.join("worktrees"),
        );
        let events: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        // A file provider under the test folder: tests never touch the real Credential Manager entry.
        let keys: KeyFactory = Arc::new(|dir| Box::new(FileKeyProvider::new(dir)));
        let state = LinkState::new(
            sessions,
            StorageStatus::ok(),
            with_dir.then(|| link_dir.clone()),
            "Test desk".to_string(),
            keys,
            Arc::new(move |name, value| lock(&sink).push((name.to_string(), value))),
        );
        Rig { state, dir: root, events }
    }

    fn rig() -> Rig {
        rig_with_dir(true, |_| {})
    }

    /// A port nothing listens on right now (loopback only).
    fn free_port() -> u16 {
        TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    fn enable(rig: &Rig, port: u16) -> Result<LinkStatus, String> {
        rig.state.set_config(&SetConfigArgs { enabled: true, binds: vec!["127.0.0.1".into()], port: port as u32, firewall_notice_seen: false })
    }

    fn secret_free(text: &str, secret: &str) {
        assert!(!text.contains(secret), "the pairing secret leaked");
        assert!(!text.contains("\"secret\""), "a secret field exists");
    }

    #[test]
    fn link_status_starts_off_with_defaults() {
        let r = rig();
        let s = r.state.status();
        assert!(s.available && !s.enabled && !s.running);
        assert_eq!((s.port, s.binds.clone()), (47800, vec!["127.0.0.1".to_string()]));
        assert!(s.addrs.is_empty() && s.missing.is_empty() && s.error.is_none() && s.config_readonly.is_none());
        let v = serde_json::to_value(&s).unwrap();
        let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            ["addrs", "available", "binds", "config_readonly", "enabled", "error", "firewall_notice_seen", "missing", "os", "port", "running"]
        );
    }

    #[test]
    fn link_without_an_app_data_folder_is_unavailable() {
        let r = rig_with_dir(false, |_| {});
        assert!(!r.state.status().available);
        assert!(enable(&r, free_port()).is_err());
        assert!(r.state.devices().is_err());
    }

    #[test]
    fn link_pairing_view_has_no_secret() {
        let secret = "ab".repeat(32);
        let offer = PairingOffer {
            v: 1,
            host_id: "0123".into(),
            host_name: "Desk".into(),
            addrs: vec!["127.0.0.1:47800".into()],
            host_key: "cd".repeat(32),
            secret: secret.clone(),
            expires_at: 1_700_000_000_000,
        };
        let view = view_of(&offer).unwrap();
        let json = serde_json::to_value(&view).unwrap();
        let mut keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["addrs", "expires_at", "host_name", "qr_svg"]);
        secret_free(&json.to_string(), &secret);
        assert!(!view.qr_svg.contains("style"));
        assert!(view.qr_svg.starts_with("<?xml") || view.qr_svg.starts_with("<svg"));
    }

    #[test]
    fn link_enable_disable_and_exit_stop_the_host() {
        let r = rig();
        let port = free_port();
        let s = enable(&r, port).unwrap();
        assert!(s.enabled && s.running, "{:?}", s.error);
        assert_eq!(s.addrs, vec![format!("127.0.0.1:{port}")]);
        // The config was saved, off disk, not just in memory.
        let (saved, ro) = link_config::load(&r.dir.join("link"));
        assert_eq!((saved.enabled, saved.port, ro), (true, port, None));
        // The offer: running, so a QR; and it has no secret field.
        let view = r.state.open_pairing().unwrap();
        assert_eq!(view.addrs, vec![format!("127.0.0.1:{port}")]);
        assert!(serde_json::to_value(&view).unwrap().get("secret").is_none());
        r.state.close_pairing();
        let s = r.state.set_config(&SetConfigArgs { enabled: false, binds: vec!["127.0.0.1".into()], port: port as u32, firewall_notice_seen: false }).unwrap();
        assert!(!s.running && !s.enabled && s.addrs.is_empty());
        assert!(r.state.open_pairing().is_err());
        assert!(lock(&r.events).iter().any(|(n, _)| n == EV_STATUS));
        // Exit: stop is bounded and leaves nothing running.
        enable(&r, port).unwrap();
        let t = std::time::Instant::now();
        r.state.shutdown(Duration::from_secs(2));
        assert!(t.elapsed() < Duration::from_secs(3));
        assert!(!r.state.status().running);
    }

    #[test]
    fn link_a_busy_port_is_a_start_error_and_keeps_enabled() {
        let r = rig();
        let taken = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = taken.local_addr().unwrap().port();
        let s = enable(&r, port).unwrap();
        assert!(s.enabled && !s.running);
        assert!(s.error.unwrap().contains(&port.to_string()));
        assert_eq!(link_config::load(&r.dir.join("link")).0.enabled, true);
    }

    #[test]
    fn link_saved_binds_missing_now_are_skipped_and_the_file_is_kept() {
        let port = free_port();
        let saved = format!(
            r#"{{"schema":1,"enabled":true,"port":{port},"binds":["127.0.0.1","10.250.250.250"],"firewall_notice_seen":true}}"#
        );
        let r = rig_with_dir(true, |dir| std::fs::write(dir.join("config.json"), &saved).unwrap());
        assert!(r.state.status().enabled && !r.state.status().running, "not started until asked");
        r.state.auto_start();
        let t = std::time::Instant::now();
        while !r.state.status().running && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        let s = r.state.status();
        assert!(s.running, "{:?}", s.error);
        assert_eq!(s.addrs, vec![format!("127.0.0.1:{port}")]);
        assert_eq!(s.missing, vec!["10.250.250.250".to_string()]);
        assert_eq!(std::fs::read_to_string(r.dir.join("link").join("config.json")).unwrap(), saved, "not rewritten");
    }

    #[test]
    fn link_unreadable_config_is_off_read_only_and_refuses_changes() {
        let r = rig_with_dir(true, |dir| std::fs::write(dir.join("config.json"), r#"{"schema":9}"#).unwrap());
        let s = r.state.status();
        assert!(!s.enabled);
        assert_eq!(s.config_readonly.as_deref(), Some("written by a newer Orchestra"));
        assert!(enable(&r, free_port()).is_err());
        assert_eq!(std::fs::read_to_string(r.dir.join("link").join("config.json")).unwrap(), r#"{"schema":9}"#);
    }

    #[test]
    fn link_set_config_refuses_bad_requests_before_anything_changes() {
        let r = rig();
        let req = |binds: Vec<&str>, port: u32| SetConfigArgs {
            enabled: true,
            binds: binds.into_iter().map(String::from).collect(),
            port,
            firewall_notice_seen: true,
        };
        for bad in [req(vec!["0.0.0.0"], 47800), req(vec!["::"], 47800), req(vec!["::ffff:0.0.0.0"], 47800), req(vec!["127.0.0.1"], 80), req(vec!["10.250.250.250"], 47800), req(vec![], 47800)] {
            assert!(r.state.set_config(&bad).is_err(), "{bad:?}");
        }
        assert!(!r.state.status().enabled);
        assert!(!r.dir.join("link").join("config.json").exists(), "nothing was saved");
    }

    #[test]
    fn link_pairing_request_callback_only_records_and_emits() {
        let r = rig();
        let cb = r.state.callbacks();
        (cb.on_pairing_request)(PendingPairing { id: "p1".into(), client_name: "Pixel".into(), fingerprint: "aaaa-bbbb".into() });
        let pending = r.state.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!((pending[0].id.as_str(), pending[0].client_name.as_str()), ("p1", "Pixel"));
        assert!(pending[0].expires_at > now_ms() + 200_000);
        let events = lock(&r.events).clone();
        let (name, payload) = events.iter().find(|(n, _)| n == EV_PAIRING_REQUEST).unwrap();
        assert_eq!(name, EV_PAIRING_REQUEST);
        assert_eq!(payload["id"], json!("p1"));
        // With a request waiting, no new pairing window is offered.
        assert!(r.state.open_pairing().is_err());
        // Confirm and reject need a running link.
        assert!(r.state.confirm("p1", &[ScopeArg::Read]).is_err());
        assert!(r.state.confirm("p1", &[]).is_err());
    }

    #[test]
    fn link_expired_pending_requests_are_dropped_when_read() {
        let r = rig();
        lock(&r.state.pending).push(PendingView { id: "old".into(), client_name: "x".into(), fingerprint: "f".into(), expires_at: 1 });
        assert!(r.state.pending().is_empty());
    }

    #[test]
    fn link_devices_are_listed_edited_and_revoked_while_off() {
        use orchestra_core::link::keys::SecretKey;
        let r = rig();
        let dir = r.dir.join("link");
        let mut store = ClientStore::open(&dir).unwrap();
        let c = store.add("Pixel", SecretKey::generate().unwrap().public(), &[Scope::Read, Scope::Answer], 42).unwrap();
        drop(store);
        assert!(!r.state.status().running);
        let list = r.state.devices().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].name.as_str(), list[0].paired_at), ("Pixel", 42));
        assert_eq!(list[0].scopes, vec![ScopeArg::Read, ScopeArg::Answer]);
        assert_eq!(list[0].fingerprint, c.public_key.fingerprint());
        assert!(r.state.set_scopes(&c.id, &[]).is_err(), "at least one scope");
        r.state.set_scopes(&c.id, &[ScopeArg::Read, ScopeArg::Control]).unwrap();
        assert_eq!(r.state.devices().unwrap()[0].scopes, vec![ScopeArg::Read, ScopeArg::Control]);
        assert!(r.state.revoke("c-nope").is_err());
        r.state.revoke(&c.id).unwrap();
        assert!(r.state.devices().unwrap().is_empty());
        assert!(lock(&r.events).iter().any(|(n, _)| n == EV_DEVICES));
    }

    #[test]
    fn link_arg_structs_reject_extra_fields_and_bad_scopes() {
        assert!(serde_json::from_value::<SetConfigArgs>(json!({"enabled":true,"binds":["127.0.0.1"],"port":47800,"firewall_notice_seen":false})).is_ok());
        assert!(serde_json::from_value::<SetConfigArgs>(json!({"enabled":true,"binds":[],"port":1,"firewall_notice_seen":false,"host":"0.0.0.0"})).is_err());
        assert!(serde_json::from_value::<ConfirmArgs>(json!({"id":"p1","scopes":["read","answer"]})).is_ok());
        assert!(serde_json::from_value::<ConfirmArgs>(json!({"id":"p1","scopes":["root"]})).is_err());
        assert!(serde_json::from_value::<ConfirmArgs>(json!({"id":"p1","scopes":["Read"]})).is_err());
        assert!(serde_json::from_value::<ConfirmArgs>(json!({"id":"p1","scopes":[],"path":"C:/"})).is_err());
        assert!(serde_json::from_value::<IdArgs>(json!({"id":"p1","force":true})).is_err());
        assert!(serde_json::from_value::<ScopesArgs>(json!({"client_id":"c","scopes":["land"]})).is_ok());
        assert!(serde_json::from_value::<RevokeArgs>(json!({"client_id":"c","scopes":[]})).is_err());
    }

    #[test]
    fn link_scope_names_round_trip_to_the_wire_scopes() {
        for s in Scope::ALL {
            assert_eq!(Scope::from(ScopeArg::from(s)), s);
            assert_eq!(serde_json::to_value(ScopeArg::from(s)).unwrap(), json!(s.as_str()));
        }
    }

    #[test]
    fn link_host_name_is_clean_and_never_empty() {
        let name = host_name_from_env();
        assert!(!name.is_empty() && name.chars().count() <= 64 && !name.chars().any(char::is_control));
    }
}

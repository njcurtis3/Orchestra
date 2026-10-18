//! One connection, on its own thread: the WebSocket upgrade, the version exchange, the Noise
//! handshake, pairing confirmation, then the message loop. The loop reads with a short
//! timeout and, between reads, sends call results and events; nothing in it blocks on a peer
//! for longer than the write timeout. No frame, payload or argument is ever logged.

use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tungstenite::handshake::HandshakeError;
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Error as WsError, Message, WebSocket};

use super::clients::Client;
use super::frame;
use super::keys::PublicKey;
use super::noise::{Handshake, Mode, Transport, prologue};
use super::server::{Decision, LinkLog, PendingEntry, PendingPairing, Shared};
use crate::wire::message::{ErrorBody, ErrorMessage, HandshakeReply, Welcome};
use crate::wire::{
    self, ClientMessage, ErrorCode, HostEvent, HostMessage, MAX_MESSAGE_BYTES, Scope, Snapshot,
    SUPPORTED_VERSIONS, WireStorageStatus, decode_client, decode_handshake_request, encode_handshake_reply,
};

/// How long a read waits before the loop looks at everything else.
const POLL: Duration = Duration::from_millis(25);
/// The plaintext version frames are tiny; anything bigger is not a client.
const MAX_HANDSHAKE_FRAME: usize = 1024;
/// Events sent per pass of the loop, so reads stay responsive under a flood.
const EVENTS_PER_PASS: usize = 256;
/// Call ids remembered per client.
const ID_CACHE: usize = 256;

/// A `TcpStream` whose read timeout reads as `WouldBlock` on every OS: Windows reports a
/// timed-out read as `TimedOut`, and the WebSocket layer resumes only on `WouldBlock`.
pub(crate) struct Sock(pub(crate) TcpStream);

impl Read for Sock {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.0.read(buf) {
            Err(e) if e.kind() == io::ErrorKind::TimedOut => Err(io::ErrorKind::WouldBlock.into()),
            other => other,
        }
    }
}

impl Write for Sock {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

type Ws = WebSocket<Sock>;

enum Polled {
    Text(String),
    Binary(Vec<u8>),
    Nothing,
    Closed,
}

fn poll(ws: &mut Ws) -> Polled {
    match ws.read() {
        Ok(Message::Text(t)) => Polled::Text(t.as_str().to_string()),
        Ok(Message::Binary(b)) => Polled::Binary(b.to_vec()),
        Ok(Message::Close(_)) => Polled::Closed,
        Ok(_) => Polled::Nothing,
        Err(WsError::Io(e)) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
            Polled::Nothing
        }
        Err(_) => Polled::Closed,
    }
}

/// The next message of the wanted kind before `deadline`, or `None` on anything else.
fn wait(shared: &Shared, ws: &mut Ws, deadline: Instant, binary: bool) -> Option<Vec<u8>> {
    loop {
        if shared.stopped() || Instant::now() >= deadline {
            return None;
        }
        match poll(ws) {
            Polled::Text(t) if !binary && t.len() <= MAX_HANDSHAKE_FRAME => return Some(t.into_bytes()),
            Polled::Binary(b) if binary => return Some(b),
            Polled::Nothing => {}
            _ => return None,
        }
    }
}

fn send_raw(ws: &mut Ws, message: Message) -> bool {
    ws.send(message).is_ok()
}

/// Encode, chunk, encrypt and send one host message.
fn send(ws: &mut Ws, tr: &mut Transport, msg: &HostMessage) -> bool {
    let Ok(text) = wire::encode_host(msg) else {
        return false;
    };
    let Ok(sealed) = frame::seal(tr, text.as_bytes()) else {
        return false;
    };
    send_raw(ws, Message::binary(sealed))
}

fn error_message(code: ErrorCode, message: &str) -> HostMessage {
    HostMessage::Error(ErrorMessage { code: code.as_str().to_string(), message: message.to_string(), supported: None })
}

fn error_body(e: wire::WireError) -> ErrorBody {
    ErrorBody { code: e.code.as_str().to_string(), message: e.message }
}

// ---- the idempotency cache ----

enum Slot {
    Pending,
    Done(Result<Value, ErrorBody>),
}

pub(crate) enum Begin {
    Run,
    Replay(Result<Value, ErrorBody>),
    InProgress,
}

/// The last `ID_CACHE` call ids of one client, so an `answer` resent after a dropped link
/// applies once. Read calls are not cached: they are safe to repeat and can be large.
#[derive(Default)]
pub(crate) struct IdCache {
    order: VecDeque<String>,
    slots: HashMap<String, Slot>,
}

impl IdCache {
    pub(crate) fn begin(&mut self, id: &str) -> Begin {
        match self.slots.get(id) {
            Some(Slot::Done(r)) => Begin::Replay(r.clone()),
            Some(Slot::Pending) => Begin::InProgress,
            None => {
                if self.order.len() >= ID_CACHE {
                    if let Some(old) = self.order.pop_front() {
                        self.slots.remove(&old);
                    }
                }
                self.order.push_back(id.to_string());
                self.slots.insert(id.to_string(), Slot::Pending);
                Begin::Run
            }
        }
    }

    pub(crate) fn finish(&mut self, id: &str, result: Result<Value, ErrorBody>) {
        if let Some(slot) = self.slots.get_mut(id) {
            *slot = Slot::Done(result);
        }
    }

    pub(crate) fn forget(&mut self, id: &str) {
        self.slots.remove(id);
        self.order.retain(|i| i != id);
    }
}

// ---- entry ----

/// Serve one accepted socket until it closes. The caller counted the connection; it is
/// uncounted by `Shared::connection_closed` when this returns.
pub(crate) fn serve(shared: &Arc<Shared>, stream: TcpStream) {
    let limits = &shared.config.limits;
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_nodelay(true);
    if stream.set_read_timeout(Some(POLL)).is_err() || stream.set_write_timeout(Some(limits.write_timeout)).is_err() {
        return;
    }
    let Some((mut ws, mode)) = upgrade(shared, stream) else {
        return;
    };
    let Some(joined) = handshake(shared, &mut ws, mode) else {
        return;
    };
    run(shared, ws, joined);
}

fn not_found() -> ErrorResponse {
    let mut r = ErrorResponse::new(None);
    *r.status_mut() = tungstenite::http::StatusCode::NOT_FOUND;
    r
}

/// The WebSocket upgrade. Only `/link`, and `/pair` while a window is open, upgrade; any
/// other path is a plain 404, so a closed window looks like a path that does not exist.
fn upgrade(shared: &Shared, stream: TcpStream) -> Option<(Ws, Mode)> {
    let cap = frame::max_wire_bytes(MAX_MESSAGE_BYTES);
    let config = WebSocketConfig::default()
        .max_message_size(Some(cap))
        .max_frame_size(Some(cap))
        .read_buffer_size(16 * 1024)
        .write_buffer_size(16 * 1024);
    let chosen: Cell<Option<Mode>> = Cell::new(None);
    let callback = |req: &Request, resp: Response| match Mode::from_path(req.uri().path()) {
        Some(Mode::Pair) if !shared.pairing_open() => Err(not_found()),
        Some(mode) => {
            chosen.set(Some(mode));
            Ok(resp)
        }
        None => Err(not_found()),
    };
    let deadline = Instant::now() + shared.config.limits.handshake_timeout;
    let mut result = tungstenite::accept_hdr_with_config(Sock(stream), callback, Some(config));
    let ws = loop {
        match result {
            Ok(ws) => break ws,
            Err(HandshakeError::Interrupted(mid)) => {
                if shared.stopped() || Instant::now() >= deadline {
                    return None;
                }
                result = mid.handshake();
            }
            Err(HandshakeError::Failure(_)) => return None,
        }
    };
    chosen.get().map(|mode| (ws, mode))
}

/// Who finished the handshake and proved it, with the state to carry on.
struct Joined {
    transport: Transport,
    client: Client,
    /// A pairing, as opposed to a reconnect.
    paired_now: bool,
}

/// Version exchange, Noise handshake, the client's `hello`, and for a pairing the host's
/// confirmation. Every failure returns `None` and the caller drops the socket, so a client
/// that fails cannot tell why.
fn handshake(shared: &Arc<Shared>, ws: &mut Ws, mode: Mode) -> Option<Joined> {
    let limits = &shared.config.limits;
    let deadline = Instant::now() + limits.handshake_timeout;

    let request = String::from_utf8(wait(shared, ws, deadline, false)?).ok()?;
    let reply = match decode_handshake_request(&request) {
        Ok(()) => encode_handshake_reply(&HandshakeReply::Accept { supported: SUPPORTED_VERSIONS.to_vec() }).ok()?,
        Err(e) => {
            let body = ErrorMessage {
                code: e.code.as_str().to_string(),
                message: e.message,
                supported: Some(SUPPORTED_VERSIONS.to_vec()),
            };
            if let Ok(text) = encode_handshake_reply(&HandshakeReply::Error(body)) {
                send_raw(ws, Message::text(text));
            }
            return None;
        }
    };
    if !send_raw(ws, Message::text(reply.clone())) {
        return None;
    }
    let pro = prologue(mode, &request, &reply);
    let message1 = wait(shared, ws, deadline, true)?;

    let mut hs;
    let known_client;
    match mode {
        Mode::Pair => {
            let psk = shared.pairing_psk()?;
            hs = Handshake::responder_pair(&shared.host_key, &psk, &pro).ok()?;
            hs.read(&message1).ok()?;
            known_client = None;
        }
        Mode::Link => {
            let keys = shared.paired_keys();
            let (state, index, _) = Handshake::responder_link(&shared.host_key, &keys, &pro, &message1).ok()?;
            hs = state;
            known_client = Some(*keys.get(index)?);
        }
    }
    let remote = hs.remote_static()?;
    let message2 = hs.write(&[]).ok()?;
    if !send_raw(ws, Message::binary(message2)) {
        return None;
    }
    let mut transport = hs.into_transport().ok()?;

    // The first transport message must decrypt and be a hello: that proves the psk (pairing)
    // or the key (reconnect).
    let first = wait(shared, ws, deadline, true)?;
    let plain = frame::open(&mut transport, &first, MAX_MESSAGE_BYTES).ok()?;
    let hello = decode_client(std::str::from_utf8(&plain).ok()?);
    let name = match hello {
        Ok(ClientMessage::Hello { client_name, .. }) => client_name,
        _ => {
            if mode == Mode::Link {
                send(ws, &mut transport, &error_message(ErrorCode::BadRequest, "hello must come first"));
            }
            return None;
        }
    };

    match (mode, known_client) {
        (Mode::Link, Some(key)) => {
            let client = shared.client_by_key(&key)?;
            Some(Joined { transport, client, paired_now: false })
        }
        _ => {
            if shared.client_by_key(&remote).is_some() || !shared.consume_pairing() {
                return None;
            }
            let client = await_confirmation(shared, &remote, &name)?;
            Some(Joined { transport, client, paired_now: true })
        }
    }
}

/// Hold the pairing until the host confirms or rejects it, or the time runs out.
fn await_confirmation(shared: &Arc<Shared>, key: &PublicKey, name: &str) -> Option<Client> {
    let id = shared.next_pending_id();
    let (tx, rx) = mpsc::sync_channel(1);
    shared.add_pending(id.clone(), PendingEntry { key: *key, name: name.to_string(), tx });
    shared.log(LinkLog::PairingRequested { id: id.clone() });
    (shared.callbacks.on_pairing_request)(PendingPairing {
        id: id.clone(),
        client_name: name.to_string(),
        fingerprint: key.fingerprint(),
    });
    let deadline = Instant::now() + shared.config.limits.confirm_timeout;
    let outcome = loop {
        if shared.stopped() || Instant::now() >= deadline {
            break None;
        }
        match rx.recv_timeout(POLL) {
            Ok(Decision::Confirmed(client)) => break Some(client),
            Ok(Decision::Rejected) | Err(RecvTimeoutError::Disconnected) => break None,
            Err(RecvTimeoutError::Timeout) => {}
        }
    };
    shared.remove_pending(&id);
    outcome
}

// ---- the message loop ----

struct Loop<'a> {
    shared: &'a Arc<Shared>,
    client_id: &'a str,
    out: &'a Sender<HostMessage>,
    inflight: &'a Arc<AtomicUsize>,
}

fn run(shared: &Arc<Shared>, mut ws: Ws, joined: Joined) {
    let Joined { mut transport, client, paired_now } = joined;
    let limits = &shared.config.limits;
    let kick = shared.register_live(&client.id);
    let (out_tx, out_rx): (Sender<HostMessage>, Receiver<HostMessage>) = mpsc::channel();
    let inflight = Arc::new(AtomicUsize::new(0));

    let (feed, view) = if client.has(Scope::Read) {
        let (view, sub) = shared.sessions.subscribe(limits.event_queue);
        (Some(sub), Some(view))
    } else {
        (None, None)
    };
    let welcome = HostMessage::Welcome(Welcome {
        host_id: shared.host_pub.host_id(),
        host_name: shared.config.host_name.clone(),
        scopes: client.scopes.iter().map(|s| s.as_str().to_string()).collect(),
        features: Vec::new(),
    });
    if !send(&mut ws, &mut transport, &welcome) {
        shared.unregister_live(&client.id, &kick);
        return;
    }
    if paired_now {
        shared.log(LinkLog::Paired { client_id: client.id.clone() });
    }
    if let Some(v) = view {
        let snapshot = HostMessage::Snapshot(Box::new(Snapshot {
            seq: v.seq,
            projects: v.projects,
            sessions: v.sessions,
            inbox: v.inbox,
            tasks: v.tasks,
            storage: WireStorageStatus::from(&shared.config.storage),
        }));
        if !send(&mut ws, &mut transport, &snapshot) {
            shared.unregister_live(&client.id, &kick);
            return;
        }
    }

    let ctx = Loop { shared, client_id: &client.id, out: &out_tx, inflight: &inflight };
    let mut last_rx = Instant::now();
    'conn: loop {
        if shared.stopped() || kick.load(Ordering::SeqCst) {
            break;
        }
        // Scopes and revocation are read from the store on every pass, so a change acts on
        // a live connection.
        let Some(current) = shared.client_by_id(&client.id) else {
            shared.log(LinkLog::Revoked { client_id: client.id.clone() });
            send(&mut ws, &mut transport, &error_message(ErrorCode::Revoked, "this device was removed"));
            break;
        };
        match poll(&mut ws) {
            Polled::Binary(bytes) => {
                last_rx = Instant::now();
                // Read the client again: a scope may have changed while the read waited.
                let Some(fresh) = shared.client_by_id(&client.id) else {
                    shared.log(LinkLog::Revoked { client_id: client.id.clone() });
                    send(&mut ws, &mut transport, &error_message(ErrorCode::Revoked, "this device was removed"));
                    break;
                };
                if !handle_binary(&ctx, &fresh, &mut ws, &mut transport, &bytes) {
                    break;
                }
            }
            Polled::Nothing => {}
            Polled::Text(_) | Polled::Closed => break,
        }
        while let Ok(msg) = out_rx.try_recv() {
            if !send(&mut ws, &mut transport, &msg) {
                break 'conn;
            }
        }
        if let Some(sub) = &feed {
            for _ in 0..EVENTS_PER_PASS {
                match sub.try_recv() {
                    Ok(item) => {
                        if !current.has(Scope::Read) {
                            continue;
                        }
                        let msg = HostMessage::Event { seq: item.0, event: HostEvent::from(item.1.clone()) };
                        if !send(&mut ws, &mut transport, &msg) {
                            break 'conn;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    // Fell too far behind: the client reconnects for a fresh snapshot.
                    Err(TryRecvError::Disconnected) => {
                        shared.log(LinkLog::Lagging { client_id: client.id.clone() });
                        break 'conn;
                    }
                }
            }
        }
        if last_rx.elapsed() >= limits.idle_timeout {
            break;
        }
    }
    shared.unregister_live(&client.id, &kick);
    // Dropping the feed unsubscribes it; dropping the socket closes it.
}

/// One decrypted message. False closes the connection.
fn handle_binary(ctx: &Loop<'_>, current: &Client, ws: &mut Ws, tr: &mut Transport, bytes: &[u8]) -> bool {
    let Ok(plain) = frame::open(tr, bytes, MAX_MESSAGE_BYTES) else {
        return false;
    };
    let Ok(text) = String::from_utf8(plain) else {
        return false;
    };
    match decode_client(&text) {
        Ok(ClientMessage::Ping) => send(ws, tr, &HostMessage::Pong),
        Ok(ClientMessage::Hello { .. }) => send(ws, tr, &error_message(ErrorCode::BadRequest, "already said hello")),
        Ok(ClientMessage::Call { id, call }) => handle_call(ctx, current, ws, tr, id, call),
        Err(e) => {
            let fatal = e.code == ErrorCode::UnsupportedVersion;
            let supported = fatal.then(|| SUPPORTED_VERSIONS.to_vec());
            let msg = HostMessage::Error(ErrorMessage { code: e.code.as_str().to_string(), message: e.message, supported });
            send(ws, tr, &msg) && !fatal
        }
    }
}

fn result(id: String, outcome: Result<Value, ErrorBody>) -> HostMessage {
    HostMessage::Result { id, outcome }
}

fn refuse(id: String, code: ErrorCode, message: &str) -> HostMessage {
    result(id, Err(ErrorBody { code: code.as_str().to_string(), message: message.to_string() }))
}

/// Scope check, idempotency, then dispatch on a worker thread. Only the method and the
/// client are ever logged.
fn handle_call(
    ctx: &Loop<'_>,
    current: &Client,
    ws: &mut Ws,
    tr: &mut Transport,
    id: String,
    call: wire::Call,
) -> bool {
    let shared = ctx.shared;
    let method = call.method();
    if !current.has(method.scope()) {
        shared.log(LinkLog::Forbidden { client_id: current.id.clone(), method: method.as_str() });
        return send(ws, tr, &refuse(id, ErrorCode::Forbidden, "this device may not do that"));
    }
    let cacheable = method.scope() != Scope::Read;
    if ctx.inflight.load(Ordering::SeqCst) >= shared.config.limits.max_calls_per_connection {
        return send(ws, tr, &refuse(id, ErrorCode::Failed, "busy"));
    }
    if cacheable {
        // The cache lock is released before anything is written.
        let begun = shared.id_cache(ctx.client_id).begin(&id);
        match begun {
            Begin::Run => {}
            Begin::Replay(r) => return send(ws, tr, &result(id, r)),
            Begin::InProgress => {
                return send(ws, tr, &refuse(id, ErrorCode::Failed, "that call is still running; resend later"));
            }
        }
    }
    ctx.inflight.fetch_add(1, Ordering::SeqCst);
    let (sessions, storage) = (Arc::clone(&shared.sessions), shared.config.storage.clone());
    let (out, inflight, worker_shared) = (ctx.out.clone(), Arc::clone(ctx.inflight), Arc::clone(shared));
    let (worker_id, client_id) = (id.clone(), ctx.client_id.to_string());
    let spawned = std::thread::Builder::new().name("link-call".into()).spawn(move || {
        let ran = catch_unwind(AssertUnwindSafe(|| wire::dispatch(&sessions, &storage, &call)));
        let outcome = match ran {
            Ok(r) => r.map_err(error_body),
            Err(_) => Err(ErrorBody { code: ErrorCode::Failed.as_str().to_string(), message: "internal error".into() }),
        };
        if cacheable {
            worker_shared.id_cache(&client_id).finish(&worker_id, outcome.clone());
        }
        inflight.fetch_sub(1, Ordering::SeqCst);
        let _ = out.send(result(worker_id, outcome));
    });
    if spawned.is_err() {
        ctx.inflight.fetch_sub(1, Ordering::SeqCst);
        if cacheable {
            shared.id_cache(ctx.client_id).forget(&id);
        }
        return send(ws, tr, &refuse(id, ErrorCode::Failed, "busy"));
    }
    true
}

pub(crate) type IdCaches = Mutex<HashMap<String, IdCache>>;
pub(crate) type LiveFlags = Mutex<HashMap<String, Arc<AtomicBool>>>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ok(v: Value) -> Result<Value, ErrorBody> {
        Ok(v)
    }

    #[test]
    fn id_cache_replays_a_finished_call_and_holds_a_running_one() {
        let mut c = IdCache::default();
        assert!(matches!(c.begin("a"), Begin::Run));
        assert!(matches!(c.begin("a"), Begin::InProgress));
        c.finish("a", ok(json!(null)));
        assert!(matches!(c.begin("a"), Begin::Replay(Ok(Value::Null))));
        c.forget("a");
        assert!(matches!(c.begin("a"), Begin::Run));
    }

    #[test]
    fn id_cache_keeps_the_last_256_ids() {
        let mut c = IdCache::default();
        for i in 0..ID_CACHE {
            assert!(matches!(c.begin(&format!("i{i}")), Begin::Run));
            c.finish(&format!("i{i}"), ok(json!(i)));
        }
        assert!(matches!(c.begin("i0"), Begin::Replay(_)));
        assert!(matches!(c.begin("new"), Begin::Run));
        // The oldest was evicted to make room; the rest are still held.
        assert!(matches!(c.begin("i0"), Begin::Run));
        assert!(matches!(c.begin("i200"), Begin::Replay(_)));
    }

    #[test]
    fn a_timed_out_read_reads_as_would_block() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();
        server.set_read_timeout(Some(Duration::from_millis(10))).unwrap();
        let mut sock = Sock(server);
        let err = sock.read(&mut [0u8; 4]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        drop(client);
    }
}

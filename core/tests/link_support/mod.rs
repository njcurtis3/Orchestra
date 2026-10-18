//! A Rust test client and a host rig for `tests/link.rs`. The client speaks the real
//! protocol over a real socket: a WebSocket upgrade, the plaintext version frames, a Noise
//! handshake (through the core's own `link::noise`, which wraps snow) and framed messages.
//! Every socket here is bound to or connected to `127.0.0.1`, with the port chosen by the OS.

#![allow(dead_code)]

use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use orchestra_core::link::clients::Client;
use orchestra_core::link::keys::{FileKeyProvider, Psk, PublicKey, SecretKey, from_hex};
use orchestra_core::link::noise::{Handshake, Mode, Transport, prologue};
use orchestra_core::link::pairing::PairingOffer;
use orchestra_core::link::{
    LinkCallbacks, LinkConfig, LinkHost, LinkLimits, LinkLog, PendingPairing, frame,
};
use orchestra_core::session::process::{Output, Proc};
use orchestra_core::session::settings::SessionSettings;
use orchestra_core::session::store::StorageStatus;
use orchestra_core::session::transcript::TranscriptStore;
use orchestra_core::session::{Sessions, Sink};
use orchestra_core::wire::message::HandshakeReply;
use orchestra_core::wire::{
    self, Call, ClientMessage, HostMessage, MAX_MESSAGE_BYTES, Method, Scope, decode_handshake_reply,
    decode_host, encode_client, encode_handshake_request,
};
use serde_json::{Value, json};
use tungstenite::client::IntoClientRequest;
use tungstenite::{Error as WsError, Message, WebSocket};

pub const INIT: &str = r#"{"type":"system","subtype":"init","session_id":"c-1"}"#;
pub const PERM: &str = r#"{"type":"control_request","request_id":"r-1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"touch x"}}}"#;

/// How long a test waits for the host before it calls a message missing.
const WAIT: Duration = Duration::from_secs(5);

static RIGS: AtomicUsize = AtomicUsize::new(0);

pub struct Fake {
    pub lines: Mutex<Vec<String>>,
}

impl Proc for Fake {
    fn send_line(&self, line: &str) -> Result<(), String> {
        self.lines.lock().unwrap().push(line.to_string());
        Ok(())
    }
    fn kill(&self) {}
    fn wait_exit(&self, _timeout: Duration) -> bool {
        true
    }
}

pub struct Rig {
    pub sessions: Arc<Sessions>,
    pub host: LinkHost,
    pub addr: SocketAddr,
    pub host_key: PublicKey,
    pub project: String,
    pub fake: Arc<Fake>,
    pub pairing: Receiver<PendingPairing>,
    pub logs: Arc<Mutex<Vec<LinkLog>>>,
    sinks: Arc<Mutex<Vec<Sink>>>,
    dir: PathBuf,
}

impl Rig {
    pub fn new() -> Rig {
        Rig::with(|_| {})
    }

    /// A host listening on `127.0.0.1:0`, with the limits adjusted by `tweak`.
    pub fn with(tweak: impl FnOnce(&mut LinkLimits)) -> Rig {
        let dir = std::env::temp_dir().join(format!(
            "orchestra-linktest-{}-{}",
            std::process::id(),
            RIGS.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fake = Arc::new(Fake { lines: Mutex::new(vec![]) });
        let sinks: Arc<Mutex<Vec<Sink>>> = Arc::new(Mutex::new(vec![]));
        let (f2, s2) = (fake.clone(), sinks.clone());
        let sessions = Sessions::with_writer(
            Box::new(|_| {}),
            Box::new(move |_, _, sink| {
                s2.lock().unwrap().push(sink);
                Ok(f2.clone() as Arc<dyn Proc>)
            }),
            Box::new(|_, _| {}),
            None,
            TranscriptStore::memory(),
            dir.join("worktrees"),
        );
        let project = sessions.add_project(&std::env::temp_dir()).unwrap().id;

        let mut config = LinkConfig::new("Test host", dir.join("link"), StorageStatus::ok());
        config.binds = vec!["127.0.0.1:0".parse().unwrap()];
        tweak(&mut config.limits);
        let (tx, pairing) = channel();
        let tx = Mutex::new(tx);
        let logs: Arc<Mutex<Vec<LinkLog>>> = Arc::new(Mutex::new(vec![]));
        let l2 = logs.clone();
        let callbacks = LinkCallbacks {
            on_pairing_request: Box::new(move |p| {
                let _ = tx.lock().unwrap().send(p);
            }),
            log: Box::new(move |entry| l2.lock().unwrap().push(entry)),
        };
        let keys = Box::new(FileKeyProvider::new(&config.link_dir));
        let host = LinkHost::new(sessions.clone(), config.clone(), keys, callbacks);
        let addr = host.start().unwrap()[0];
        let host_key = FileKeyProvider::new(&config.link_dir);
        let host_key = orchestra_core::link::keys::KeyProvider::load_or_create(&host_key).unwrap().public();
        Rig { sessions, host, addr, host_key, project, fake, pairing, logs, sinks, dir }
    }

    /// Start a session on the fake process and return its id.
    pub fn start_session(&self, prompt: &str) -> String {
        self.sessions.start(&self.project, prompt, SessionSettings::default()).unwrap().id
    }

    /// A line of the newest session's stdout.
    pub fn feed(&self, line: &str) {
        let guard = self.sinks.lock().unwrap();
        (guard.last().unwrap())(Output::Line(line.to_string()));
    }

    pub fn logged(&self) -> Vec<LinkLog> {
        self.logs.lock().unwrap().clone()
    }

    /// Open a pairing window, connect `me`, send the hello, let the host confirm with
    /// `scopes`, and return the connection with its welcome read.
    pub fn pair(&self, me: &Identity, name: &str, scopes: Option<&[Scope]>) -> (Client, Conn) {
        let offer = self.host.open_pairing().unwrap();
        let mut conn = Conn::pair(self.addr, &offer, me, name).expect("pairing handshake");
        let request = self.pairing.recv_timeout(WAIT).expect("a pairing request");
        assert_eq!(request.client_name, name);
        assert_eq!(request.fingerprint, me.public.fingerprint());
        let client = self.host.confirm_pairing(&request.id, scopes).unwrap();
        assert!(matches!(conn.expect(), HostMessage::Welcome(_)));
        (client, conn)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.host.stop();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub struct Identity {
    pub secret: SecretKey,
    pub public: PublicKey,
}

impl Identity {
    pub fn new() -> Identity {
        let secret = SecretKey::generate().unwrap();
        let public = secret.public();
        Identity { secret, public }
    }
}

pub type Ws = WebSocket<TcpStream>;

pub enum Got {
    Msg(HostMessage),
    Closed,
    Timeout,
}

/// Connect and upgrade. `Err` when the host answered the upgrade with an error or dropped.
pub fn upgrade(addr: SocketAddr, path: &str) -> Result<Ws, String> {
    let stream = TcpStream::connect(addr).map_err(|e| e.to_string())?;
    stream.set_read_timeout(Some(WAIT)).unwrap();
    stream.set_write_timeout(Some(WAIT)).unwrap();
    let request = format!("ws://{addr}{path}").into_client_request().map_err(|e| e.to_string())?;
    let (ws, _) = tungstenite::client(request, stream).map_err(|e| e.to_string())?;
    Ok(ws)
}

pub fn read_text(ws: &mut Ws) -> Result<String, String> {
    loop {
        match ws.read() {
            Ok(Message::Text(t)) => return Ok(t.as_str().to_string()),
            Ok(Message::Ping(_) | Message::Pong(_)) => {}
            Ok(other) => return Err(format!("unexpected {other:?}")),
            Err(e) => return Err(e.to_string()),
        }
    }
}

pub fn read_binary(ws: &mut Ws) -> Result<Vec<u8>, String> {
    loop {
        match ws.read() {
            Ok(Message::Binary(b)) => return Ok(b.to_vec()),
            Ok(Message::Ping(_) | Message::Pong(_)) => {}
            Ok(other) => return Err(format!("unexpected {other:?}")),
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// An established, encrypted connection.
pub struct Conn {
    pub ws: Ws,
    pub tr: Transport,
}

impl Conn {
    /// The version exchange as an accepted client does it: returns both plaintext frames.
    pub fn versions(ws: &mut Ws) -> Result<(String, String), String> {
        let request = encode_handshake_request();
        ws.send(Message::text(request.clone())).map_err(|e| e.to_string())?;
        let reply = read_text(ws)?;
        match decode_handshake_reply(&reply) {
            Ok(HandshakeReply::Accept { .. }) => Ok((request, reply)),
            other => Err(format!("version refused: {other:?}")),
        }
    }

    /// Pair: upgrade `/pair`, IKpsk2, and send the hello. The host's confirmation is a
    /// separate step; the welcome follows it.
    pub fn pair(addr: SocketAddr, offer: &PairingOffer, me: &Identity, name: &str) -> Result<Conn, String> {
        let host = PublicKey::from_hex(&offer.host_key).ok_or("bad host key")?;
        let psk = Psk::from_bytes(from_hex::<32>(&offer.secret).ok_or("bad secret")?);
        Conn::pair_with(addr, &host, &psk, me, name)
    }

    pub fn pair_with(addr: SocketAddr, host: &PublicKey, psk: &Psk, me: &Identity, name: &str) -> Result<Conn, String> {
        let mut ws = upgrade(addr, Mode::Pair.path())?;
        let (request, reply) = Conn::versions(&mut ws)?;
        let mut hs = Handshake::initiator_pair(&me.secret, host, psk, &prologue(Mode::Pair, &request, &reply))
            .map_err(|e| e.to_string())?;
        let m1 = hs.write(&[]).map_err(|e| e.to_string())?;
        ws.send(Message::binary(m1)).map_err(|e| e.to_string())?;
        let m2 = read_binary(&mut ws)?;
        hs.read(&m2).map_err(|e| e.to_string())?;
        let mut conn = Conn { ws, tr: hs.into_transport().map_err(|e| e.to_string())? };
        conn.send(&hello(name));
        Ok(conn)
    }

    /// Reconnect: upgrade `/link`, KK, no hello yet.
    pub fn link_open(addr: SocketAddr, host: &PublicKey, me: &Identity) -> Result<Conn, String> {
        let mut ws = upgrade(addr, Mode::Link.path())?;
        let (request, reply) = Conn::versions(&mut ws)?;
        let mut hs = Handshake::initiator_link(&me.secret, host, &prologue(Mode::Link, &request, &reply))
            .map_err(|e| e.to_string())?;
        let m1 = hs.write(&[]).map_err(|e| e.to_string())?;
        ws.send(Message::binary(m1)).map_err(|e| e.to_string())?;
        let m2 = read_binary(&mut ws)?;
        hs.read(&m2).map_err(|e| e.to_string())?;
        Ok(Conn { ws, tr: hs.into_transport().map_err(|e| e.to_string())? })
    }

    /// `link_open` and the hello; the welcome is next.
    pub fn link(addr: SocketAddr, host: &PublicKey, me: &Identity, name: &str) -> Result<Conn, String> {
        let mut conn = Conn::link_open(addr, host, me)?;
        conn.send(&hello(name));
        Ok(conn)
    }

    pub fn send(&mut self, msg: &ClientMessage) {
        self.send_text(&encode_client(msg).unwrap());
    }

    pub fn send_text(&mut self, text: &str) {
        self.send_plain(text.as_bytes());
    }

    /// Chunk, encrypt and send arbitrary plaintext (not necessarily valid JSON).
    pub fn send_plain(&mut self, plain: &[u8]) {
        let sealed = frame::seal(&mut self.tr, plain).unwrap();
        self.ws.send(Message::binary(sealed)).unwrap();
    }

    pub fn call(&mut self, id: &str, call: Call) {
        self.send(&ClientMessage::Call { id: id.to_string(), call });
    }

    pub fn recv(&mut self) -> Got {
        loop {
            match self.ws.read() {
                Ok(Message::Binary(b)) => {
                    let plain = frame::open(&mut self.tr, &b, MAX_MESSAGE_BYTES).expect("a frame that opens");
                    let text = String::from_utf8(plain).expect("utf-8");
                    return Got::Msg(decode_host(&text).unwrap_or_else(|e| panic!("host message: {e}")));
                }
                Ok(Message::Close(_)) => return Got::Closed,
                Ok(Message::Ping(_) | Message::Pong(_)) => {}
                Ok(other) => panic!("unexpected {other:?}"),
                Err(WsError::Io(e))
                    if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
                {
                    return Got::Timeout;
                }
                Err(_) => return Got::Closed,
            }
        }
    }

    /// The next message, or a panic if the host closed or stayed silent.
    pub fn expect(&mut self) -> HostMessage {
        match self.recv() {
            Got::Msg(m) => m,
            Got::Closed => panic!("the host closed the connection"),
            Got::Timeout => panic!("the host sent nothing"),
        }
    }

    /// The host closes without sending anything first.
    pub fn expect_closed(&mut self) {
        match self.recv() {
            Got::Closed => {}
            Got::Msg(m) => panic!("expected a silent close, got {m:?}"),
            Got::Timeout => panic!("the connection stayed open"),
        }
    }

    /// Read until the host closes, returning what it sent first.
    pub fn drain_until_closed(&mut self) -> Vec<HostMessage> {
        let mut out = vec![];
        loop {
            match self.recv() {
                Got::Msg(m) => out.push(m),
                Got::Closed => return out,
                Got::Timeout => panic!("the connection stayed open"),
            }
        }
    }

    /// Read until a result with this id arrives (events may come first).
    pub fn result(&mut self, id: &str) -> Result<Value, wire::message::ErrorBody> {
        loop {
            if let HostMessage::Result { id: got, outcome } = self.expect() {
                if got == id {
                    return outcome;
                }
            }
        }
    }
}

pub fn hello(name: &str) -> ClientMessage {
    ClientMessage::Hello { client_name: name.to_string(), app_version: "test".to_string() }
}

/// Read the snapshot that follows a welcome, skipping nothing: it must come next.
pub fn expect_snapshot(conn: &mut Conn) -> wire::Snapshot {
    match conn.expect() {
        HostMessage::Snapshot(s) => *s,
        other => panic!("expected a snapshot, got {other:?}"),
    }
}

/// `args` for a method as the wire carries it.
pub fn call_of(method: Method, args: Value) -> Call {
    let text = json!({"v": 1, "type": "call", "id": "x", "method": method.as_str(), "args": args}).to_string();
    match wire::decode_client(&text).unwrap() {
        ClientMessage::Call { call, .. } => call,
        _ => unreachable!(),
    }
}

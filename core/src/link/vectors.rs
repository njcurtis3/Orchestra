//! Deterministic Noise transcripts for both handshakes, written to
//! `protocol/fixtures/noise/*.json`. They are the shared contract between this host and the
//! phone's JS Noise: the phone must reproduce every byte from the same fixed keys.
//!
//! The default test rebuilds them in memory and compares with the committed files, so a
//! change in snow, serde_json or the framing fails here. `ORCHESTRA_WRITE_VECTORS=1` rewrites
//! the files instead. Every key is the SHA-256 of a label; none of them is a secret.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::frame;
use super::keys::{Psk, PublicKey, SecretKey, sha256, to_hex};
use super::noise::{Handshake, Mode, Transport, prologue};
use super::pairing::{OFFER_VERSION, PairingOffer};
use crate::wire::codec::{encode_client, encode_handshake_reply, encode_handshake_request, encode_host};
use crate::wire::message::{ClientMessage, HandshakeReply, HostMessage, Welcome};

const EXPIRES_AT: u64 = 1_800_000_000_000;
const ADDRS: [&str; 2] = ["127.0.0.1:47800", "192.168.1.20:47800"];
const BIG: usize = 65520;
const CAP: usize = 4 * 1024 * 1024;

fn key(label: &str) -> [u8; 32] {
    sha256(label.as_bytes())
}

fn secret(label: &str) -> SecretKey {
    SecretKey::from_bytes(key(label))
}

fn hex(b: &[u8]) -> String {
    to_hex(b)
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Byte `i` is `0x61 + i mod 26`: a-z repeating.
fn big_plaintext() -> Vec<u8> {
    (0..BIG).map(|i| 0x61 + (i % 26) as u8).collect()
}

fn request_frame() -> String {
    encode_handshake_request()
}

fn reply_frame() -> String {
    encode_handshake_reply(&HandshakeReply::Accept { supported: vec![1] }).unwrap()
}

fn hello_text() -> String {
    encode_client(&ClientMessage::Hello { client_name: "Vector phone".into(), app_version: "1.0.0".into() }).unwrap()
}

fn ping_text() -> String {
    encode_client(&ClientMessage::Ping).unwrap()
}

fn welcome_text(host: &PublicKey) -> String {
    encode_host(&HostMessage::Welcome(Welcome {
        host_id: host.host_id(),
        host_name: "Vector desktop".into(),
        scopes: vec!["read".into(), "answer".into()],
        features: vec![],
    }))
    .unwrap()
}

fn offer(host: &PublicKey, psk: &Psk) -> PairingOffer {
    PairingOffer {
        v: OFFER_VERSION,
        host_id: host.host_id(),
        host_name: "Vector desktop".into(),
        addrs: ADDRS.iter().map(|a| a.to_string()).collect(),
        host_key: host.to_hex(),
        secret: hex(psk.expose()),
        expires_at: EXPIRES_AT,
    }
}

/// Everything that differs between the two suites.
struct Setup {
    mode: Mode,
    client: SecretKey,
    host: SecretKey,
    client_eph: [u8; 32],
    host_eph: [u8; 32],
    psk: Option<Psk>,
}

impl Setup {
    fn prologue(&self) -> Vec<u8> {
        prologue(self.mode, &request_frame(), &reply_frame())
    }

    fn initiator(&self) -> Handshake {
        let host = self.host.public();
        Handshake::with_ephemeral(
            self.mode,
            &self.client,
            Some(&host),
            self.psk.as_ref(),
            &self.prologue(),
            true,
            &self.client_eph,
        )
        .unwrap()
    }

    /// The responder with the same fixed ephemeral as the vectors. IKpsk2 learns the client
    /// key from message 1; KK is given it.
    fn responder(&self) -> Handshake {
        let remote = (self.mode == Mode::Link).then(|| self.client.public());
        Handshake::with_ephemeral(
            self.mode,
            &self.host,
            remote.as_ref(),
            self.psk.as_ref(),
            &self.prologue(),
            false,
            &self.host_eph,
        )
        .unwrap()
    }
}

struct Run {
    msg1: Vec<u8>,
    msg2: Vec<u8>,
    hash: Vec<u8>,
    client: Transport,
    host: Transport,
}

fn handshake(s: &Setup) -> Run {
    let (mut init, mut resp) = (s.initiator(), s.responder());
    let msg1 = init.write(&[]).unwrap();
    resp.read(&msg1).unwrap();
    let msg2 = resp.write(&[]).unwrap();
    init.read(&msg2).unwrap();
    assert!(init.is_finished() && resp.is_finished());
    let hash = init.handshake_hash();
    assert_eq!(hash, resp.handshake_hash());
    Run { msg1, msg2, hash, client: init.into_transport().unwrap(), host: resp.into_transport().unwrap() }
}

fn transcript(label: &str, s: &Setup) -> Value {
    let Run { msg1, msg2, hash, mut client, mut host } = handshake(s);
    let host_pub = s.host.public();
    let big = big_plaintext();

    let (hello, ping, welcome) = (hello_text(), ping_text(), welcome_text(&host_pub));
    let c2h_hello = frame::seal(&mut client, hello.as_bytes()).unwrap();
    let h2c_welcome = frame::seal(&mut host, welcome.as_bytes()).unwrap();
    let c2h_ping = frame::seal(&mut client, ping.as_bytes()).unwrap();
    let h2c_big = frame::seal(&mut host, &big).unwrap();
    let c2h_big = frame::seal(&mut client, &big).unwrap();

    let text = |wire: &[u8], plain: &str| json!({"plaintext": plain, "wire_hex": hex(wire)});
    let mut v = json!({
        "about": format!(
            "Noise transcript for {label}, generated by core/src/link/vectors.rs from fixed keys. \
             Regenerate with ORCHESTRA_WRITE_VECTORS=1 cargo test -p orchestra-core --lib link::vectors"
        ),
        "suite": s.mode.suite(),
        "path": s.mode.path(),
        "request_frame": request_frame(),
        "reply_frame": reply_frame(),
        "prologue_hex": hex(&s.prologue()),
        "client_static_secret_hex": hex(s.client.expose()),
        "client_static_public_hex": s.client.public().to_hex(),
        "client_ephemeral_secret_hex": hex(&s.client_eph),
        "host_static_secret_hex": hex(s.host.expose()),
        "host_static_public_hex": host_pub.to_hex(),
        "host_ephemeral_secret_hex": hex(&s.host_eph),
        "msg1_hex": hex(&msg1),
        "msg2_hex": hex(&msg2),
        "handshake_hash_hex": hex(&hash),
        "transport": {
            "max_message_bytes": CAP,
            "c2h_hello": text(&c2h_hello, &hello),
            "h2c_welcome": text(&h2c_welcome, &welcome),
            "c2h_ping": text(&c2h_ping, &ping),
            "big_plaintext": {
                "rule": "byte i is 0x61 + (i mod 26)",
                "length": BIG,
                "sha256_hex": hex(&sha256(&big)),
            },
            "h2c_big": {
                "wire_length": h2c_big.len(),
                "wire_base64": base64(&h2c_big),
                "wire_sha256_hex": hex(&sha256(&h2c_big)),
            },
            "c2h_big": {
                "wire_length": c2h_big.len(),
                "wire_sha256_hex": hex(&sha256(&c2h_big)),
            },
        },
    });
    if let Some(psk) = &s.psk {
        v["psk_hex"] = json!(hex(psk.expose()));
    }
    v
}

fn setup_pair() -> Setup {
    Setup {
        mode: Mode::Pair,
        client: secret("orchestra vector: client static"),
        host: secret("orchestra vector: host static"),
        client_eph: key("orchestra vector: client ephemeral"),
        host_eph: key("orchestra vector: host ephemeral"),
        psk: Some(Psk::from_bytes(key("orchestra vector: psk"))),
    }
}

fn setup_link() -> Setup {
    Setup { mode: Mode::Link, psk: None, ..setup_pair() }
}

fn build_ikpsk2() -> Value {
    transcript("Noise_IKpsk2_25519_ChaChaPoly_SHA256 (pairing, /pair)", &setup_pair())
}

fn build_kk() -> Value {
    transcript("Noise_KK_25519_ChaChaPoly_SHA256 (reconnect, /link)", &setup_link())
}

fn build_offer() -> Value {
    let s = setup_pair();
    let o = offer(&s.host.public(), s.psk.as_ref().unwrap());
    json!({
        "about": "The pairing QR payload for the keys in ikpsk2.json: json is the exact text a QR carries.",
        "json": o.to_json(),
        "fingerprint_of_client_key": s.client.public().fingerprint(),
    })
}

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../protocol/fixtures/noise")
}

fn rendered(v: &Value) -> String {
    let mut s = serde_json::to_string_pretty(v).unwrap();
    s.push('\n');
    s
}

#[test]
fn vectors_match_committed_files() {
    let write = std::env::var("ORCHESTRA_WRITE_VECTORS").is_ok_and(|v| v == "1");
    let files = [("ikpsk2.json", build_ikpsk2()), ("kk.json", build_kk()), ("offer.json", build_offer())];
    for (name, value) in files {
        let want = rendered(&value);
        assert!(want.len() <= 262_144, "{name} is over 256 KiB");
        let path = dir().join(name);
        if write {
            std::fs::create_dir_all(dir()).unwrap();
            std::fs::write(&path, &want).unwrap();
        } else {
            let have = std::fs::read_to_string(&path).unwrap_or_default().replace("\r\n", "\n");
            assert!(have == want, "{name} is out of date: rerun with ORCHESTRA_WRITE_VECTORS=1");
        }
    }
}

fn committed(name: &str) -> Value {
    let text = std::fs::read_to_string(dir().join(name)).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn unhex(v: &Value) -> Vec<u8> {
    let s = v.as_str().unwrap();
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

/// Feed the committed bytes to the production host read path: the responder constructors,
/// then `frame::open` on the client frames.
#[test]
fn vectors_open_on_the_production_host_path() {
    for (name, s) in [("ikpsk2.json", setup_pair()), ("kk.json", setup_link())] {
        let doc = committed(name);
        let pro = unhex(&doc["prologue_hex"]);
        let msg1 = unhex(&doc["msg1_hex"]);
        assert_eq!(pro, s.prologue());

        // The production responder reads the committed message 1 and finds the client key.
        let seen = match s.mode {
            Mode::Pair => {
                let mut r = Handshake::responder_pair(&s.host, s.psk.as_ref().unwrap(), &pro).unwrap();
                r.read(&msg1).unwrap();
                r.remote_static()
            }
            Mode::Link => {
                let (r, found, _) = Handshake::responder_link(&s.host, &[s.client.public()], &pro, &msg1).unwrap();
                assert_eq!(found, 0);
                r.remote_static()
            }
        };
        assert_eq!(seen, Some(s.client.public()), "{name}");

        // The same responder with the vector's ephemeral reproduces message 2, the hash and
        // the transport keys, so the committed client frames open.
        let mut r = s.responder();
        r.read(&msg1).unwrap();
        assert_eq!(r.remote_static(), Some(s.client.public()));
        assert_eq!(r.write(&[]).unwrap(), unhex(&doc["msg2_hex"]), "{name}");
        assert_eq!(hex(&r.handshake_hash()), doc["handshake_hash_hex"].as_str().unwrap(), "{name}");
        let mut host = r.into_transport().unwrap();
        let t = &doc["transport"];
        let hello = frame::open(&mut host, &unhex(&t["c2h_hello"]["wire_hex"]), CAP).unwrap();
        assert_eq!(hello, t["c2h_hello"]["plaintext"].as_str().unwrap().as_bytes());
        let ping = frame::open(&mut host, &unhex(&t["c2h_ping"]["wire_hex"]), CAP).unwrap();
        assert_eq!(ping, t["c2h_ping"]["plaintext"].as_str().unwrap().as_bytes());
    }
}

//! The two Noise handshakes of the link and the transport they end in.
//!
//! - Pairing, `/pair`: `Noise_IKpsk2_25519_ChaChaPoly_SHA256`. The client knows the host's
//!   static key and the one-time psk from the QR; the host learns the client's static key.
//! - Reconnecting, `/link`: `Noise_KK_25519_ChaChaPoly_SHA256`. Both static keys are known.
//!   A host does not know which paired client is calling before it reads message 1, so it
//!   tries each stored key (at most `clients::MAX_CLIENTS`) and keeps the one whose tag
//!   verifies.
//!
//! The prologue binds the WebSocket path and both plaintext frames, so none of the three can
//! be swapped or rewritten without failing the handshake. Errors say nothing about why.

use std::fmt;

use snow::{Builder, HandshakeState, TransportState};

use super::keys::{Psk, PublicKey, SecretKey};

pub const SUITE_PAIR: &str = "Noise_IKpsk2_25519_ChaChaPoly_SHA256";
pub const SUITE_LINK: &str = "Noise_KK_25519_ChaChaPoly_SHA256";
/// A Noise message, handshake or transport, is at most this long.
pub const MAX_NOISE_MESSAGE: usize = 65535;
/// The authentication tag each transport message adds.
pub const TAG_BYTES: usize = 16;
const PROLOGUE_TAG: &[u8] = b"orchestra-link v1";

/// The two handshakes, named by the WebSocket path that selects them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Pair,
    Link,
}

impl Mode {
    pub fn path(self) -> &'static str {
        match self {
            Mode::Pair => "/pair",
            Mode::Link => "/link",
        }
    }

    pub fn from_path(path: &str) -> Option<Self> {
        [Mode::Pair, Mode::Link].into_iter().find(|m| m.path() == path)
    }

    pub fn suite(self) -> &'static str {
        match self {
            Mode::Pair => SUITE_PAIR,
            Mode::Link => SUITE_LINK,
        }
    }
}

/// A failure with no detail: what went wrong in a handshake is not for the peer, or a log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoiseError {
    Init,
    Handshake,
    /// No stored key verified message 1.
    NoMatch,
    Transport,
}

impl fmt::Display for NoiseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            NoiseError::Init => "could not set up the handshake",
            NoiseError::Handshake => "the handshake failed",
            NoiseError::NoMatch => "no known key matched",
            NoiseError::Transport => "a message did not decrypt",
        })
    }
}

impl std::error::Error for NoiseError {}

/// The prologue: a version tag, then the path, the client's `{proto,v}` text frame and the
/// host's reply text frame, each as a 4 byte big-endian length and its bytes.
pub fn prologue(mode: Mode, request_frame: &str, reply_frame: &str) -> Vec<u8> {
    let mut out = PROLOGUE_TAG.to_vec();
    for part in [mode.path(), request_frame, reply_frame] {
        out.extend_from_slice(&(part.len() as u32).to_be_bytes());
        out.extend_from_slice(part.as_bytes());
    }
    out
}

fn params(mode: Mode) -> Result<snow::params::NoiseParams, NoiseError> {
    mode.suite().parse().map_err(|_| NoiseError::Init)
}

pub struct Handshake {
    state: HandshakeState,
}

impl Handshake {
    /// The pairing initiator (the phone): knows the host key and the psk.
    pub fn initiator_pair(
        local: &SecretKey,
        host: &PublicKey,
        psk: &Psk,
        prologue: &[u8],
    ) -> Result<Self, NoiseError> {
        Self::build(Mode::Pair, local, Some(host), Some(psk), prologue, true)
    }

    /// The pairing responder (the host): learns the client's key from message 1.
    pub fn responder_pair(local: &SecretKey, psk: &Psk, prologue: &[u8]) -> Result<Self, NoiseError> {
        Self::build(Mode::Pair, local, None, Some(psk), prologue, false)
    }

    /// The reconnecting initiator: knows the host key.
    pub fn initiator_link(local: &SecretKey, host: &PublicKey, prologue: &[u8]) -> Result<Self, NoiseError> {
        Self::build(Mode::Link, local, Some(host), None, prologue, true)
    }

    /// The reconnecting responder: read `message1` against each candidate client key in turn
    /// and return the handshake (ready to write message 2), the index of the key that
    /// verified, and message 1's payload. `NoMatch` when none did.
    pub fn responder_link(
        local: &SecretKey,
        candidates: &[PublicKey],
        prologue: &[u8],
        message1: &[u8],
    ) -> Result<(Self, usize, Vec<u8>), NoiseError> {
        for (i, candidate) in candidates.iter().enumerate() {
            let Ok(mut hs) = Self::build(Mode::Link, local, Some(candidate), None, prologue, false) else {
                continue;
            };
            if let Ok(payload) = hs.read(message1) {
                return Ok((hs, i, payload));
            }
        }
        Err(NoiseError::NoMatch)
    }

    fn build(
        mode: Mode,
        local: &SecretKey,
        remote: Option<&PublicKey>,
        psk: Option<&Psk>,
        prologue: &[u8],
        initiator: bool,
    ) -> Result<Self, NoiseError> {
        let mut b = Builder::new(params(mode)?).local_private_key(local.expose()).map_err(|_| NoiseError::Init)?;
        b = b.prologue(prologue).map_err(|_| NoiseError::Init)?;
        if let Some(r) = remote {
            b = b.remote_public_key(r.as_bytes()).map_err(|_| NoiseError::Init)?;
        }
        if let Some(p) = psk {
            b = b.psk(2, p.expose()).map_err(|_| NoiseError::Init)?;
        }
        let state = if initiator { b.build_initiator() } else { b.build_responder() };
        Ok(Self { state: state.map_err(|_| NoiseError::Init)? })
    }

    pub fn write(&mut self, payload: &[u8]) -> Result<Vec<u8>, NoiseError> {
        let mut buf = vec![0u8; MAX_NOISE_MESSAGE];
        let n = self.state.write_message(payload, &mut buf).map_err(|_| NoiseError::Handshake)?;
        buf.truncate(n);
        Ok(buf)
    }

    pub fn read(&mut self, message: &[u8]) -> Result<Vec<u8>, NoiseError> {
        if message.len() > MAX_NOISE_MESSAGE {
            return Err(NoiseError::Handshake);
        }
        let mut buf = vec![0u8; MAX_NOISE_MESSAGE];
        let n = self.state.read_message(message, &mut buf).map_err(|_| NoiseError::Handshake)?;
        buf.truncate(n);
        Ok(buf)
    }

    pub fn is_finished(&self) -> bool {
        self.state.is_handshake_finished()
    }

    /// The peer's static key, once the message that carries it has been read.
    pub fn remote_static(&self) -> Option<PublicKey> {
        self.state.get_remote_static().and_then(PublicKey::from_slice)
    }

    pub fn into_transport(self) -> Result<Transport, NoiseError> {
        self.state.into_transport_mode().map(|state| Transport { state }).map_err(|_| NoiseError::Handshake)
    }
}

pub struct Transport {
    state: TransportState,
}

impl Transport {
    /// One Noise message: `plaintext` is at most `MAX_NOISE_MESSAGE - TAG_BYTES`.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, NoiseError> {
        if plaintext.len() > MAX_NOISE_MESSAGE - TAG_BYTES {
            return Err(NoiseError::Transport);
        }
        let mut buf = vec![0u8; plaintext.len() + TAG_BYTES];
        let n = self.state.write_message(plaintext, &mut buf).map_err(|_| NoiseError::Transport)?;
        buf.truncate(n);
        Ok(buf)
    }

    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, NoiseError> {
        if ciphertext.len() < TAG_BYTES || ciphertext.len() > MAX_NOISE_MESSAGE {
            return Err(NoiseError::Transport);
        }
        let mut buf = vec![0u8; ciphertext.len()];
        let n = self.state.read_message(ciphertext, &mut buf).map_err(|_| NoiseError::Transport)?;
        buf.truncate(n);
        Ok(buf)
    }
}

/// A handshake with the ephemeral key fixed, so a transcript is the same every run. Test
/// only: it exists to write and replay the vectors in `protocol/fixtures/noise`. It also
/// exposes the handshake hash those vectors record.
#[cfg(test)]
impl Handshake {
    pub(crate) fn with_ephemeral(
        mode: Mode,
        local: &SecretKey,
        remote: Option<&PublicKey>,
        psk: Option<&Psk>,
        prologue: &[u8],
        initiator: bool,
        ephemeral: &[u8; 32],
    ) -> Result<Self, NoiseError> {
        let mut b = Builder::new(params(mode)?).local_private_key(local.expose()).map_err(|_| NoiseError::Init)?;
        b = b.prologue(prologue).map_err(|_| NoiseError::Init)?;
        if let Some(r) = remote {
            b = b.remote_public_key(r.as_bytes()).map_err(|_| NoiseError::Init)?;
        }
        if let Some(p) = psk {
            b = b.psk(2, p.expose()).map_err(|_| NoiseError::Init)?;
        }
        b = b.fixed_ephemeral_key_for_testing_only(ephemeral);
        let state = if initiator { b.build_initiator() } else { b.build_responder() };
        Ok(Self { state: state.map_err(|_| NoiseError::Init)? })
    }

    pub(crate) fn handshake_hash(&self) -> Vec<u8> {
        self.state.get_handshake_hash().to_vec()
    }
}

/// Two connected transports from a finished KK handshake. For the tests of this module and
/// of `frame`.
#[cfg(test)]
pub(crate) fn test_transports() -> (Transport, Transport) {
    let (a, b) = (SecretKey::generate().unwrap(), SecretKey::generate().unwrap());
    let pro = prologue(Mode::Link, "req", "rep");
    let mut init = Handshake::initiator_link(&a, &b.public(), &pro).unwrap();
    let m1 = init.write(&[]).unwrap();
    let (mut resp, _, _) = Handshake::responder_link(&b, &[a.public()], &pro, &m1).unwrap();
    let m2 = resp.write(&[]).unwrap();
    init.read(&m2).unwrap();
    (init.into_transport().unwrap(), resp.into_transport().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (SecretKey, SecretKey) {
        (SecretKey::generate().unwrap(), SecretKey::generate().unwrap())
    }

    fn pro() -> Vec<u8> {
        prologue(Mode::Pair, r#"{"proto":"orchestra","v":[1]}"#, r#"{"proto":"orchestra","v":1}"#)
    }

    #[test]
    fn prologue_is_exact_and_binds_the_path_and_both_frames() {
        let p = prologue(Mode::Pair, "ab", "c");
        let mut want = b"orchestra-link v1".to_vec();
        want.extend_from_slice(&[0, 0, 0, 5]);
        want.extend_from_slice(b"/pair");
        want.extend_from_slice(&[0, 0, 0, 2]);
        want.extend_from_slice(b"ab");
        want.extend_from_slice(&[0, 0, 0, 1]);
        want.extend_from_slice(b"c");
        assert_eq!(p, want);
        assert_ne!(p, prologue(Mode::Link, "ab", "c"));
        assert_ne!(p, prologue(Mode::Pair, "ab", "d"));
        // Moving a byte between the two frames changes it too.
        assert_ne!(prologue(Mode::Pair, "ab", "c"), prologue(Mode::Pair, "a", "bc"));
    }

    #[test]
    fn mode_paths_round_trip() {
        assert_eq!(Mode::from_path("/pair"), Some(Mode::Pair));
        assert_eq!(Mode::from_path("/link"), Some(Mode::Link));
        assert_eq!(Mode::from_path("/"), None);
        assert_eq!(Mode::from_path("/link/"), None);
    }

    #[test]
    fn ikpsk2_round_trip_learns_the_client_key() {
        let (host, client) = pair();
        let psk = Psk::generate().unwrap();
        let psk2 = Psk::from_bytes(*psk.expose());
        let mut init = Handshake::initiator_pair(&client, &host.public(), &psk, &pro()).unwrap();
        let mut resp = Handshake::responder_pair(&host, &psk2, &pro()).unwrap();
        let m1 = init.write(&[]).unwrap();
        resp.read(&m1).unwrap();
        assert_eq!(resp.remote_static(), Some(client.public()));
        let m2 = resp.write(&[]).unwrap();
        init.read(&m2).unwrap();
        assert!(init.is_finished() && resp.is_finished());
        let (mut ct, mut ht) = (init.into_transport().unwrap(), resp.into_transport().unwrap());
        let wire = ct.encrypt(b"hello").unwrap();
        assert_eq!(ht.decrypt(&wire).unwrap(), b"hello");
        let back = ht.encrypt(b"welcome").unwrap();
        assert_eq!(ct.decrypt(&back).unwrap(), b"welcome");
    }

    #[test]
    fn ikpsk2_wrong_psk_never_reaches_a_transport_message() {
        let (host, client) = pair();
        let (psk, wrong) = (Psk::generate().unwrap(), Psk::generate().unwrap());
        let mut init = Handshake::initiator_pair(&client, &host.public(), &psk, &pro()).unwrap();
        let mut resp = Handshake::responder_pair(&host, &wrong, &pro()).unwrap();
        resp.read(&init.write(&[]).unwrap()).unwrap();
        // The psk is mixed in message 2: the initiator cannot open it, so it never gets a
        // transport to send a hello with, and the host's first read fails.
        assert!(init.read(&resp.write(&[]).unwrap()).is_err());
        let mut ht = resp.into_transport().unwrap();
        assert!(ht.decrypt(&[0u8; 40]).is_err());
    }

    #[test]
    fn ikpsk2_wrong_host_key_or_prologue_fails() {
        let (host, client) = pair();
        let other = SecretKey::generate().unwrap();
        let psk = Psk::generate().unwrap();
        let psk2 = Psk::from_bytes(*psk.expose());
        let mut init = Handshake::initiator_pair(&client, &other.public(), &psk, &pro()).unwrap();
        let mut resp = Handshake::responder_pair(&host, &psk2, &pro()).unwrap();
        assert!(resp.read(&init.write(&[]).unwrap()).is_err());

        let mut init = Handshake::initiator_pair(&client, &host.public(), &psk, &pro()).unwrap();
        let swapped = prologue(Mode::Link, "x", "y");
        let mut resp = Handshake::responder_pair(&host, &psk2, &swapped).unwrap();
        assert!(resp.read(&init.write(&[]).unwrap()).is_err());
    }

    #[test]
    fn kk_round_trip() {
        let (mut ct, mut ht) = test_transports();
        let m = ct.encrypt(b"ping").unwrap();
        assert_eq!(ht.decrypt(&m).unwrap(), b"ping");
        // A replayed or reordered message does not decrypt.
        assert!(ht.decrypt(&m).is_err());
    }

    #[test]
    fn kk_trial_decryption_picks_the_right_key_among_several() {
        let host = SecretKey::generate().unwrap();
        let clients: Vec<SecretKey> = (0..5).map(|_| SecretKey::generate().unwrap()).collect();
        let keys: Vec<PublicKey> = clients.iter().map(SecretKey::public).collect();
        let pro = prologue(Mode::Link, "q", "r");
        for (i, c) in clients.iter().enumerate() {
            let mut init = Handshake::initiator_link(c, &host.public(), &pro).unwrap();
            let m1 = init.write(&[]).unwrap();
            let (mut resp, found, _) = Handshake::responder_link(&host, &keys, &pro, &m1).unwrap();
            assert_eq!(found, i);
            assert_eq!(resp.remote_static(), Some(keys[i]));
            init.read(&resp.write(&[]).unwrap()).unwrap();
            let (mut ct, mut ht) = (init.into_transport().unwrap(), resp.into_transport().unwrap());
            assert_eq!(ht.decrypt(&ct.encrypt(b"x").unwrap()).unwrap(), b"x");
        }
    }

    #[test]
    fn kk_unknown_key_and_wrong_prologue_fail() {
        let host = SecretKey::generate().unwrap();
        let known = SecretKey::generate().unwrap();
        let stranger = SecretKey::generate().unwrap();
        let pro = prologue(Mode::Link, "q", "r");
        let mut init = Handshake::initiator_link(&stranger, &host.public(), &pro).unwrap();
        let m1 = init.write(&[]).unwrap();
        assert_eq!(Handshake::responder_link(&host, &[known.public()], &pro, &m1).err(), Some(NoiseError::NoMatch));
        assert_eq!(Handshake::responder_link(&host, &[], &pro, &m1).err(), Some(NoiseError::NoMatch));

        let mut init = Handshake::initiator_link(&known, &host.public(), &pro).unwrap();
        let m1 = init.write(&[]).unwrap();
        let other_pro = prologue(Mode::Link, "q", "different");
        assert!(Handshake::responder_link(&host, &[known.public()], &other_pro, &m1).is_err());
        assert!(Handshake::responder_link(&host, &[known.public()], &pro, &m1[..20]).is_err());
    }

    #[test]
    fn transport_refuses_oversize_and_garbage() {
        let (mut ct, mut ht) = test_transports();
        assert!(ct.encrypt(&vec![0u8; MAX_NOISE_MESSAGE - TAG_BYTES + 1]).is_err());
        assert!(ct.encrypt(&vec![0u8; MAX_NOISE_MESSAGE - TAG_BYTES]).is_ok());
        assert!(ht.decrypt(&[1, 2, 3]).is_err());
        assert!(ht.decrypt(&vec![0u8; MAX_NOISE_MESSAGE + 1]).is_err());
    }
}

//! Key material and where the host's private key lives. A secret is a type that cannot be
//! printed, cloned or serialised by accident: `Debug` is redacted, there is no `Display`,
//! `Clone` or `Serialize`, and the bytes are overwritten on drop. Randomness is the OS's.

use std::fmt;
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use snow::params::{DHChoice, HashChoice};
use snow::resolvers::{CryptoResolver, DefaultResolver};

pub const KEY_BYTES: usize = 32;
const HOST_KEY_FILE: &str = "host_key";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyError {
    /// The OS refused to give random bytes.
    Random,
    /// A key file is not exactly 32 bytes. It is never overwritten.
    BadFile,
    Io(ErrorKind),
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyError::Random => f.write_str("the system could not provide random bytes"),
            KeyError::BadFile => f.write_str("the host key file is not a 32 byte key"),
            KeyError::Io(kind) => write!(f, "could not read or write the host key ({kind})"),
        }
    }
}

impl std::error::Error for KeyError {}

impl From<std::io::Error> for KeyError {
    fn from(e: std::io::Error) -> Self {
        KeyError::Io(e.kind())
    }
}

/// Fill from the OS generator. The only source of randomness in the link.
pub fn random_bytes<const N: usize>() -> Result<[u8; N], KeyError> {
    let mut out = [0u8; N];
    getrandom::fill(&mut out).map_err(|_| KeyError::Random)?;
    Ok(out)
}

fn wipe(bytes: &mut [u8]) {
    for b in bytes.iter_mut() {
        // SAFETY: `b` is a valid, aligned, exclusive reference; volatile keeps the write.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
}

pub fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[usize::from(b >> 4)] as char);
        s.push(DIGITS[usize::from(b & 15)] as char);
    }
    s
}

/// Lowercase or uppercase hex of exactly `N` bytes.
pub fn from_hex<const N: usize>(s: &str) -> Option<[u8; N]> {
    let s = s.as_bytes();
    if s.len() != N * 2 {
        return None;
    }
    let nibble = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    let mut out = [0u8; N];
    for (i, pair) in s.chunks(2).enumerate() {
        out[i] = nibble(pair[0])? << 4 | nibble(pair[1])?;
    }
    Some(out)
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    if let Some(mut h) = DefaultResolver.resolve_hash(&HashChoice::SHA256) {
        h.input(data);
        h.result(&mut out);
    }
    out
}

/// A Curve25519 private key. No `Clone`, `Display` or `Serialize`; `Debug` is redacted.
pub struct SecretKey([u8; KEY_BYTES]);

impl SecretKey {
    pub fn generate() -> Result<Self, KeyError> {
        Ok(Self(random_bytes()?))
    }

    pub fn from_bytes(bytes: [u8; KEY_BYTES]) -> Self {
        Self(bytes)
    }

    /// For the Noise builder and the key file. Do not log or format the result.
    pub fn expose(&self) -> &[u8; KEY_BYTES] {
        &self.0
    }

    pub fn public(&self) -> PublicKey {
        let mut out = [0u8; KEY_BYTES];
        if let Some(mut dh) = DefaultResolver.resolve_dh(&DHChoice::Curve25519) {
            dh.set(&self.0);
            out.copy_from_slice(dh.pubkey());
        }
        PublicKey(out)
    }
}

impl Drop for SecretKey {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretKey(<redacted>)")
    }
}

/// The 32 byte pairing secret, used as the IKpsk2 psk. Same rules as `SecretKey`.
pub struct Psk([u8; KEY_BYTES]);

impl Psk {
    pub fn generate() -> Result<Self, KeyError> {
        Ok(Self(random_bytes()?))
    }

    pub fn from_bytes(bytes: [u8; KEY_BYTES]) -> Self {
        Self(bytes)
    }

    pub fn expose(&self) -> &[u8; KEY_BYTES] {
        &self.0
    }
}

impl Drop for Psk {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

impl fmt::Debug for Psk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Psk(<redacted>)")
    }
}

/// A Curve25519 public key. Not secret, so it may be shown and stored.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublicKey([u8; KEY_BYTES]);

impl PublicKey {
    pub fn from_bytes(bytes: [u8; KEY_BYTES]) -> Self {
        Self(bytes)
    }

    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        <[u8; KEY_BYTES]>::try_from(bytes).ok().map(Self)
    }

    pub fn as_bytes(&self) -> &[u8; KEY_BYTES] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        to_hex(&self.0)
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        from_hex::<KEY_BYTES>(s).map(Self)
    }

    /// The first 8 bytes of SHA-256 of the key as lowercase hex in groups of four, joined
    /// by `-`: what the host shows next to a pairing request.
    pub fn fingerprint(&self) -> String {
        let hex = to_hex(&sha256(&self.0)[..8]);
        hex.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join("-")
    }

    /// Hex of the first 16 bytes of SHA-256 of the key. Names a host to its clients.
    pub fn host_id(&self) -> String {
        to_hex(&sha256(&self.0)[..16])
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", self.to_hex())
    }
}

/// Where the host's private key comes from. The desktop will add an OS keyring provider in
/// its own crate; this one has no dependency on any.
pub trait KeyProvider: Send + Sync {
    /// The same key on every call; created on the first.
    fn load_or_create(&self) -> Result<SecretKey, KeyError>;
}

/// `<link dir>/host_key`: 32 raw bytes, created once with mode 0600 on Unix. On Windows it
/// inherits the per-user app data folder it lives in.
pub struct FileKeyProvider {
    path: PathBuf,
}

impl FileKeyProvider {
    pub fn new(link_dir: &Path) -> Self {
        Self { path: link_dir.join(HOST_KEY_FILE) }
    }

    fn read(&self) -> Result<SecretKey, KeyError> {
        let mut f = fs::File::open(&self.path)?;
        let mut bytes = [0u8; KEY_BYTES];
        let mut extra = [0u8; 1];
        let ok = f.read_exact(&mut bytes).is_ok() && f.read(&mut extra)? == 0;
        if !ok {
            wipe(&mut bytes);
            return Err(KeyError::BadFile);
        }
        tighten(&self.path);
        let key = SecretKey::from_bytes(bytes);
        wipe(&mut bytes);
        Ok(key)
    }
}

/// Owner read and write only. Best effort on a file that already exists.
#[cfg(unix)]
fn tighten(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = fs::metadata(path) {
        if meta.permissions().mode() & 0o077 != 0 {
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        }
    }
}

#[cfg(not(unix))]
fn tighten(_path: &Path) {}

#[cfg(unix)]
fn create_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

#[cfg(not(unix))]
fn create_dir(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)
}

fn create_new(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

impl KeyProvider for FileKeyProvider {
    fn load_or_create(&self) -> Result<SecretKey, KeyError> {
        match self.read() {
            Ok(key) => return Ok(key),
            Err(KeyError::Io(ErrorKind::NotFound)) => {}
            Err(e) => return Err(e),
        }
        if let Some(dir) = self.path.parent() {
            create_dir(dir)?;
        }
        let key = SecretKey::generate()?;
        match create_new(&self.path) {
            Ok(mut f) => {
                f.write_all(key.expose())?;
                f.sync_all()?;
                Ok(key)
            }
            // Another thread or process created it first: use theirs.
            Err(e) if e.kind() == ErrorKind::AlreadyExists => self.read(),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);

    pub(crate) fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "orchestra-link-{name}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn key_debug_output_does_not_contain_the_key() {
        let key = SecretKey::generate().unwrap();
        let hex = to_hex(key.expose());
        let shown = format!("{key:?} {:#?}", key);
        assert!(!shown.contains(&hex));
        assert!(!shown.contains(&hex[..8]));
        assert!(shown.contains("redacted"));
        let psk = Psk::generate().unwrap();
        let hex = to_hex(psk.expose());
        assert!(!format!("{psk:?}").contains(&hex[..8]));
    }

    #[test]
    fn public_key_is_derived_and_stable() {
        let key = SecretKey::from_bytes([7u8; 32]);
        assert_eq!(key.public(), key.public());
        assert_ne!(key.public(), SecretKey::from_bytes([8u8; 32]).public());
        assert_ne!(key.public().as_bytes(), key.expose());
    }

    #[test]
    fn hex_round_trips_and_rejects_bad_input() {
        let bytes = [0u8, 1, 0xab, 0xff];
        assert_eq!(to_hex(&bytes), "0001abff");
        assert_eq!(from_hex::<4>("0001ABFF"), Some(bytes));
        assert_eq!(from_hex::<4>("0001abf"), None);
        assert_eq!(from_hex::<4>("0001abfg"), None);
        assert_eq!(from_hex::<3>("0001abff"), None);
    }

    #[test]
    fn fingerprint_and_host_id_have_their_documented_shape() {
        let pk = SecretKey::from_bytes([3u8; 32]).public();
        let fp = pk.fingerprint();
        assert_eq!(fp.len(), 19);
        assert!(fp.split('-').all(|g| g.len() == 4 && g.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())));
        assert_eq!(pk.host_id().len(), 32);
        assert_eq!(pk.host_id(), pk.host_id());
        // SHA-256 of the empty input, to pin the hash.
        assert_eq!(to_hex(&sha256(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn file_key_is_created_once_and_reused_across_loads() {
        let dir = scratch("keyfile");
        let first = FileKeyProvider::new(&dir.join("link")).load_or_create().unwrap();
        let again = FileKeyProvider::new(&dir.join("link")).load_or_create().unwrap();
        assert_eq!(first.expose(), again.expose());
        assert_eq!(fs::metadata(dir.join("link").join("host_key")).unwrap().len(), 32);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_malformed_key_file_is_refused_and_left_alone() {
        let dir = scratch("badkey");
        fs::write(dir.join("host_key"), b"short").unwrap();
        assert_eq!(FileKeyProvider::new(&dir).load_or_create().unwrap_err(), KeyError::BadFile);
        assert_eq!(fs::read(dir.join("host_key")).unwrap(), b"short");
        fs::write(dir.join("host_key"), [1u8; 33]).unwrap();
        assert_eq!(FileKeyProvider::new(&dir).load_or_create().unwrap_err(), KeyError::BadFile);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn host_key_is_mode_0600_and_its_folder_0700() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("keymode");
        let link = dir.join("link");
        FileKeyProvider::new(&link).load_or_create().unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&link.join("host_key")), 0o600);
        assert_eq!(mode(&link), 0o700);
        // A looser file is tightened on the next load.
        fs::set_permissions(link.join("host_key"), fs::Permissions::from_mode(0o644)).unwrap();
        FileKeyProvider::new(&link).load_or_create().unwrap();
        assert_eq!(mode(&link.join("host_key")), 0o600);
        let _ = fs::remove_dir_all(&dir);
    }
}

//! The paired clients: `<link dir>/clients.json`, schema 1. It holds public keys, names and
//! scopes, never a secret. A file with a schema this build does not know, or one that does
//! not parse, refuses to open and is never overwritten: the link fails closed.

use std::fmt;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::keys::{PublicKey, random_bytes, to_hex};
use crate::wire::Scope;

pub const SCHEMA: u64 = 1;
/// Bounds the trial decryption of a reconnect (one DH pair per key).
pub const MAX_CLIENTS: usize = 32;
/// Granted when the host confirms a pairing without choosing.
pub const DEFAULT_SCOPES: [Scope; 2] = [Scope::Read, Scope::Answer];
const FILE: &str = "clients.json";
const MAX_NAME_BYTES: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub id: String,
    pub name: String,
    pub public_key: PublicKey,
    /// Only scopes this build knows. An unknown string in the file grants nothing.
    pub scopes: Vec<Scope>,
    pub paired_at: u64,
}

impl Client {
    pub fn has(&self, scope: Scope) -> bool {
        self.scopes.contains(&scope)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// A `schema` other than the one this build writes. Nothing is read or written.
    UnknownSchema(u64),
    /// The file is there but is not a clients file.
    Invalid,
    Full,
    /// This key is already paired.
    Duplicate,
    UnknownClient,
    Io(std::io::ErrorKind),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::UnknownSchema(v) => write!(f, "clients file has schema {v}, which this build does not know"),
            StoreError::Invalid => f.write_str("clients file is not valid"),
            StoreError::Full => f.write_str("too many paired clients"),
            StoreError::Duplicate => f.write_str("this device is already paired"),
            StoreError::UnknownClient => f.write_str("unknown client"),
            StoreError::Io(kind) => write!(f, "could not read or write the clients file ({kind})"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e.kind())
    }
}

#[derive(Serialize, Deserialize)]
struct File1 {
    schema: u64,
    clients: Vec<Stored>,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    id: String,
    name: String,
    public_key: String,
    scopes: Vec<String>,
    paired_at: u64,
}

impl Stored {
    fn from_client(c: &Client) -> Self {
        Stored {
            id: c.id.clone(),
            name: c.name.clone(),
            public_key: c.public_key.to_hex(),
            scopes: c.scopes.iter().map(|s| s.as_str().to_string()).collect(),
            paired_at: c.paired_at,
        }
    }

    fn into_client(self) -> Option<Client> {
        let scopes = uniq(
            self.scopes.iter().filter_map(|s| Scope::ALL.into_iter().find(|k| k.as_str() == s)).collect(),
        );
        Some(Client {
            id: self.id,
            name: self.name,
            public_key: PublicKey::from_hex(&self.public_key)?,
            scopes,
            paired_at: self.paired_at,
        })
    }
}

pub struct ClientStore {
    path: PathBuf,
    clients: Vec<Client>,
}

impl ClientStore {
    /// Open `<link dir>/clients.json`. A missing file is an empty store; nothing is written
    /// until the first change.
    pub fn open(link_dir: &Path) -> Result<Self, StoreError> {
        let path = link_dir.join(FILE);
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self { path, clients: Vec::new() });
            }
            Err(e) => return Err(e.into()),
        };
        // Read the schema before anything else, so a newer file is told apart from a bad one.
        let raw: serde_json::Value = serde_json::from_str(&text).map_err(|_| StoreError::Invalid)?;
        match raw.get("schema").and_then(serde_json::Value::as_u64) {
            Some(SCHEMA) => {}
            Some(other) => return Err(StoreError::UnknownSchema(other)),
            None => return Err(StoreError::Invalid),
        }
        let file: File1 = serde_json::from_value(raw).map_err(|_| StoreError::Invalid)?;
        if file.clients.len() > MAX_CLIENTS {
            return Err(StoreError::Invalid);
        }
        let mut clients = Vec::with_capacity(file.clients.len());
        for stored in file.clients {
            let client = stored.into_client().ok_or(StoreError::Invalid)?;
            if clients.iter().any(|c: &Client| c.id == client.id || c.public_key == client.public_key) {
                return Err(StoreError::Invalid);
            }
            clients.push(client);
        }
        Ok(Self { path, clients })
    }

    pub fn list(&self) -> &[Client] {
        &self.clients
    }

    pub fn get(&self, id: &str) -> Option<&Client> {
        self.clients.iter().find(|c| c.id == id)
    }

    pub fn find_by_key(&self, key: &PublicKey) -> Option<&Client> {
        self.clients.iter().find(|c| &c.public_key == key)
    }

    pub fn keys(&self) -> Vec<PublicKey> {
        self.clients.iter().map(|c| c.public_key).collect()
    }

    /// Store a newly confirmed client and return it. The name is cut to a sane length.
    pub fn add(
        &mut self,
        name: &str,
        public_key: PublicKey,
        scopes: &[Scope],
        paired_at: u64,
    ) -> Result<Client, StoreError> {
        if self.clients.len() >= MAX_CLIENTS {
            return Err(StoreError::Full);
        }
        if self.find_by_key(&public_key).is_some() {
            return Err(StoreError::Duplicate);
        }
        let id = format!("c-{}", to_hex(&random_bytes::<8>().map_err(|_| StoreError::Io(std::io::ErrorKind::Other))?));
        let client = Client { id, name: clip(name), public_key, scopes: uniq(scopes.to_vec()), paired_at };
        self.clients.push(client.clone());
        if let Err(e) = self.save() {
            self.clients.pop();
            return Err(e);
        }
        Ok(client)
    }

    pub fn set_scopes(&mut self, id: &str, scopes: &[Scope]) -> Result<(), StoreError> {
        let i = self.clients.iter().position(|c| c.id == id).ok_or(StoreError::UnknownClient)?;
        let before = std::mem::replace(&mut self.clients[i].scopes, uniq(scopes.to_vec()));
        if let Err(e) = self.save() {
            self.clients[i].scopes = before;
            return Err(e);
        }
        Ok(())
    }

    pub fn revoke(&mut self, id: &str) -> Result<(), StoreError> {
        let i = self.clients.iter().position(|c| c.id == id).ok_or(StoreError::UnknownClient)?;
        let removed = self.clients.remove(i);
        if let Err(e) = self.save() {
            self.clients.insert(i, removed);
            return Err(e);
        }
        Ok(())
    }

    /// Write a sibling temp file, flush it to disk, then rename over the target.
    fn save(&self) -> Result<(), StoreError> {
        let file = File1 { schema: SCHEMA, clients: self.clients.iter().map(Stored::from_client).collect() };
        let text = serde_json::to_string_pretty(&file).map_err(|_| StoreError::Invalid)?;
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let mut f = File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// Each scope once, in first-seen order.
fn uniq(scopes: Vec<Scope>) -> Vec<Scope> {
    let mut out: Vec<Scope> = Vec::with_capacity(scopes.len());
    for s in scopes {
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// A name fit to show and store: control and bidirectional-formatting characters dropped
/// (they can hide or reorder what a person reads), trimmed, at most `MAX_NAME_BYTES` bytes
/// on a char boundary.
pub(crate) fn clip(name: &str) -> String {
    let cleaned: String = name.chars().filter(|c| !c.is_control() && !is_bidi_format(*c)).collect();
    let name = cleaned.trim();
    if name.len() <= MAX_NAME_BYTES {
        return name.to_string();
    }
    let mut end = MAX_NAME_BYTES;
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].trim_end().to_string()
}

/// Marks, embeddings, overrides and isolates (U+061C, U+200E/F, U+202A-E, U+2066-9).
fn is_bidi_format(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

#[cfg(test)]
mod tests {
    use super::super::keys::SecretKey;
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "orchestra-clients-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn key() -> PublicKey {
        SecretKey::generate().unwrap().public()
    }

    #[test]
    fn clients_json_round_trip() {
        let dir = scratch();
        let mut store = ClientStore::open(&dir).unwrap();
        assert!(store.list().is_empty());
        assert!(!dir.join("clients.json").exists(), "opening writes nothing");
        let (k1, k2) = (key(), key());
        let a = store.add("Pixel", k1, &[Scope::Read, Scope::Answer], 100).unwrap();
        let b = store.add("iPad", k2, &[Scope::Read], 200).unwrap();
        assert_ne!(a.id, b.id);
        let again = ClientStore::open(&dir).unwrap();
        assert_eq!(again.list(), store.list());
        assert_eq!(again.find_by_key(&k1).unwrap().name, "Pixel");
        assert!(again.get(&b.id).unwrap().has(Scope::Read));
        assert!(!again.get(&b.id).unwrap().has(Scope::Land));
        assert!(!dir.join("clients.json.tmp").exists());
        let text = fs::read_to_string(dir.join("clients.json")).unwrap();
        assert!(text.contains("\"schema\": 1"));
    }

    #[test]
    fn set_scopes_and_revoke_persist() {
        let dir = scratch();
        let mut store = ClientStore::open(&dir).unwrap();
        let a = store.add("Pixel", key(), &DEFAULT_SCOPES, 1).unwrap();
        store.set_scopes(&a.id, &[Scope::Read, Scope::Land]).unwrap();
        assert_eq!(ClientStore::open(&dir).unwrap().get(&a.id).unwrap().scopes, vec![Scope::Read, Scope::Land]);
        store.revoke(&a.id).unwrap();
        assert!(ClientStore::open(&dir).unwrap().list().is_empty());
        assert_eq!(store.revoke(&a.id), Err(StoreError::UnknownClient));
        assert_eq!(store.set_scopes("nope", &[]), Err(StoreError::UnknownClient));
    }

    #[test]
    fn unknown_clients_schema_is_refused_with_the_file_untouched() {
        let dir = scratch();
        let text = r#"{"schema":2,"clients":[],"future":true}"#;
        fs::write(dir.join("clients.json"), text).unwrap();
        assert_eq!(ClientStore::open(&dir).err(), Some(StoreError::UnknownSchema(2)));
        assert_eq!(fs::read_to_string(dir.join("clients.json")).unwrap(), text);
        for bad in ["not json", "[]", r#"{"clients":[]}"#, r#"{"schema":"1","clients":[]}"#] {
            fs::write(dir.join("clients.json"), bad).unwrap();
            assert_eq!(ClientStore::open(&dir).err(), Some(StoreError::Invalid), "{bad}");
            assert_eq!(fs::read_to_string(dir.join("clients.json")).unwrap(), bad);
        }
    }

    #[test]
    fn unknown_scope_strings_grant_nothing() {
        let dir = scratch();
        let k = key();
        let text = format!(
            r#"{{"schema":1,"clients":[{{"id":"c-1","name":"x","public_key":"{}","scopes":["read","root","LAND",""],"paired_at":5}}]}}"#,
            k.to_hex()
        );
        fs::write(dir.join("clients.json"), text).unwrap();
        let store = ClientStore::open(&dir).unwrap();
        let c = store.get("c-1").unwrap();
        assert_eq!(c.scopes, vec![Scope::Read]);
        assert!(!c.has(Scope::Land) && !c.has(Scope::Control));
    }

    #[test]
    fn a_bad_entry_fails_the_whole_file_closed() {
        let dir = scratch();
        let k = key().to_hex();
        let entry = |id: &str, pk: &str| {
            format!(r#"{{"id":"{id}","name":"x","public_key":"{pk}","scopes":[],"paired_at":1}}"#)
        };
        let cases = [
            format!(r#"{{"schema":1,"clients":[{}]}}"#, entry("c-1", "nothex")),
            format!(r#"{{"schema":1,"clients":[{},{}]}}"#, entry("c-1", &k), entry("c-2", &k)),
            format!(r#"{{"schema":1,"clients":[{},{}]}}"#, entry("c-1", &k), entry("c-1", &key().to_hex())),
        ];
        for case in cases {
            fs::write(dir.join("clients.json"), &case).unwrap();
            assert_eq!(ClientStore::open(&dir).err(), Some(StoreError::Invalid));
        }
    }

    #[test]
    fn the_client_cap_and_duplicate_keys_are_enforced() {
        let dir = scratch();
        let mut store = ClientStore::open(&dir).unwrap();
        let first = key();
        store.add("one", first, &[], 1).unwrap();
        assert_eq!(store.add("again", first, &[], 2), Err(StoreError::Duplicate));
        for i in 1..MAX_CLIENTS {
            store.add(&format!("c{i}"), key(), &[], 3).unwrap();
        }
        assert_eq!(store.add("extra", key(), &[], 4), Err(StoreError::Full));
        assert_eq!(store.list().len(), MAX_CLIENTS);
        assert_eq!(ClientStore::open(&dir).unwrap().list().len(), MAX_CLIENTS);
    }

    #[test]
    fn names_are_trimmed_and_cut_on_a_char_boundary() {
        let dir = scratch();
        let mut store = ClientStore::open(&dir).unwrap();
        let long = format!(" {} ", "\u{00e9}".repeat(200));
        let c = store.add(&long, key(), &[], 1).unwrap();
        assert!(c.name.len() <= MAX_NAME_BYTES);
        assert!(c.name.chars().all(|ch| ch == '\u{00e9}'));
    }

    #[test]
    fn pairing_name_clip_drops_control_and_bidi_characters() {
        assert_eq!(clip("  Pi\u{0}x\u{1b}el\n"), "Pixel");
        assert_eq!(clip("a\u{202E}b\u{2066}c\u{200F}d"), "abcd");
        assert_eq!(clip("\u{202E}\u{0007}"), "");
        let long = clip(&"x".repeat(4 << 20));
        assert_eq!(long.len(), MAX_NAME_BYTES);
        // A multi-byte name is cut on a char boundary and never ends in a space.
        let cut = clip(&format!("{}\u{00e9}", "y".repeat(MAX_NAME_BYTES - 1)));
        assert_eq!(cut, "y".repeat(MAX_NAME_BYTES - 1));
    }
}

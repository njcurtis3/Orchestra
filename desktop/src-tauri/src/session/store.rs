//! On-disk state: one JSON file under the app data dir, replaced atomically. The absolute
//! project paths live only in this Rust-written file and never cross IPC.
//!
//! An unknown schema version or an unreadable file is never overwritten: `open` hands back
//! no writer, so saving stays off for the whole run and the file is left as found.

use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::state::PersistedSession;
use crate::projects::StoredProject;

pub const SCHEMA: u32 = 2;
/// Oldest schema `load` still reads. A schema-1 file has no tasks and is written back as 2.
const OLDEST_SCHEMA: u64 = 1;
const FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredSession {
    pub id: String,
    pub project_id: String,
    pub project_name: String,
    pub started_at: u64,
    /// Settings as the strings the allowlist maps to. Optional within schema 1: an older
    /// file has neither, and an unknown value loads as no setting.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub permission_mode: Option<String>,
    /// The task this session belongs to; absent for a plain session and in schema 1.
    #[serde(default)]
    pub task_id: Option<String>,
    pub state: PersistedSession,
}

/// A task as written to the state file. The status is a string that `TaskStatus::parse`
/// reads back, so an unknown one loads as `error` and does not make the file unreadable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredTask {
    pub id: String,
    pub project_id: String,
    pub project_name: String,
    pub prompt: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub permission_mode: Option<String>,
    pub created_at: u64,
    pub status: String,
    #[serde(default)]
    pub session_id: Option<String>,
    /// A Rust-derived path under the worktree folder. Re-checked on load; never sent over IPC.
    #[serde(default)]
    pub worktree: Option<PathBuf>,
    #[serde(default)]
    pub branch: Option<String>,
    pub base_branch: String,
    pub base_commit: String,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub last_turn_error: bool,
    #[serde(default)]
    pub session_over: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema: u32,
    pub revision: u64,
    pub next_session: u64,
    pub next_project: u64,
    pub projects: Vec<StoredProject>,
    pub sessions: Vec<StoredSession>,
    /// Absent in schema 1.
    #[serde(default)]
    pub next_task: u64,
    #[serde(default)]
    pub tasks: Vec<StoredTask>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LoadError {
    UnknownSchema { found: u64 },
    Unreadable,
}

/// What the frontend may know about storage: no path, ever.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StorageStatus {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub found_version: Option<u64>,
}

impl StorageStatus {
    pub fn ok() -> Self {
        Self { ok: true, reason: None, found_version: None }
    }

    pub fn unreadable() -> Self {
        Self { ok: false, reason: Some("unreadable"), found_version: None }
    }

    fn from_error(e: &LoadError) -> Self {
        match e {
            LoadError::UnknownSchema { found } => {
                Self { ok: false, reason: Some("unknown_schema_version"), found_version: Some(*found) }
            }
            LoadError::Unreadable => Self::unreadable(),
        }
    }
}

/// `Ok(None)` when there is no file yet.
pub fn load(path: &Path) -> Result<Option<Snapshot>, LoadError> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(LoadError::Unreadable),
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| LoadError::Unreadable)?;
    let found = value.get("schema").and_then(serde_json::Value::as_u64).ok_or(LoadError::Unreadable)?;
    if !(OLDEST_SCHEMA..=u64::from(SCHEMA)).contains(&found) {
        return Err(LoadError::UnknownSchema { found });
    }
    let mut snapshot: Snapshot = serde_json::from_value(value).map_err(|_| LoadError::Unreadable)?;
    // Migrated in memory; the next save writes the current schema.
    snapshot.schema = SCHEMA;
    Ok(Some(snapshot))
}

/// Write a sibling temp file, flush it to disk, then rename over the target. The rename
/// replaces atomically, so a crash leaves either the old file or the new one.
pub fn save(path: &Path, snapshot: &Snapshot) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec(snapshot).map_err(std::io::Error::other)?;
    let mut f = File::create(&tmp)?;
    f.write_all(&bytes)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, path)
}

/// Load the file and decide whether saving is on. On any error the file is untouched and
/// there is no writer.
pub fn open(path: &Path) -> (Option<Snapshot>, StorageStatus, Option<Writer>) {
    match load(path) {
        Ok(snap) => (snap, StorageStatus::ok(), Some(Writer::new(path.to_path_buf()))),
        Err(e) => (None, StorageStatus::from_error(&e), None),
    }
}

enum Msg {
    Save(Snapshot),
    Flush(Sender<()>),
}

/// One background thread writes snapshots. It coalesces to the newest and never writes a
/// revision at or below the last one written, so an older snapshot cannot replace a newer.
pub struct Writer {
    tx: Sender<Msg>,
}

impl Writer {
    pub fn new(path: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel::<Msg>();
        std::thread::spawn(move || {
            let mut last: Option<u64> = None;
            while let Ok(first) = rx.recv() {
                let mut newest: Option<Snapshot> = None;
                let mut acks: Vec<Sender<()>> = Vec::new();
                let mut take = |m: Msg| match m {
                    Msg::Save(s) => {
                        if newest.as_ref().is_none_or(|n| s.revision > n.revision) {
                            newest = Some(s);
                        }
                    }
                    Msg::Flush(a) => acks.push(a),
                };
                take(first);
                while let Ok(m) = rx.try_recv() {
                    take(m);
                }
                if let Some(s) = newest.filter(|s| last.is_none_or(|l| s.revision > l)) {
                    // A failed write is retried by the next save; there is nowhere to report it.
                    if save(&path, &s).is_ok() {
                        last = Some(s.revision);
                    }
                }
                for a in acks {
                    let _ = a.send(());
                }
            }
        });
        Self { tx }
    }

    /// Queue a snapshot. Never blocks on disk.
    pub fn save(&self, snapshot: Snapshot) {
        let _ = self.tx.send(Msg::Save(snapshot));
    }

    /// Wait until everything queued so far has been handled.
    pub fn flush(&self) {
        let (ack, done) = mpsc::channel();
        if self.tx.send(Msg::Flush(ack)).is_ok() {
            let _ = done.recv_timeout(FLUSH_TIMEOUT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("orchestra-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn snap(revision: u64) -> Snapshot {
        Snapshot {
            schema: SCHEMA,
            revision,
            next_session: 0,
            next_project: 0,
            projects: vec![],
            sessions: vec![],
            next_task: 0,
            tasks: vec![],
        }
    }

    #[test]
    fn refuses_unknown_schema_version() {
        let d = dir("schema");
        let f = d.join("state.json");
        let future = br#"{"schema":3,"revision":9,"anything":["kept"]}"#;
        fs::write(&f, future).unwrap();
        assert_eq!(load(&f), Err(LoadError::UnknownSchema { found: 3 }));
        let (snapshot, status, writer) = open(&f);
        assert!(snapshot.is_none() && writer.is_none(), "saving is off for the run");
        assert_eq!(status, StorageStatus { ok: false, reason: Some("unknown_schema_version"), found_version: Some(3) });
        assert_eq!(fs::read(&f).unwrap(), future, "bytes unchanged after load");
        // Nothing can be saved without a writer, and the temp file is never created.
        assert!(writer.is_none());
        assert_eq!(fs::read(&f).unwrap(), future, "bytes unchanged after a save attempt");
        assert!(!f.with_extension("json.tmp").exists());

        fs::write(&f, b"not json").unwrap();
        let (_, status, writer) = open(&f);
        assert!(writer.is_none());
        assert_eq!(status, StorageStatus::unreadable());
        assert_eq!(fs::read(&f).unwrap(), b"not json");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn schema3_is_refused() {
        let d = dir("schema3");
        let f = d.join("state.json");
        let future = br#"{"schema":3,"revision":1,"next_session":0,"next_project":0,"projects":[],"sessions":[]}"#;
        fs::write(&f, future).unwrap();
        assert_eq!(load(&f), Err(LoadError::UnknownSchema { found: 3 }));
        fs::write(&f, br#"{"schema":0}"#).unwrap();
        assert_eq!(load(&f), Err(LoadError::UnknownSchema { found: 0 }));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn schema1_file_loads_as_schema2_with_no_tasks() {
        let d = dir("migrate");
        let f = d.join("state.json");
        // Shaped like a D3 file: settings present, no task fields anywhere.
        let old = br#"{"schema":1,"revision":7,"next_session":2,"next_project":1,
            "projects":[{"id":"p1","name":"proj","path":"C:/proj"}],
            "sessions":[
              {"id":"s1","project_id":"p1","project_name":"proj","started_at":1,"model":"opus","permission_mode":"plan",
               "state":{"status":"idle","last_activity_at":2,"last_message":"hi","claude_session_id":null,
                        "inbox":[],"next_item":0,"unknown_count":0}},
              {"id":"s2","project_id":"p1","project_name":"proj","started_at":3,
               "state":{"status":"ended","last_activity_at":4,"last_message":null,"claude_session_id":null,
                        "inbox":[],"next_item":0,"unknown_count":0}}]}"#;
        fs::write(&f, old).unwrap();
        let snap = load(&f).unwrap().unwrap();
        assert_eq!(snap.schema, 2);
        assert!(snap.tasks.is_empty() && snap.next_task == 0);
        assert!(snap.sessions.iter().all(|s| s.task_id.is_none()));
        save(&f, &snap).unwrap();
        let written: serde_json::Value = serde_json::from_slice(&fs::read(&f).unwrap()).unwrap();
        assert_eq!(written["schema"], 2);
        assert_eq!(written["tasks"], serde_json::json!([]));
        let again = load(&f).unwrap().unwrap();
        assert_eq!(again, snap);
        assert_eq!(again.sessions.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["s1", "s2"]);
        assert_eq!(again.sessions[0].model.as_deref(), Some("opus"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn schema1_file_without_settings_loads() {
        let d = dir("nosettings");
        let f = d.join("state.json");
        let old = br#"{"schema":1,"revision":4,"next_session":1,"next_project":1,
            "projects":[],
            "sessions":[{"id":"s1","project_id":"p1","project_name":"proj","started_at":1,
              "state":{"status":"idle","last_activity_at":2,"last_message":null,"claude_session_id":null,
                       "inbox":[],"next_item":0,"unknown_count":0}}]}"#;
        fs::write(&f, old).unwrap();
        let snap = load(&f).unwrap().unwrap();
        assert_eq!((snap.sessions[0].model.as_deref(), snap.sessions[0].permission_mode.as_deref()), (None, None));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn atomic_save_roundtrip() {
        let d = dir("roundtrip");
        let f = d.join("sub").join("state.json");
        assert_eq!(load(&f), Ok(None), "absent file is a fresh start");
        let s = snap(3);
        save(&f, &s).unwrap();
        assert_eq!(load(&f), Ok(Some(s)));
        assert!(!f.with_extension("json.tmp").exists(), "temp file is renamed away");
        save(&f, &snap(4)).unwrap();
        assert_eq!(load(&f).unwrap().unwrap().revision, 4, "second save replaces the first");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn older_revision_never_overwrites_newer() {
        let d = dir("revision");
        let f = d.join("state.json");
        let w = Writer::new(f.clone());
        w.save(snap(5));
        w.flush();
        w.save(snap(3));
        w.flush();
        assert_eq!(load(&f).unwrap().unwrap().revision, 5);
        w.save(snap(4));
        w.save(snap(7));
        w.save(snap(6));
        w.flush();
        assert_eq!(load(&f).unwrap().unwrap().revision, 7, "queued snapshots coalesce to the newest");
        let _ = fs::remove_dir_all(&d);
    }
}

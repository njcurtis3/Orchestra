//! The boot sequence both hosts share: open the state file, pick transcript and worktree
//! storage to match, build the session manager, restore what was saved and start the queue.
//! The caller supplies the three things that differ: how events leave, how a child starts and
//! how a notice is delivered.

use std::path::Path;
use std::sync::Arc;

use crate::session::store::{self, StorageStatus};
use crate::session::transcript::TranscriptStore;
use crate::session::{Emit, Notify, Sessions, Spawner};

/// Open `data_dir` (`state.json`, `transcripts/`, `worktrees/` inside it) and return the
/// running session manager with the storage status. Saving stays off for the run when the
/// file cannot be loaded, or when there is no `data_dir`; see `store::open`. Restore spawns
/// nothing: queued tasks start in the background, and cleanups left pending get one more try.
pub fn open_host(
    data_dir: Option<&Path>,
    emit: Emit,
    spawner: Spawner,
    notify: Notify,
) -> (Arc<Sessions>, StorageStatus) {
    let (snapshot, status, writer) = match data_dir {
        Some(dir) => store::open(&dir.join("state.json")),
        None => (None, StorageStatus::unreadable(), None),
    };
    // Transcripts follow the state file: with saving off they stay in memory.
    let transcripts = match (data_dir, &writer) {
        (Some(dir), Some(_)) => TranscriptStore::disk(dir.join("transcripts")),
        _ => TranscriptStore::memory(),
    };
    // Task worktrees live under the data dir, nowhere near the projects.
    let worktree_base = match data_dir {
        Some(dir) => dir.join("worktrees"),
        None => std::env::temp_dir().join("orchestra-worktrees"),
    };
    let sessions = Sessions::with_writer(emit, spawner, notify, writer, transcripts, worktree_base);
    if let Some(snapshot) = snapshot {
        sessions.restore(snapshot);
    }
    sessions.pump_in_background();
    sessions.retry_pending_in_background();
    (sessions, status)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(dir: Option<&Path>) -> (Arc<Sessions>, StorageStatus) {
        open_host(
            dir,
            Box::new(|_| {}),
            Box::new(|_, _, _| Err("no children in this test".to_string())),
            Box::new(|_, _| {}),
        )
    }

    #[test]
    fn no_data_dir_runs_unsaved() {
        let (sessions, status) = open(None);
        assert!(sessions.list_projects().is_empty());
        assert_eq!(status, StorageStatus::unreadable());
    }

    #[test]
    fn project_survives_a_reopen() {
        let dir = std::env::temp_dir().join(format!("orchestra-host-{}", std::process::id()));
        let project = dir.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let (sessions, _) = open(Some(&dir));
        sessions.add_project(&project).unwrap();
        drop(sessions);
        // The writer thread flushes when the manager drops its last handle.
        let mut found = false;
        for _ in 0..50 {
            let (again, _) = open(Some(&dir));
            if again.list_projects().len() == 1 {
                found = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(found);
    }
}

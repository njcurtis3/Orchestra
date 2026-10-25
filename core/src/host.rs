//! The boot sequence both hosts share: open the state file, pick transcript and worktree
//! storage to match, build the session manager, restore what was saved and start the queue.
//! The caller supplies the three things that differ: how events leave, how a child starts and
//! how a notice is delivered.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::session::store::{self, StorageStatus};
use crate::session::transcript::TranscriptStore;
use crate::session::{Emit, Notify, Sessions, Spawner};

/// Open `data_dir` (`state.json`, `transcripts/`, `worktrees/` inside it) and return the
/// running session manager with the storage status. Saving stays off for the run when the
/// file cannot be loaded, or when there is no `data_dir`; see `store::open`. Restore spawns
/// nothing: queued tasks start in the background, and cleanups left pending get one more try.
///
/// `allowed` is for a host whose project set comes from a config: `Some` takes every restored
/// project whose folder is not listed out of service (`Sessions::retain_projects`) before any
/// background thread starts, so a queued task or pending cleanup there cannot run in the gap.
/// The desktop passes `None` and nothing changes.
pub fn open_host(
    data_dir: Option<&Path>,
    emit: Emit,
    spawner: Spawner,
    notify: Notify,
    allowed: Option<&[PathBuf]>,
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
    if let Some(allowed) = allowed {
        sessions.retain_projects(allowed);
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
            None,
        )
    }

    /// A saved state with project p1 at `project` and one queued task on it.
    fn save_queued_task(dir: &Path, project: &Path) {
        use crate::projects::StoredProject;
        use crate::session::store::{Snapshot, StoredTask};
        std::fs::create_dir_all(dir).unwrap();
        let task = StoredTask {
            id: "t1".into(),
            project_id: "p1".into(),
            project_name: "proj".into(),
            prompt: "work".into(),
            model: None,
            permission_mode: None,
            created_at: 1,
            status: "queued".into(),
            session_id: None,
            worktree: None,
            branch: None,
            base_branch: "main".into(),
            base_commit: "0".repeat(40),
            note: None,
            last_turn_error: false,
            session_over: false,
        };
        let snapshot = Snapshot {
            schema: store::SCHEMA,
            revision: 1,
            next_session: 0,
            next_project: 1,
            projects: vec![StoredProject { id: "p1".into(), name: "proj".into(), path: project.to_path_buf() }],
            sessions: vec![],
            next_task: 1,
            tasks: vec![task],
        };
        store::save(&dir.join("state.json"), &snapshot).unwrap();
    }

    fn boot(dir: &Path, allowed: Option<&[PathBuf]>) -> Arc<Sessions> {
        open_host(
            Some(dir),
            Box::new(|_| {}),
            Box::new(|_, _, _| Err("no children in this test".to_string())),
            Box::new(|_, _| {}),
            allowed,
        )
        .0
    }

    #[test]
    fn a_queued_task_on_a_disallowed_project_never_starts_at_boot() {
        use crate::session::task::TaskStatus;
        let root = std::env::temp_dir().join(format!("orchestra-host-allow-{}", std::process::id()));
        let project = root.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let state = root.join("state");
        save_queued_task(&state, &project);
        let status = |s: &Sessions| s.list_tasks().remove(0).status;
        // Told to allow nothing: the task stays queued however long the pump has had.
        let held = boot(&state, Some(&[]));
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert_eq!(status(&held), TaskStatus::Queued);
        assert!(held.list_projects().is_empty());
        drop(held);
        // The same state with no limit: the pump takes it (and fails it, as proj is no repo).
        let open = boot(&state, None);
        let mut moved = false;
        for _ in 0..100 {
            if status(&open) != TaskStatus::Queued {
                moved = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = std::fs::remove_dir_all(&root);
        assert!(moved, "without `allowed` the queued task is picked up as before");
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

//! Projects the user picked. The absolute path is held here, in Rust, and never leaves:
//! the frontend only sees `Project { id, name }`. The one other place it is written is the
//! Rust-owned state file (`session::store`), which the frontend cannot read.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
}

pub struct ProjectEntry {
    pub project: Project,
    pub path: PathBuf,
    /// The folder was gone when the state was loaded. It stays in the store, hidden and
    /// unusable, until the user picks it again.
    missing: bool,
}

/// A project as written to the state file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredProject {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
}

#[derive(Default)]
pub struct Projects {
    entries: Vec<ProjectEntry>,
    /// Last number handed out for a `pN` id; persisted so ids never repeat.
    next: u64,
}

impl Projects {
    /// Register a folder chosen through the native picker. Re-picking the same folder
    /// returns the existing project.
    pub fn add(&mut self, path: &Path) -> Result<Project, String> {
        if !path.is_dir() {
            return Err("not a folder".into());
        }
        if let Some(e) = self.entries.iter_mut().find(|e| e.path == path) {
            e.missing = false;
            return Ok(e.project.clone());
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "project".to_string());
        self.next += 1;
        let project = Project { id: format!("p{}", self.next), name };
        self.entries.push(ProjectEntry { project: project.clone(), path: path.to_path_buf(), missing: false });
        Ok(project)
    }

    /// Hide every project whose folder is not in `allowed`, as if its folder were gone: it
    /// leaves `list` and `get`, so nothing new can start there, and stays in the store with
    /// its id, so adding the folder again brings it back. Nothing is deleted. Returns how
    /// many were newly hidden. For a host whose project set comes from a config file.
    pub fn retain(&mut self, allowed: &[PathBuf]) -> usize {
        let mut hidden = 0;
        for e in self.entries.iter_mut().filter(|e| !e.missing && !allowed.contains(&e.path)) {
            e.missing = true;
            hidden += 1;
        }
        hidden
    }

    /// Usable projects only.
    pub fn list(&self) -> Vec<Project> {
        self.entries.iter().filter(|e| !e.missing).map(|e| e.project.clone()).collect()
    }

    /// A usable project; a missing folder is `None`.
    pub fn get(&self, id: &str) -> Option<&ProjectEntry> {
        self.entries.iter().find(|e| e.project.id == id && !e.missing)
    }

    pub fn to_stored(&self) -> Vec<StoredProject> {
        self.entries
            .iter()
            .map(|e| StoredProject { id: e.project.id.clone(), name: e.project.name.clone(), path: e.path.clone() })
            .collect()
    }

    pub fn counter(&self) -> u64 {
        self.next
    }

    /// Rebuild from the state file. Each folder is checked with `is_dir` now.
    pub fn restore(stored: Vec<StoredProject>, next: u64) -> Self {
        let mut next = next;
        let entries = stored
            .into_iter()
            .map(|p| {
                if let Some(n) = p.id.strip_prefix('p').and_then(|n| n.parse::<u64>().ok()) {
                    next = next.max(n);
                }
                let missing = !p.path.is_dir();
                ProjectEntry { project: Project { id: p.id, name: p.name }, path: p.path, missing }
            })
            .collect();
        Self { entries, next }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_dedupes_and_exposes_only_basename() {
        let mut p = Projects::default();
        let dir = std::env::temp_dir();
        let a = p.add(&dir).unwrap();
        let b = p.add(&dir).unwrap();
        assert_eq!(a.id, b.id);
        assert!(!a.name.contains(['/', '\\']));
        assert_eq!(p.list().len(), 1);
        assert!(p.get(&a.id).is_some());
        assert!(p.get("p99").is_none());
    }

    #[test]
    fn missing_dir_hidden_after_restore() {
        let dir = std::env::temp_dir();
        let gone = dir.join(format!("orchestra-gone-{}", std::process::id()));
        let stored = vec![
            StoredProject { id: "p1".into(), name: "here".into(), path: dir.clone() },
            StoredProject { id: "p2".into(), name: "gone".into(), path: gone.clone() },
        ];
        let mut p = Projects::restore(stored.clone(), 2);
        assert_eq!(p.list().iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), ["p1"]);
        assert!(p.get("p1").is_some() && p.get("p2").is_none(), "unusable by start/resume");
        assert_eq!(p.to_stored(), stored, "the missing project stays in the store");
        assert_eq!(p.add(&dir).unwrap().id, "p1", "re-picking finds the existing entry");
        assert_eq!(p.counter(), 2);
        std::fs::create_dir_all(&gone).unwrap();
        assert_eq!(p.add(&gone).unwrap().id, "p2", "a folder that came back is usable again");
        assert!(p.get("p2").is_some());
        let _ = std::fs::remove_dir_all(&gone);
    }

    #[test]
    fn retain_hides_the_rest_and_add_brings_one_back() {
        let dir = std::env::temp_dir();
        let other = dir.join(format!("orchestra-retain-{}", std::process::id()));
        std::fs::create_dir_all(&other).unwrap();
        let mut p = Projects::default();
        let a = p.add(&dir).unwrap();
        let b = p.add(&other).unwrap();
        assert_eq!(p.retain(&[dir.clone()]), 1);
        assert_eq!(p.list(), vec![a.clone()]);
        assert!(p.get(&b.id).is_none(), "a hidden project cannot start work");
        assert_eq!(p.to_stored().len(), 2, "nothing is deleted");
        assert_eq!(p.retain(&[dir.clone()]), 0, "already hidden");
        assert_eq!(p.retain(&[]), 1);
        assert!(p.list().is_empty());
        assert_eq!(p.add(&other).unwrap().id, b.id, "the same id comes back");
        assert!(p.get(&b.id).is_some());
        let _ = std::fs::remove_dir_all(&other);
    }

    #[test]
    fn rejects_non_directory() {
        let mut p = Projects::default();
        assert!(p.add(Path::new("definitely/not/a/dir")).is_err());
    }
}

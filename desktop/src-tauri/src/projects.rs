//! Projects the user picked. The absolute path is held here, in Rust, and never leaves:
//! the frontend only sees `Project { id, name }`.

use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Project {
    pub id: String,
    pub name: String,
}

pub struct ProjectEntry {
    pub project: Project,
    pub path: PathBuf,
}

#[derive(Default)]
pub struct Projects {
    entries: Vec<ProjectEntry>,
}

impl Projects {
    /// Register a folder chosen through the native picker. Re-picking the same folder
    /// returns the existing project.
    pub fn add(&mut self, path: &Path) -> Result<Project, String> {
        if !path.is_dir() {
            return Err("not a folder".into());
        }
        if let Some(e) = self.entries.iter().find(|e| e.path == path) {
            return Ok(e.project.clone());
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "project".to_string());
        let project = Project { id: format!("p{}", self.entries.len() + 1), name };
        self.entries.push(ProjectEntry { project: project.clone(), path: path.to_path_buf() });
        Ok(project)
    }

    pub fn list(&self) -> Vec<Project> {
        self.entries.iter().map(|e| e.project.clone()).collect()
    }

    pub fn get(&self, id: &str) -> Option<&ProjectEntry> {
        self.entries.iter().find(|e| e.project.id == id)
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
    fn rejects_non_directory() {
        let mut p = Projects::default();
        assert!(p.add(Path::new("definitely/not/a/dir")).is_err());
    }
}

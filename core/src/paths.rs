//! Where a headless host keeps its data, by the XDG base directory rules. Nothing calls
//! this yet: the desktop keeps the OS app data dir. The environment is a parameter so the
//! rules can be tested without touching the real one.

use std::ffi::OsString;
use std::path::PathBuf;

const APP: &str = "orchestra";

/// `$XDG_STATE_HOME/orchestra`, else `$HOME/.local/state/orchestra`, else `None`. A value
/// that is empty or not an absolute path is ignored, as the spec says.
pub fn state_dir_from(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let absolute = |name: &str| env(name).map(PathBuf::from).filter(|p| p.is_absolute());
    if let Some(xdg) = absolute("XDG_STATE_HOME") {
        return Some(xdg.join(APP));
    }
    absolute("HOME").map(|home| home.join(".local").join("state").join(APP))
}

/// `state_dir_from` over the real environment.
pub fn state_dir() -> Option<PathBuf> {
    state_dir_from(|k| std::env::var_os(k))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// An absolute path on the host OS.
    fn abs(rest: &str) -> String {
        std::env::temp_dir().join(rest).to_string_lossy().into_owned()
    }

    fn dir(vars: &[(&str, String)]) -> Option<PathBuf> {
        let map: HashMap<String, String> = vars.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        state_dir_from(|k| map.get(k).map(OsString::from))
    }

    #[test]
    fn xdg_state_home_wins() {
        let got = dir(&[("XDG_STATE_HOME", abs("state")), ("HOME", abs("home"))]);
        assert_eq!(got, Some(PathBuf::from(abs("state")).join("orchestra")));
    }

    #[test]
    fn empty_or_relative_xdg_state_home_is_ignored() {
        let want = Some(PathBuf::from(abs("home")).join(".local").join("state").join("orchestra"));
        assert_eq!(dir(&[("XDG_STATE_HOME", String::new()), ("HOME", abs("home"))]), want);
        assert_eq!(dir(&[("XDG_STATE_HOME", "rel/state".into()), ("HOME", abs("home"))]), want);
    }

    #[test]
    fn falls_back_to_home_local_state() {
        let got = dir(&[("HOME", abs("home"))]);
        assert_eq!(got, Some(PathBuf::from(abs("home")).join(".local").join("state").join("orchestra")));
    }

    #[test]
    fn none_without_xdg_or_home() {
        assert_eq!(dir(&[]), None);
        assert_eq!(dir(&[("HOME", String::new())]), None);
        assert_eq!(dir(&[("HOME", "relative".into())]), None);
    }
}

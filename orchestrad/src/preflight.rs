//! Checks made before the host starts: the `claude` program and its sign-in.
//!
//! Pure over an environment lookup and a file probe. It only learns whether a credential
//! exists, never what it is, so nothing here can leak one into a log or a status line.

use std::path::{Path, PathBuf};

/// Where the sign-in was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignIn {
    ApiKey,
    OAuthToken,
    CredentialsFile,
}

impl SignIn {
    pub fn describe(self) -> &'static str {
        match self {
            SignIn::ApiKey => "ANTHROPIC_API_KEY is set",
            SignIn::OAuthToken => "CLAUDE_CODE_OAUTH_TOKEN is set",
            SignIn::CredentialsFile => "a login is saved under $HOME/.claude",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preflight {
    pub claude_on_path: bool,
    pub sign_in: Option<SignIn>,
}

/// Run the checks. `env` returns a variable's value; `is_file` says whether a path is a file.
pub fn check(env: impl Fn(&str) -> Option<String>, is_file: impl Fn(&Path) -> bool) -> Preflight {
    // An empty variable is unset: `KEY=` in an env file is a placeholder, not a credential.
    let set = |name: &str| env(name).is_some_and(|v| !v.trim().is_empty());
    let claude_on_path = env("PATH")
        .map(|path| path.split(':').filter(|d| d.starts_with('/')).any(|d| is_file(&Path::new(d).join("claude"))))
        .unwrap_or(false);
    let sign_in = if set("ANTHROPIC_API_KEY") {
        Some(SignIn::ApiKey)
    } else if set("CLAUDE_CODE_OAUTH_TOKEN") {
        Some(SignIn::OAuthToken)
    } else {
        env("HOME")
            .filter(|h| h.starts_with('/'))
            .map(|h| PathBuf::from(h).join(".claude").join(".credentials.json"))
            .filter(|p| is_file(p))
            .map(|_| SignIn::CredentialsFile)
    };
    Preflight { claude_on_path, sign_in }
}

/// `check` over the real environment and file system.
pub fn check_system() -> Preflight {
    check(|k| std::env::var(k).ok(), |p| p.is_file() && (p.file_name() != Some(std::ffi::OsStr::new("claude")) || executable(p)))
}

#[cfg(unix)]
fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn executable(_: &Path) -> bool {
    true
}

impl Preflight {
    pub fn ok(&self) -> bool {
        self.claude_on_path && self.sign_in.is_some()
    }

    /// One line per problem, each saying what to do. Empty when `ok`.
    pub fn problems(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if !self.claude_on_path {
            out.push("`claude` was not found on PATH; install Claude Code where this user can run it");
        }
        if self.sign_in.is_none() {
            out.push(
                "claude has no sign-in: run `claude setup-token` and put CLAUDE_CODE_OAUTH_TOKEN (or ANTHROPIC_API_KEY) \
                 in /etc/orchestrad/env, or sign in as this user so $HOME/.claude/.credentials.json exists",
            );
        }
        out
    }

    /// The status line(s) for this check.
    pub fn summary(&self) -> String {
        if self.ok() {
            let how = self.sign_in.map(SignIn::describe).unwrap_or_default();
            return format!("ok (claude on PATH; {how})");
        }
        self.problems().join("\n  ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| map.get(k).cloned()
    }

    fn files(list: &'static [&'static str]) -> impl Fn(&Path) -> bool {
        move |p| list.iter().any(|f| Path::new(f) == p)
    }

    #[test]
    fn token_and_claude_pass() {
        let p = check(
            env_of(&[("PATH", "/usr/bin:/usr/local/bin"), ("CLAUDE_CODE_OAUTH_TOKEN", "secret-value")]),
            files(&["/usr/local/bin/claude"]),
        );
        assert!(p.ok());
        assert_eq!(p.sign_in, Some(SignIn::OAuthToken));
        assert!(p.problems().is_empty());
    }

    #[test]
    fn api_key_wins_and_empty_counts_as_unset() {
        let p = check(
            env_of(&[("PATH", "/bin"), ("ANTHROPIC_API_KEY", "k"), ("CLAUDE_CODE_OAUTH_TOKEN", "")]),
            files(&["/bin/claude"]),
        );
        assert_eq!(p.sign_in, Some(SignIn::ApiKey));
        let p = check(env_of(&[("PATH", "/bin"), ("ANTHROPIC_API_KEY", "  ")]), files(&["/bin/claude"]));
        assert_eq!(p.sign_in, None);
    }

    #[test]
    fn credentials_file_counts() {
        let p = check(
            env_of(&[("PATH", "/bin"), ("HOME", "/var/lib/orchestra")]),
            files(&["/bin/claude", "/var/lib/orchestra/.claude/.credentials.json"]),
        );
        assert_eq!(p.sign_in, Some(SignIn::CredentialsFile));
    }

    #[test]
    fn missing_everything_names_the_fix() {
        let p = check(env_of(&[("PATH", "/bin"), ("HOME", "relative")]), files(&[]));
        assert!(!p.ok() && !p.claude_on_path && p.sign_in.is_none());
        let text = p.summary();
        assert!(text.contains("claude setup-token") && text.contains("/etc/orchestrad/env"), "{text}");
    }

    #[test]
    fn relative_path_entries_are_ignored() {
        let p = check(env_of(&[("PATH", "bin:.")]), files(&["bin/claude"]));
        assert!(!p.claude_on_path);
    }

    #[test]
    fn the_summary_never_holds_a_value() {
        let p = check(env_of(&[("PATH", "/bin"), ("ANTHROPIC_API_KEY", "sk-very-secret")]), files(&["/bin/claude"]));
        assert!(!format!("{p:?} {}", p.summary()).contains("sk-very-secret"));
    }
}

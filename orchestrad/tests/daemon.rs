//! The built binary, run for real. Every test here is Linux only, so on Windows this file is
//! empty and the tests run in CI (test.yml, job `core` on ubuntu-24.04 and ubuntu-24.04-arm).
#![cfg(target_os = "linux")]

use std::io::Read;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use orchestrad::control::{Reply, Request, request};

const BIN: &str = env!("CARGO_BIN_EXE_orchestrad");

fn geteuid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orchestrad-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A port nobody listens on right now (>= 1024, as the config requires).
fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

/// A running `orchestrad run`, killed on drop.
struct Daemon {
    child: Child,
    dir: PathBuf,
    socket: PathBuf,
    /// Remove `dir` on drop; off when several runs share one.
    cleanup: bool,
}

impl Daemon {
    fn stderr(&self) -> String {
        std::fs::read_to_string(self.dir.join("stderr.log")).unwrap_or_default()
    }

    fn pid(&self) -> i32 {
        self.child.id() as i32
    }

    fn cli(&self, args: &[&str]) -> std::process::Output {
        cli(&self.socket, args)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if self.cleanup {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

fn cli(socket: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .arg("--socket")
        .arg(socket)
        .stdin(Stdio::null())
        .env_clear()
        .output()
        .unwrap()
}

/// Start the daemon with a config body (after `schema = 1`) and wait until `status` answers.
fn start(tag: &str, config_body: &str) -> Daemon {
    spawn_in(temp_dir(tag), config_body, true)
}

/// `start` in an existing directory, so a second run sees the first run's state.
fn spawn_in(dir: PathBuf, config_body: &str, cleanup: bool) -> Daemon {
    std::fs::write(dir.join("config.toml"), format!("schema = 1
{config_body}")).unwrap();
    let run_dir = dir.join("run");
    let stderr = std::fs::File::create(dir.join("stderr.log")).unwrap();
    let child = Command::new(BIN)
        .args(["run", "--config"])
        .arg(dir.join("config.toml"))
        .arg("--state-dir")
        .arg(dir.join("state"))
        .env_clear()
        // No claude and no credentials here, so the preflight has something to report.
        .env("PATH", "/nonexistent")
        .env("HOME", dir.join("home"))
        .env("RUNTIME_DIRECTORY", &run_dir)
        .env("JOURNAL_STREAM", "1:1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .unwrap();
    let daemon = Daemon { child, socket: run_dir.join("control.sock"), dir, cleanup };
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if daemon.cli(&["status"]).status.success() {
            return daemon;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the daemon never answered status; its stderr:
{}", daemon.stderr());
}

fn text(out: &[u8]) -> String {
    String::from_utf8_lossy(out).into_owned()
}

fn link_config(port: u16) -> String {
    format!("[link]\nport = {port}\nbinds = [\"127.0.0.1\"]\n")
}

macro_rules! skip_as_root {
    () => {
        if geteuid() == 0 {
            eprintln!("skipped: orchestrad refuses to run as root");
            return;
        }
    };
}

#[test]
fn status_lists_the_bound_address() {
    skip_as_root!();
    let port = free_port();
    let d = start("status", &link_config(port));
    let out = d.cli(&["status"]);
    assert!(out.status.success());
    let status = text(&out.stdout);
    assert!(status.contains(&format!("127.0.0.1:{port} (loopback)")), "{status}");
    assert!(status.contains("paired phones: 0") && status.contains("pairing: closed"), "{status}");
    // The link really listens there.
    TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_secs(2)).unwrap();
    // No claude and no sign-in in this environment: said in the log and in status.
    assert!(status.contains("claude setup-token"), "{status}");
    assert!(d.stderr().contains("<3>") && d.stderr().contains("claude setup-token"));
    assert!(d.stderr().contains(&format!("<5>link listening on 127.0.0.1:{port}")), "{}", d.stderr());
}

#[test]
fn no_binds_means_nothing_listens() {
    skip_as_root!();
    let port = free_port();
    let d = start("nobinds", "");
    let status = text(&d.cli(&["status"]).stdout);
    assert!(status.contains("listening on nothing"), "{status}");
    let refused = TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_secs(2));
    assert_eq!(refused.unwrap_err().kind(), std::io::ErrorKind::ConnectionRefused);
    // With the link off there is nothing to pair with.
    match request(&d.socket, &Request::OpenPairing, Some(Duration::from_secs(5))).unwrap() {
        Reply::Error { message } => assert!(message.contains("link"), "{message}"),
        other => panic!("expected an error, got {other:?}"),
    }
}

#[test]
fn the_pairing_secret_never_reaches_stderr() {
    skip_as_root!();
    let port = free_port();
    let d = start("secret", &link_config(port));
    let offer = match request(&d.socket, &Request::OpenPairing, Some(Duration::from_secs(5))).unwrap() {
        Reply::Offer { offer } => offer,
        other => panic!("expected an offer, got {other:?}"),
    };
    let json: serde_json::Value = serde_json::from_str(&offer).unwrap();
    let secret = json["secret"].as_str().unwrap().to_string();
    assert_eq!(secret.len(), 64, "a 32 byte secret in hex");
    assert!(json["addrs"].to_string().contains(&format!("127.0.0.1:{port}")));
    assert!(orchestrad::qr::render(&offer).is_ok());
    // `status` and `pair` never hold it either, and `pair` refuses without a terminal.
    assert!(text(&d.cli(&["status"]).stdout).contains("pairing: open"));
    let pair = d.cli(&["pair"]);
    assert_eq!(pair.status.code(), Some(1));
    assert!(text(&pair.stderr).contains("needs a terminal"));
    assert!(!text(&pair.stdout).contains(&secret) && !text(&pair.stderr).contains(&secret));
    let _ = request(&d.socket, &Request::ClosePairing, Some(Duration::from_secs(5)));
    std::thread::sleep(Duration::from_millis(300));
    let log = d.stderr();
    assert!(log.contains("pairing window opened"), "{log}");
    assert!(!log.contains(&secret), "the pairing secret reached the daemon's stderr");
    assert!(!log.contains(&offer));
}

#[test]
fn a_project_dropped_from_the_config_is_hidden_after_a_restart() {
    skip_as_root!();
    let dir = temp_dir("restart");
    let project = dir.join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let with = format!("[[projects]]
path = \"{}\"
", project.display());
    let mut first = spawn_in(dir.clone(), &with, false);
    assert!(text(&first.cli(&["status"]).stdout).contains("proj"));
    // SAFETY: signalling a child process this test started.
    assert_eq!(unsafe { libc::kill(first.pid(), libc::SIGTERM) }, 0);
    assert_eq!(first.child.wait().unwrap().code(), Some(0), "{}", first.stderr());
    // Same state dir, project no longer in the config: it was saved, so it must be hidden.
    let second = spawn_in(dir.clone(), "", false);
    let log = second.stderr();
    assert!(log.contains("1 saved project(s) are not in the config and are hidden"), "{log}");
    assert!(text(&second.cli(&["status"]).stdout).contains("projects: none configured"));
    // A third run with it listed again brings it back, with nothing hidden.
    drop(second);
    let third = spawn_in(dir.clone(), &with, false);
    assert!(!third.stderr().contains("are hidden"), "{}", third.stderr());
    assert!(third.stderr().contains("project proj ready"), "{}", third.stderr());
    drop(third);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn clients_lists_nobody_and_revoke_says_so() {
    skip_as_root!();
    let d = start("clients", &link_config(free_port()));
    let out = d.cli(&["clients"]);
    assert!(out.status.success() && text(&out.stdout).contains("no paired phones"));
    let out = d.cli(&["revoke", "nobody"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(!text(&out.stderr).is_empty());
}

#[test]
fn sigterm_exits_zero_within_five_seconds() {
    skip_as_root!();
    let mut d = start("sigterm", &link_config(free_port()));
    // SAFETY: signalling a child process this test started.
    assert_eq!(unsafe { libc::kill(d.pid(), libc::SIGTERM) }, 0);
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = d.child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "still running 5 s after SIGTERM:\n{}", d.stderr());
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(0), "{}", d.stderr());
    assert!(!d.socket.exists(), "the control socket was left behind");
}

#[test]
fn status_with_no_daemon_exits_3() {
    let dir = temp_dir("nodaemon");
    let out = cli(&dir.join("nothing.sock"), &["status"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(text(&out.stderr).contains("not running"));
    // A socket file nobody listens on is the same answer.
    let stale = dir.join("stale.sock");
    drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
    assert_eq!(cli(&stale, &["status"]).status.code(), Some(3));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_bad_config_stops_the_daemon_and_names_the_field() {
    skip_as_root!();
    let dir = temp_dir("badconfig");
    std::fs::write(dir.join("config.toml"), "schema = 1\n[link]\nbinds = [\"0.0.0.0\"]\n").unwrap();
    let mut child = Command::new(BIN)
        .args(["run", "--config"])
        .arg(dir.join("config.toml"))
        .arg("--state-dir")
        .arg(dir.join("state"))
        .env_clear()
        .env("RUNTIME_DIRECTORY", dir.join("run"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut err = String::new();
    child.stderr.take().unwrap().read_to_string(&mut err).unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(1));
    assert!(err.contains("link.binds[0]"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn usage_errors_exit_2() {
    let out = Command::new(BIN).arg("frobnicate").env_clear().output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

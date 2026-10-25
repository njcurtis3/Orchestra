//! The local control channel: pairing and status for the person at the host.
//!
//! One JSON object per line over a Unix socket. The codec and the path rules are pure and
//! run everywhere; the socket server and client are Linux only. The server knows nothing of
//! the daemon: it hands each request to a [`Handler`]. A reply that carries the pairing
//! secret goes only to the peer that asked, and nothing here logs a request or a reply.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Longest request or reply line, newline included.
pub const MAX_LINE: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Status,
    OpenPairing,
    /// Waits for the next client that proved the pairing secret.
    NextPairingRequest,
    Confirm { id: String },
    Reject { id: String },
    ClosePairing,
    ListClients,
    Revoke { id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub id: String,
    pub client_name: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    pub id: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub paired_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    Ok,
    Status { text: String },
    /// The offer JSON to draw as a QR code. It holds the pairing secret.
    Offer { offer: String },
    /// `None` when no client asked before the handler gave up waiting.
    Pending { pending: Option<Pending> },
    Clients { clients: Vec<ClientInfo> },
    Error { message: String },
}

/// What the server calls for each request. Implemented by the daemon, not here.
pub trait Handler: Send + Sync {
    fn handle(&self, request: Request) -> Reply;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    TooLong,
    Empty,
    Invalid,
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TooLong => "line is longer than 64 KiB",
            Self::Empty => "empty line",
            Self::Invalid => "not a known request",
        })
    }
}

impl std::error::Error for CodecError {}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, CodecError> {
    let mut line = serde_json::to_vec(value).map_err(|_| CodecError::Invalid)?;
    line.push(b'\n');
    if line.len() > MAX_LINE {
        return Err(CodecError::TooLong);
    }
    Ok(line)
}

fn decode<T: for<'a> Deserialize<'a>>(line: &[u8]) -> Result<T, CodecError> {
    if line.len() > MAX_LINE {
        return Err(CodecError::TooLong);
    }
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    if line.iter().all(u8::is_ascii_whitespace) {
        return Err(CodecError::Empty);
    }
    serde_json::from_slice(line).map_err(|_| CodecError::Invalid)
}

/// A request as one newline-terminated line.
pub fn encode_request(request: &Request) -> Result<Vec<u8>, CodecError> {
    encode(request)
}

pub fn decode_request(line: &[u8]) -> Result<Request, CodecError> {
    decode(line)
}

pub fn encode_reply(reply: &Reply) -> Result<Vec<u8>, CodecError> {
    encode(reply)
}

pub fn decode_reply(line: &[u8]) -> Result<Reply, CodecError> {
    decode(line)
}

/// Where the socket lives: `--socket`, else `$RUNTIME_DIRECTORY/control.sock` (systemd sets
/// it, and may list several directories, so the first is used), else
/// `$XDG_RUNTIME_DIR/orchestrad/control.sock`. Only absolute paths count; with none, `None`.
pub fn socket_path(
    flag: Option<&Path>,
    runtime_directory: Option<&str>,
    xdg_runtime_dir: Option<&str>,
) -> Option<PathBuf> {
    if let Some(path) = flag {
        return Some(path.to_path_buf());
    }
    let absolute = |s: &str| {
        let p = Path::new(s);
        s.starts_with('/').then(|| p.to_path_buf())
    };
    if let Some(dir) = runtime_directory.and_then(|v| v.split(':').next()).and_then(absolute) {
        return Some(dir.join("control.sock"));
    }
    xdg_runtime_dir
        .and_then(absolute)
        .map(|dir| dir.join("orchestrad").join("control.sock"))
}

/// A peer may use the channel only when it runs as the daemon's own user.
pub fn peer_allowed(peer_uid: u32, daemon_euid: u32) -> bool {
    peer_uid == daemon_euid
}

#[cfg(target_os = "linux")]
pub use unix::{Server, request, serve};

#[cfg(target_os = "linux")]
mod unix {
    use std::io::{self, BufRead, BufReader, Read, Write};
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;
    use std::time::Duration;

    use super::{
        CodecError, Handler, MAX_LINE, Reply, Request, decode_reply, decode_request, encode_reply,
        encode_request, peer_allowed,
    };

    /// A peer that sends nothing for this long is dropped.
    const READ_TIMEOUT: Duration = Duration::from_secs(30);
    const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

    /// A running control socket. Dropping it stops accepting and removes the socket file.
    pub struct Server {
        path: PathBuf,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl Server {
        pub fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            // Wake the blocked accept(); it sees the flag and returns.
            let _ = UnixStream::connect(&self.path);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn own_euid() -> u32 {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() }
    }

    fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
        let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: `cred` and `len` are valid for writes of the sizes passed, and the fd is
        // a live socket owned by `stream`.
        let rc = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(cred.uid)
    }

    /// Reads one line of at most `MAX_LINE` bytes. `Ok(None)` is a clean end of stream.
    fn read_line(reader: &mut impl BufRead) -> io::Result<Option<Result<Vec<u8>, CodecError>>> {
        let mut line = Vec::new();
        let n = reader.by_ref().take(MAX_LINE as u64 + 1).read_until(b'\n', &mut line)?;
        if n == 0 {
            return Ok(None);
        }
        if line.len() > MAX_LINE || (line.len() == MAX_LINE && line.last() != Some(&b'\n')) {
            return Ok(Some(Err(CodecError::TooLong)));
        }
        Ok(Some(Ok(line)))
    }

    fn write_reply(stream: &mut UnixStream, reply: &Reply) -> io::Result<()> {
        let line = encode_reply(reply)
            .or_else(|_| encode_reply(&Reply::Error { message: "reply is too large".into() }));
        stream.write_all(&line.map_err(|e| io::Error::other(e.to_string()))?)
    }

    fn serve_peer(stream: UnixStream, handler: &dyn Handler) -> io::Result<()> {
        if !peer_allowed(peer_uid(&stream)?, own_euid()) {
            return Ok(());
        }
        stream.set_read_timeout(Some(READ_TIMEOUT))?;
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        let mut writer = stream.try_clone()?;
        let mut reader = BufReader::new(stream);
        while let Some(read) = read_line(&mut reader)? {
            let reply = match read.and_then(|line| decode_request(&line)) {
                Ok(request) => handler.handle(request),
                Err(CodecError::TooLong) => {
                    // The rest of an over-long line is unframed: say so and hang up.
                    let message = CodecError::TooLong.to_string();
                    return write_reply(&mut writer, &Reply::Error { message });
                }
                Err(e) => Reply::Error { message: e.to_string() },
            };
            write_reply(&mut writer, &reply)?;
        }
        Ok(())
    }

    /// Binds `path` (mode 0600, parent created 0700), then serves on a background thread,
    /// one thread per accepted peer. A stale socket file is replaced; a live one is an error.
    pub fn serve(path: &Path, handler: Arc<dyn Handler>) -> io::Result<Server> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
            && !parent.exists()
        {
            std::fs::create_dir_all(parent)?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            if !meta.file_type().is_socket() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "control path exists and is not a socket",
                ));
            }
            if UnixStream::connect(path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "another orchestrad is listening on the control socket",
                ));
            }
            std::fs::remove_file(path)?;
        }
        // SAFETY: umask only swaps the process file-creation mask.
        let previous = unsafe { libc::umask(0o077) };
        let bound = UnixListener::bind(path);
        // SAFETY: as above, restoring the previous mask.
        unsafe { libc::umask(previous) };
        let listener = bound?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;

        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new().name("control".into()).spawn(move || {
            for stream in listener.incoming() {
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let handler = handler.clone();
                let _ = std::thread::Builder::new().name("control-peer".into()).spawn(move || {
                    let _ = serve_peer(stream, handler.as_ref());
                });
            }
        })?;
        Ok(Server { path: path.to_path_buf(), stop, thread: Some(thread) })
    }

    /// Sends one request and reads the reply. `timeout` bounds each read and write; pass
    /// `None` to wait as long as the handler does (`NextPairingRequest`).
    pub fn request(path: &Path, request: &Request, timeout: Option<Duration>) -> io::Result<Reply> {
        let mut stream = UnixStream::connect(path)?;
        stream.set_read_timeout(timeout)?;
        stream.set_write_timeout(timeout)?;
        stream.write_all(&encode_request(request).map_err(|e| io::Error::other(e.to_string()))?)?;
        let mut reader = BufReader::new(stream);
        let invalid = |e: CodecError| io::Error::new(io::ErrorKind::InvalidData, e.to_string());
        match read_line(&mut reader)? {
            Some(Ok(line)) => decode_reply(&line).map_err(invalid),
            Some(Err(e)) => Err(invalid(e)),
            None => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the daemon closed the connection",
            )),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::Mutex;

        struct Echo(Mutex<Vec<Request>>);

        impl Handler for Echo {
            fn handle(&self, request: Request) -> Reply {
                self.0.lock().unwrap().push(request.clone());
                match request {
                    Request::Status => Reply::Status { text: "ok".into() },
                    Request::Confirm { id } => Reply::Error { message: format!("no pending {id}") },
                    _ => Reply::Ok,
                }
            }
        }

        fn echo() -> Arc<Echo> {
            Arc::new(Echo(Mutex::new(Vec::new())))
        }

        fn temp_dir(tag: &str) -> PathBuf {
            let dir =
                std::env::temp_dir().join(format!("orchestrad-ctl-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn start(tag: &str) -> (PathBuf, Server, Arc<Echo>) {
            let dir = temp_dir(tag);
            let echo = echo();
            let server = serve(&dir.join("control.sock"), echo.clone()).unwrap();
            (dir, server, echo)
        }

        #[test]
        fn round_trip_over_a_real_socket() {
            let (dir, server, echo) = start("rt");
            let t = Some(Duration::from_secs(5));
            assert_eq!(
                request(server.path(), &Request::Status, t).unwrap(),
                Reply::Status { text: "ok".into() }
            );
            assert_eq!(
                request(server.path(), &Request::Confirm { id: "p1".into() }, t).unwrap(),
                Reply::Error { message: "no pending p1".into() }
            );
            assert_eq!(echo.0.lock().unwrap().len(), 2);
            drop(server);
            assert!(!dir.join("control.sock").exists());
            let _ = std::fs::remove_dir_all(dir);
        }

        #[test]
        fn socket_file_is_0600() {
            let (dir, server, _) = start("mode");
            let mode = std::fs::metadata(server.path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
            drop(server);
            let _ = std::fs::remove_dir_all(dir);
        }

        #[test]
        fn created_parent_is_0700() {
            let dir = temp_dir("parent");
            let server = serve(&dir.join("sub").join("control.sock"), echo()).unwrap();
            let mode = std::fs::metadata(dir.join("sub")).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
            drop(server);
            let _ = std::fs::remove_dir_all(dir);
        }

        #[test]
        fn peer_uid_of_a_local_connection_is_our_euid() {
            let (dir, server, _) = start("uid");
            let stream = UnixStream::connect(server.path()).unwrap();
            let uid = peer_uid(&stream).unwrap();
            assert_eq!(uid, own_euid());
            assert!(peer_allowed(uid, own_euid()));
            assert!(!peer_allowed(uid.wrapping_add(1), own_euid()));
            drop(stream);
            drop(server);
            let _ = std::fs::remove_dir_all(dir);
        }

        #[test]
        fn unknown_and_bad_lines_get_an_error_and_the_channel_survives() {
            let (dir, server, echo) = start("bad");
            let mut stream = UnixStream::connect(server.path()).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            stream
                .write_all(b"{\"op\":\"shutdown\"}\nnot json\n{\"op\":\"status\"}\n")
                .unwrap();
            let mut reader = BufReader::new(stream);
            for expect_error in [true, true, false] {
                let line = read_line(&mut reader).unwrap().unwrap().unwrap();
                let reply = decode_reply(&line).unwrap();
                assert_eq!(matches!(reply, Reply::Error { .. }), expect_error);
            }
            assert_eq!(echo.0.lock().unwrap().as_slice(), &[Request::Status]);
            drop(server);
            let _ = std::fs::remove_dir_all(dir);
        }

        #[test]
        fn an_oversize_line_is_refused_and_the_peer_dropped() {
            let (dir, server, echo) = start("big");
            let mut stream = UnixStream::connect(server.path()).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut big = vec![b'a'; MAX_LINE + 10];
            big.push(b'\n');
            let _ = stream.write_all(&big);
            let mut reader = BufReader::new(stream);
            let line = read_line(&mut reader).unwrap().unwrap().unwrap();
            assert!(matches!(decode_reply(&line).unwrap(), Reply::Error { .. }));
            assert!(echo.0.lock().unwrap().is_empty());
            drop(server);
            let _ = std::fs::remove_dir_all(dir);
        }

        #[test]
        fn a_live_socket_is_not_replaced_and_a_stale_one_is() {
            let (dir, server, _) = start("live");
            let err = serve(server.path(), echo()).err().unwrap();
            assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
            // A bound-and-abandoned socket file is stale.
            let stale = dir.join("stale.sock");
            drop(UnixListener::bind(&stale).unwrap());
            assert!(stale.exists());
            let second = serve(&stale, echo()).unwrap();
            drop(second);
            drop(server);
            let _ = std::fs::remove_dir_all(dir);
        }

        #[test]
        fn a_regular_file_at_the_path_is_left_alone() {
            let dir = temp_dir("file");
            let path = dir.join("control.sock");
            std::fs::write(&path, b"mine").unwrap();
            assert!(serve(&path, echo()).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), b"mine");
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_requests() -> Vec<Request> {
        vec![
            Request::Status,
            Request::OpenPairing,
            Request::NextPairingRequest,
            Request::Confirm { id: "c1".into() },
            Request::Reject { id: "c2".into() },
            Request::ClosePairing,
            Request::ListClients,
            Request::Revoke { id: "k1".into() },
        ]
    }

    #[test]
    fn requests_round_trip() {
        for request in all_requests() {
            let line = encode_request(&request).unwrap();
            assert_eq!(line.last(), Some(&b'\n'));
            assert_eq!(line.iter().filter(|b| **b == b'\n').count(), 1);
            assert_eq!(decode_request(&line).unwrap(), request);
        }
    }

    #[test]
    fn replies_round_trip() {
        let replies = [
            Reply::Ok,
            Reply::Status { text: "link on".into() },
            Reply::Offer { offer: "{\"v\":1}".into() },
            Reply::Pending { pending: None },
            Reply::Pending {
                pending: Some(Pending {
                    id: "p1".into(),
                    client_name: "Pixel".into(),
                    fingerprint: "ab cd".into(),
                }),
            },
            Reply::Clients {
                clients: vec![ClientInfo {
                    id: "c1".into(),
                    name: "Pixel".into(),
                    scopes: vec!["read".into()],
                    paired_at: 7,
                }],
            },
            Reply::Error { message: "nope".into() },
        ];
        for reply in replies {
            assert_eq!(decode_reply(&encode_reply(&reply).unwrap()).unwrap(), reply);
        }
    }

    #[test]
    fn wire_shape_is_pinned() {
        assert_eq!(encode_request(&Request::Status).unwrap(), b"{\"op\":\"status\"}\n");
        assert_eq!(
            encode_request(&Request::Revoke { id: "k".into() }).unwrap(),
            b"{\"op\":\"revoke\",\"id\":\"k\"}\n"
        );
    }

    #[test]
    fn oversize_line_is_refused() {
        let line = vec![b'a'; MAX_LINE + 1];
        assert_eq!(decode_request(&line), Err(CodecError::TooLong));
        let big = Reply::Offer { offer: "x".repeat(MAX_LINE) };
        assert_eq!(encode_reply(&big), Err(CodecError::TooLong));
    }

    #[test]
    fn unknown_or_malformed_requests_are_refused() {
        for bad in [
            &b"{\"op\":\"shutdown\"}\n"[..],
            b"{\"op\":\"confirm\"}\n",
            b"[]\n",
            b"nope\n",
        ] {
            assert_eq!(
                decode_request(bad),
                Err(CodecError::Invalid),
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
        assert_eq!(decode_request(b"\n"), Err(CodecError::Empty));
        assert_eq!(decode_request(b""), Err(CodecError::Empty));
    }

    #[test]
    fn socket_path_precedence() {
        let flag = Path::new("/tmp/x.sock");
        assert_eq!(
            socket_path(Some(flag), Some("/run/orchestrad"), Some("/run/user/1")),
            Some(flag.to_path_buf())
        );
        assert_eq!(
            socket_path(None, Some("/run/orchestrad:/other"), Some("/run/user/1")),
            Some(PathBuf::from("/run/orchestrad/control.sock"))
        );
        assert_eq!(
            socket_path(None, None, Some("/run/user/1")),
            Some(PathBuf::from("/run/user/1/orchestrad/control.sock"))
        );
        assert_eq!(socket_path(None, Some("relative"), Some("")), None);
        assert_eq!(socket_path(None, None, None), None);
    }

    #[test]
    fn only_our_own_uid_is_allowed() {
        assert!(peer_allowed(1000, 1000));
        assert!(!peer_allowed(0, 1000));
        assert!(!peer_allowed(1001, 1000));
    }
}

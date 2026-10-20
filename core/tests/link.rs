//! The link end to end: a real listener on `127.0.0.1` (port chosen by the OS), a Rust
//! client doing the real handshakes, and a fake process behind `Sessions`.

mod link_support;

use std::net::TcpStream;
use std::time::Duration;

use link_support::{
    Conn, INIT, Identity, PERM, Rig, call_of, expect_snapshot, hello, read_text, upgrade,
};
use orchestra_core::link::keys::{Psk, SecretKey};
use orchestra_core::link::{LinkCallbacks, LinkConfig, LinkHost, LinkLog};
use orchestra_core::session::store::StorageStatus;
use orchestra_core::wire::message::HandshakeReply;
use orchestra_core::wire::{
    Call, ClientMessage, HostEvent, HostMessage, MAX_MESSAGE_BYTES, Method, Scope, decode_handshake_reply, encode_host,
};
use serde_json::{Value, json};

fn error_code(r: Result<Value, orchestra_core::wire::message::ErrorBody>) -> String {
    r.unwrap_err().code
}

#[test]
fn pair_confirm_then_kk_reconnect() {
    let rig = Rig::new();
    let me = Identity::new();
    let (client, mut conn) = rig.pair(&me, "Pixel", None);
    assert_eq!(client.scopes, vec![Scope::Read, Scope::Answer]);
    // The welcome was read by `pair`; the snapshot follows it.
    let snap = expect_snapshot(&mut conn);
    assert_eq!(snap.projects.len(), 1);
    assert!(snap.storage.ok);
    drop(conn);

    // The same key reconnects over `/link` with no window and no secret.
    let mut again = Conn::link(rig.addr, &rig.host_key, &me, "Pixel").expect("kk handshake");
    match again.expect() {
        HostMessage::Welcome(w) => {
            assert_eq!(w.host_id, rig.host_key.host_id());
            assert_eq!(w.host_name, "Test host");
            assert_eq!(w.scopes, vec!["read", "answer"]);
            assert!(w.features.is_empty());
        }
        other => panic!("expected welcome, got {other:?}"),
    }
    expect_snapshot(&mut again);
    again.send(&ClientMessage::Ping);
    assert_eq!(again.expect(), HostMessage::Pong);
    assert_eq!(rig.host.list_clients().unwrap().len(), 1);
}

#[test]
fn pairing_reject_closes_silently() {
    let rig = Rig::new();
    let me = Identity::new();
    let offer = rig.host.open_pairing().unwrap();
    let mut conn = Conn::pair(rig.addr, &offer, &me, "Pixel").unwrap();
    let request = rig.pairing.recv_timeout(Duration::from_secs(5)).unwrap();
    rig.host.reject_pairing(&request.id).unwrap();
    conn.expect_closed();
    assert!(rig.host.list_clients().unwrap().is_empty());
    // Rejecting spent the window: the same offer cannot be tried again.
    assert!(Conn::pair(rig.addr, &offer, &Identity::new(), "Again").is_err());
    assert!(rig.host.reject_pairing(&request.id).is_err());
}

#[test]
fn pairing_window_expired_or_used_refused() {
    // Expired.
    let rig = Rig::with(|l| l.pairing_ttl = Duration::from_millis(150));
    let offer = rig.host.open_pairing().unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert!(Conn::pair(rig.addr, &offer, &Identity::new(), "Late").is_err());
    assert!(rig.pairing.try_recv().is_err());

    // Never opened.
    let rig = Rig::new();
    let stale = rig.host.open_pairing().unwrap();
    rig.host.close_pairing();
    assert!(Conn::pair(rig.addr, &stale, &Identity::new(), "Closed").is_err());

    // Used: a second device with the same offer is turned away.
    let offer = rig.host.open_pairing().unwrap();
    let mut first = Conn::pair(rig.addr, &offer, &Identity::new(), "First").unwrap();
    let request = rig.pairing.recv_timeout(Duration::from_secs(5)).unwrap();
    rig.host.confirm_pairing(&request.id, None).unwrap();
    assert!(matches!(first.expect(), HostMessage::Welcome(_)));
    assert!(Conn::pair(rig.addr, &offer, &Identity::new(), "Second").is_err());
    assert_eq!(rig.host.list_clients().unwrap().len(), 1);
}

#[test]
fn wrong_psk_closes_silently() {
    let rig = Rig::new();
    let offer = rig.host.open_pairing().unwrap();
    let wrong = Psk::generate().unwrap();
    let result = Conn::pair_with(rig.addr, &rig.host_key, &wrong, &Identity::new(), "Intruder");
    assert!(result.is_err(), "the client cannot even open the host's second message");
    std::thread::sleep(Duration::from_millis(200));
    assert!(rig.pairing.try_recv().is_err(), "the host never saw a request");
    assert!(rig.host.list_clients().unwrap().is_empty());
    // A wrong host key (a different host) fails the same way.
    let other_host = SecretKey::generate().unwrap().public();
    let psk = Psk::from_bytes(orchestra_core::link::keys::from_hex::<32>(&offer.secret).unwrap());
    assert!(Conn::pair_with(rig.addr, &other_host, &psk, &Identity::new(), "Intruder").is_err());
    assert!(rig.pairing.try_recv().is_err());
}

#[test]
fn unknown_key_closes_silently() {
    let rig = Rig::new();
    let (_, _paired) = rig.pair(&Identity::new(), "Known", None);
    let stranger = Identity::new();
    assert!(Conn::link_open(rig.addr, &rig.host_key, &stranger).is_err());
    // With nobody paired at all it is the same.
    let empty = Rig::new();
    assert!(Conn::link_open(empty.addr, &empty.host_key, &stranger).is_err());
}

#[test]
fn unsupported_version_error_then_close() {
    let rig = Rig::new();
    let mut ws = upgrade(rig.addr, "/link").unwrap();
    ws.send(tungstenite::Message::text(r#"{"proto":"orchestra","v":2}"#)).unwrap();
    let reply = read_text(&mut ws).unwrap();
    match decode_handshake_reply(&reply).unwrap() {
        HandshakeReply::Error(e) => {
            assert_eq!(e.code, "unsupported_version");
            assert_eq!(e.supported, Some(vec![1]));
        }
        other => panic!("expected an error, got {other:?}"),
    }
    assert!(ws.read().is_err(), "the host closes after the error");
    // Not our protocol at all: closed, and nothing was paired or logged.
    let mut ws = upgrade(rig.addr, "/link").unwrap();
    ws.send(tungstenite::Message::text("hello there")).unwrap();
    let _ = read_text(&mut ws);
    assert!(ws.read().is_err());
}

#[test]
fn only_pair_and_link_upgrade() {
    let rig = Rig::new();
    for path in ["/", "/pair/", "/link/x", "/ws", "/pair"] {
        // `/pair` has no open window here.
        assert!(upgrade(rig.addr, path).is_err(), "{path}");
    }
    assert!(upgrade(rig.addr, "/link").is_ok());
}

#[test]
fn hello_must_be_first() {
    let rig = Rig::new();
    let me = Identity::new();
    let (_, conn) = rig.pair(&me, "Pixel", None);
    drop(conn);
    let mut conn = Conn::link_open(rig.addr, &rig.host_key, &me).unwrap();
    conn.send(&ClientMessage::Ping);
    let seen = conn.drain_until_closed();
    assert_eq!(seen.len(), 1);
    assert!(matches!(&seen[0], HostMessage::Error(e) if e.code == "bad_request"));

    // A call before the hello gets nothing but a close, and does nothing.
    let mut conn = Conn::link_open(rig.addr, &rig.host_key, &me).unwrap();
    conn.call("c1", Call::ListProjects);
    assert!(!conn.drain_until_closed().iter().any(|m| matches!(m, HostMessage::Result { .. })));
}

#[test]
fn a_second_hello_is_a_bad_request_not_a_crash() {
    let rig = Rig::new();
    let (_, mut conn) = rig.pair(&Identity::new(), "Pixel", None);
    expect_snapshot(&mut conn);
    conn.send(&hello("again"));
    assert!(matches!(conn.expect(), HostMessage::Error(e) if e.code == "bad_request"));
    conn.send(&ClientMessage::Ping);
    assert_eq!(conn.expect(), HostMessage::Pong);
}

#[test]
fn snapshot_then_events_in_seq_order() {
    let rig = Rig::new();
    let id = rig.start_session("go");
    rig.feed(INIT);
    let (_, mut conn) = rig.pair(&Identity::new(), "Pixel", None);
    let snap = expect_snapshot(&mut conn);
    assert_eq!(snap.sessions.len(), 1);
    assert_eq!(snap.sessions[0].id, id);
    assert!(snap.seq > 0);

    rig.feed(PERM);
    let mut seqs = vec![];
    let mut names = vec![];
    while !names.contains(&"inbox-updated".to_string()) {
        match conn.expect() {
            HostMessage::Event { seq, event } => {
                seqs.push(seq);
                names.push(event.name().to_string());
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(seqs[0], snap.seq + 1, "the first event follows the snapshot with no gap");
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");
    assert!(names.contains(&"session-updated".to_string()));
}

#[test]
fn calls_round_trip() {
    let rig = Rig::new();
    let (_, mut conn) = rig.pair(&Identity::new(), "Pixel", Some(&[Scope::Read, Scope::Control]));
    expect_snapshot(&mut conn);
    conn.call("p", call_of(Method::ListProjects, json!({})));
    assert_eq!(conn.result("p").unwrap().as_array().unwrap().len(), 1);
    conn.call("s", call_of(Method::StorageStatus, json!({})));
    assert_eq!(conn.result("s").unwrap(), json!({"ok": true}));
    let start = call_of(Method::StartSession, json!({"project_id": rig.project, "prompt": "hello"}));
    conn.call("start", start);
    assert_eq!(conn.result("start").unwrap()["status"], json!("starting"));
    assert_eq!(rig.sessions.list_sessions().len(), 1);
    conn.call("bad", call_of(Method::StopSession, json!({"session_id": "nope"})));
    assert_eq!(error_code(conn.result("bad")), "failed");
}

#[test]
fn call_outside_scope_forbidden_and_logged_without_payload() {
    let rig = Rig::new();
    let (client, mut conn) = rig.pair(&Identity::new(), "Pixel", Some(&[Scope::Read]));
    expect_snapshot(&mut conn);
    let secret = "SECRET-PROMPT-7731";
    let start = call_of(Method::StartSession, json!({"project_id": rig.project, "prompt": secret}));
    conn.call("c1", start);
    assert_eq!(error_code(conn.result("c1")), "forbidden");
    assert!(rig.sessions.list_sessions().is_empty(), "nothing ran");

    let logs = rig.logged();
    assert!(logs.contains(&LinkLog::Forbidden { client_id: client.id.clone(), method: "start_session" }));
    let shown = format!("{logs:?}");
    assert!(!shown.contains(secret), "{shown}");

    // Read calls are still allowed.
    conn.call("c2", call_of(Method::ListSessions, json!({})));
    assert_eq!(conn.result("c2").unwrap(), json!([]));
}

#[test]
fn scope_changes_apply_to_a_live_connection() {
    let rig = Rig::new();
    let (client, mut conn) = rig.pair(&Identity::new(), "Pixel", Some(&[Scope::Read]));
    expect_snapshot(&mut conn);
    let stop = || call_of(Method::StopSession, json!({"session_id": "nope"}));
    conn.call("a", stop());
    assert_eq!(error_code(conn.result("a")), "forbidden");
    rig.host.set_scopes(&client.id, &[Scope::Read, Scope::Control]).unwrap();
    conn.call("b", stop());
    assert_eq!(error_code(conn.result("b")), "failed", "allowed now, then refused by the core");
    rig.host.set_scopes(&client.id, &[Scope::Read]).unwrap();
    conn.call("c", stop());
    assert_eq!(error_code(conn.result("c")), "forbidden");
}

#[test]
fn answer_resend_applies_once() {
    let rig = Rig::new();
    rig.start_session("go");
    rig.feed(INIT);
    rig.feed(PERM);
    let item = rig.sessions.list_inbox().into_iter().find(|i| !i.resolved).unwrap();
    let (_, mut conn) = rig.pair(&Identity::new(), "Pixel", None);
    expect_snapshot(&mut conn);
    let before = rig.fake.lines.lock().unwrap().len();
    let answer = || call_of(Method::Answer, json!({"kind": "permission", "item_id": item.id, "decision": "allow"}));

    conn.call("a1", answer());
    assert_eq!(conn.result("a1").unwrap(), Value::Null);
    assert_eq!(rig.fake.lines.lock().unwrap().len(), before + 1);
    // The same id again, as an offline queue would resend it: replayed, not applied.
    conn.call("a1", answer());
    assert_eq!(conn.result("a1").unwrap(), Value::Null);
    assert_eq!(rig.fake.lines.lock().unwrap().len(), before + 1);
    // A new id for the now resolved item says so.
    conn.call("a2", answer());
    assert_eq!(error_code(conn.result("a2")), "already_answered");
    assert_eq!(rig.fake.lines.lock().unwrap().len(), before + 1);
}

#[test]
fn answer_resend_after_reconnect_applies_once() {
    let rig = Rig::new();
    rig.start_session("go");
    rig.feed(INIT);
    rig.feed(PERM);
    let item = rig.sessions.list_inbox().into_iter().find(|i| !i.resolved).unwrap();
    let me = Identity::new();
    let (_, mut conn) = rig.pair(&me, "Pixel", None);
    expect_snapshot(&mut conn);
    let before = rig.fake.lines.lock().unwrap().len();
    let answer = || call_of(Method::Answer, json!({"kind": "permission", "item_id": item.id, "decision": "allow"}));
    conn.call("a1", answer());
    assert_eq!(conn.result("a1").unwrap(), Value::Null);
    drop(conn);

    let mut again = Conn::link(rig.addr, &rig.host_key, &me, "Pixel").unwrap();
    assert!(matches!(again.expect(), HostMessage::Welcome(_)));
    expect_snapshot(&mut again);
    again.call("a1", answer());
    assert_eq!(again.result("a1").unwrap(), Value::Null);
    assert_eq!(rig.fake.lines.lock().unwrap().len(), before + 1);
}

#[test]
fn answering_an_expired_item_over_the_link_is_failed_not_already_answered() {
    let rig = Rig::new();
    rig.start_session("go");
    rig.feed(INIT);
    rig.feed(PERM);
    let item = rig.sessions.list_inbox().into_iter().find(|i| !i.resolved).unwrap();
    let (_, mut conn) = rig.pair(&Identity::new(), "Pixel", None);
    expect_snapshot(&mut conn);
    // Closing Orchestra marks the open item resolved and expired, with no event.
    rig.sessions.shutdown();
    let answer = call_of(Method::Answer, json!({"kind": "permission", "item_id": item.id, "decision": "allow"}));
    conn.call("a1", answer);
    let err = conn.result("a1").unwrap_err();
    assert_eq!(err.code, "failed");
    assert!(err.message.contains("expired"), "{}", err.message);
}

#[test]
fn inbox_updated_carries_only_the_changed_sessions_items() {
    let rig = Rig::new();
    let a = rig.start_session("first");
    rig.feed(INIT);
    rig.feed(PERM);
    let b = rig.start_session("second");
    rig.feed(INIT);
    let (_, mut conn) = rig.pair(&Identity::new(), "Pixel", None);
    let snap = expect_snapshot(&mut conn);
    assert_eq!(snap.inbox.len(), 1);
    assert_eq!(snap.inbox[0].session_id, a);

    // `feed` targets the newest session, so the permission request belongs to B.
    rig.feed(PERM);
    let items = loop {
        match conn.expect() {
            HostMessage::Event { event: HostEvent::InboxUpdated(items), .. } => break items,
            HostMessage::Event { .. } => {}
            other => panic!("unexpected {other:?}"),
        }
    };
    assert!(!items.is_empty());
    assert!(items.iter().all(|i| i.session_id == b), "A's open item must not ride along: {items:?}");
}

#[test]
fn revoke_live_connection_gets_revoked() {
    let rig = Rig::new();
    let me = Identity::new();
    let (client, mut conn) = rig.pair(&me, "Pixel", None);
    expect_snapshot(&mut conn);
    rig.host.revoke(&client.id).unwrap();
    let seen = conn.drain_until_closed();
    assert!(matches!(seen.first(), Some(HostMessage::Error(e)) if e.code == "revoked"), "{seen:?}");
    assert!(rig.logged().contains(&LinkLog::Revoked { client_id: client.id.clone() }));
    // Its key is gone: the next handshake fails with no message.
    assert!(Conn::link_open(rig.addr, &rig.host_key, &me).is_err());
    assert!(rig.host.list_clients().unwrap().is_empty());
}

#[test]
fn a_newer_connection_from_the_same_client_replaces_the_old() {
    let rig = Rig::new();
    let me = Identity::new();
    let (_, mut first) = rig.pair(&me, "Pixel", None);
    expect_snapshot(&mut first);
    let mut second = Conn::link(rig.addr, &rig.host_key, &me, "Pixel").unwrap();
    assert!(matches!(second.expect(), HostMessage::Welcome(_)));
    first.drain_until_closed();
    expect_snapshot(&mut second);
    second.send(&ClientMessage::Ping);
    assert_eq!(second.expect(), HostMessage::Pong);
}

#[test]
fn large_message_chunked_round_trip() {
    let rig = Rig::new();
    // A snapshot well past one Noise message (65519 bytes of plaintext).
    for i in 0..400 {
        rig.start_session(&format!("session number {i} with a prompt of some length"));
    }
    let (_, mut conn) = rig.pair(&Identity::new(), "Pixel", Some(&[Scope::Read, Scope::Answer]));
    let snap = expect_snapshot(&mut conn);
    assert_eq!(snap.sessions.len(), 400);
    let size = encode_host(&HostMessage::Snapshot(Box::new(snap))).unwrap().len();
    assert!(size > 65_519, "{size} bytes should span more than one Noise message");

    // And the other way: a client message of about 130 kB.
    let answers: serde_json::Map<String, Value> =
        (0..32).map(|i| (format!("q{i}"), Value::String("x".repeat(4000)))).collect();
    let big = call_of(Method::Answer, json!({"kind": "question", "item_id": "nope", "answers": answers}));
    conn.call("big", big);
    assert_eq!(error_code(conn.result("big")), "already_answered", "it arrived whole and decoded");
}

#[test]
fn oversize_message_closes() {
    let rig = Rig::new();
    let me = Identity::new();
    let (_, conn) = rig.pair(&me, "Pixel", None);
    drop(conn);

    // Past the decoded-message cap, in valid encrypted chunks.
    let mut conn = Conn::link(rig.addr, &rig.host_key, &me, "Pixel").unwrap();
    assert!(matches!(conn.expect(), HostMessage::Welcome(_)));
    conn.send_plain(&vec![b'x'; MAX_MESSAGE_BYTES + 1]);
    assert!(!conn.drain_until_closed().iter().any(|m| matches!(m, HostMessage::Result { .. })));

    // Past the WebSocket message cap, before any decryption.
    let mut conn = Conn::link(rig.addr, &rig.host_key, &me, "Pixel").unwrap();
    assert!(matches!(conn.expect(), HostMessage::Welcome(_)));
    let _ = conn.ws.send(tungstenite::Message::binary(vec![0u8; 5 * 1024 * 1024]));
    let _ = conn.drain_until_closed();

    // Garbage that is not a frame.
    let mut conn = Conn::link(rig.addr, &rig.host_key, &me, "Pixel").unwrap();
    assert!(matches!(conn.expect(), HostMessage::Welcome(_)));
    conn.ws.send(tungstenite::Message::binary(vec![1u8, 2, 3])).unwrap();
    conn.drain_until_closed();
}

#[test]
fn handshake_rate_limited() {
    let rig = Rig::with(|l| l.handshakes_per_minute = 3);
    for _ in 0..3 {
        let _ = TcpStream::connect(rig.addr).unwrap();
    }
    std::thread::sleep(Duration::from_millis(300));
    // The fourth attempt from this address is dropped before any upgrade.
    assert!(upgrade(rig.addr, "/link").is_err());
    assert!(rig.logged().iter().any(|l| matches!(l, LinkLog::Refused { .. })));
}

#[test]
fn connection_cap_refuses_the_extra_connection() {
    let rig = Rig::with(|l| l.max_connections = 2);
    let _a = upgrade(rig.addr, "/link").unwrap();
    let _b = upgrade(rig.addr, "/link").unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert!(upgrade(rig.addr, "/link").is_err());
}

#[test]
fn idle_connection_is_closed() {
    let rig = Rig::with(|l| l.idle_timeout = Duration::from_millis(500));
    let (_, mut conn) = rig.pair(&Identity::new(), "Pixel", None);
    expect_snapshot(&mut conn);
    conn.drain_until_closed();
}

#[test]
fn stop_closes_listener_and_connections() {
    let rig = Rig::new();
    let me = Identity::new();
    let (_, mut conn) = rig.pair(&me, "Pixel", None);
    expect_snapshot(&mut conn);
    let waiting = upgrade(rig.addr, "/link").unwrap();
    rig.host.stop();
    conn.drain_until_closed();
    drop(waiting);
    assert!(TcpStream::connect(rig.addr).is_err(), "the listener is gone");
    assert!(!rig.host.is_running());
    assert!(rig.host.open_pairing().is_err());
}

#[test]
fn paired_clients_survive_a_restart() {
    let rig = Rig::new();
    let me = Identity::new();
    let (client, conn) = rig.pair(&me, "Pixel", Some(&[Scope::Read, Scope::Land]));
    drop(conn);
    rig.host.stop();
    let addr = rig.host.start().unwrap()[0];
    let mut conn = Conn::link(addr, &rig.host_key, &me, "Pixel").expect("the key and the client were kept");
    match conn.expect() {
        HostMessage::Welcome(w) => assert_eq!(w.scopes, vec!["read", "land"]),
        other => panic!("{other:?}"),
    }
    assert_eq!(rig.host.list_clients().unwrap()[0].id, client.id);
}

#[test]
fn empty_binds_listens_on_nothing() {
    let dir = std::env::temp_dir().join(format!("orchestra-linktest-empty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let sessions = orchestra_core::session::Sessions::with_writer(
        Box::new(|_| {}),
        Box::new(|_, _, _| Err("no process".to_string())),
        Box::new(|_, _| {}),
        None,
        orchestra_core::session::transcript::TranscriptStore::memory(),
        dir.join("worktrees"),
    );
    let config = LinkConfig::new("Off", dir.join("link"), StorageStatus::ok());
    assert!(config.binds.is_empty());
    let keys = Box::new(orchestra_core::link::keys::FileKeyProvider::new(&config.link_dir));
    let host = LinkHost::new(sessions, config, keys, LinkCallbacks::silent());
    assert_eq!(host.start().unwrap(), vec![]);
    assert!(host.local_addrs().is_empty());
    assert!(!host.is_running());
    assert!(!dir.join("link").exists(), "no key, no clients file, nothing created");
    assert!(host.open_pairing().is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pairing_offer_names_the_bound_address() {
    let rig = Rig::new();
    let offer = rig.host.open_pairing().unwrap();
    assert_eq!(offer.addrs, vec![rig.addr.to_string()]);
    assert_eq!(offer.host_id, rig.host_key.host_id());
    assert!(offer.addrs[0].starts_with("127.0.0.1:"));
}

#[test]
fn pairing_name_huge_name_reaches_the_host_clipped() {
    let rig = Rig::new();
    let me = Identity::new();
    let offer = rig.host.open_pairing().unwrap();
    // As long as the wire allows a string to be (MAX_TEXT_BYTES, 32 KiB); clip is also unit-tested at 4 MiB.
    let huge = "n".repeat(30_000);
    let _conn = Conn::pair(rig.addr, &offer, &me, &huge).unwrap();
    let request = rig.pairing.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(request.client_name.len(), 128);
    assert!(request.client_name.bytes().all(|b| b == b'n'));
    let client = rig.host.confirm_pairing(&request.id, None).unwrap();
    assert_eq!(client.name, request.client_name, "shown and stored names agree");
}

#[test]
fn pairing_name_control_and_bidi_characters_are_dropped() {
    let rig = Rig::new();
    let me = Identity::new();
    let offer = rig.host.open_pairing().unwrap();
    let _conn = Conn::pair(rig.addr, &offer, &me, " \u{202E}Pi\u{0}xel\r\n\u{2066}7\u{1b}[2J ").unwrap();
    let request = rig.pairing.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(request.client_name, "Pixel7[2J");
    let client = rig.host.confirm_pairing(&request.id, None).unwrap();
    assert_eq!(client.name, "Pixel7[2J");
}

#[test]
fn confirm_race_a_confirm_after_the_deadline_stores_nothing() {
    let rig = Rig::with(|l| l.confirm_timeout = Duration::from_millis(250));
    let offer = rig.host.open_pairing().unwrap();
    let mut conn = Conn::pair(rig.addr, &offer, &Identity::new(), "Late").unwrap();
    let request = rig.pairing.recv_timeout(Duration::from_secs(5)).unwrap();
    std::thread::sleep(Duration::from_millis(900));
    assert!(rig.host.confirm_pairing(&request.id, None).is_err());
    conn.expect_closed();
    assert!(rig.host.list_clients().unwrap().is_empty());
}

#[test]
fn confirm_race_a_confirm_at_the_deadline_is_welcomed_or_leaves_no_client() {
    let rig = Rig::with(|l| l.confirm_timeout = Duration::from_millis(300));
    // Confirms landing before, at and after the deadline: whichever side wins, a stored
    // client always got its welcome, and a refused confirm leaves nothing stored.
    for delay_ms in [100u64, 220, 260, 280, 295, 305, 320, 400] {
        let before = rig.host.list_clients().unwrap().len();
        let offer = rig.host.open_pairing().unwrap();
        let mut conn = Conn::pair(rig.addr, &offer, &Identity::new(), "Racer").unwrap();
        let request = rig.pairing.recv_timeout(Duration::from_secs(5)).unwrap();
        std::thread::sleep(Duration::from_millis(delay_ms));
        match rig.host.confirm_pairing(&request.id, None) {
            Ok(_) => {
                assert!(matches!(conn.expect(), HostMessage::Welcome(_)), "delay {delay_ms}: stored but not welcomed");
                assert_eq!(rig.host.list_clients().unwrap().len(), before + 1);
            }
            Err(_) => {
                conn.expect_closed();
                assert_eq!(rig.host.list_clients().unwrap().len(), before, "delay {delay_ms}: refused but stored");
            }
        }
    }
}

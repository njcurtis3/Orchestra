//! Runs the shared protocol fixtures (`protocol/fixtures/*.json`) through the Rust copy.
//! The TypeScript run is `desktop/src/protocol.test.ts`. The directory is read at test time,
//! so a new case file is picked up with no list to keep in step.

use std::fs;
use std::path::PathBuf;

use orchestra_core::wire::{
    self, ErrorCode, HandshakeReply, Method, Scope, WireError, decode_client, decode_handshake_reply,
    decode_handshake_request, decode_host, encode_client, encode_handshake_reply,
    encode_handshake_request, encode_host,
};
use serde_json::{Value, json};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../protocol/fixtures")
}

fn read_json(name: &str) -> Value {
    let text = fs::read_to_string(fixtures_dir().join(name)).expect("fixture is readable");
    serde_json::from_str(&text).expect("fixture is JSON")
}

fn case_files() -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(fixtures_dir())
        .expect("fixtures dir exists")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".json") && n != "constants.json")
        .collect();
    names.sort();
    names
}

/// Decode `text` in direction `dir` and, on success, encode it back to a JSON value.
fn round_trip(dir: &str, text: &str) -> Result<Value, WireError> {
    let encoded = match dir {
        "c2h" => encode_client(&decode_client(text)?)?,
        "h2c" => encode_host(&decode_host(text)?)?,
        "hs_c2h" => {
            decode_handshake_request(text)?;
            encode_handshake_request()
        }
        "hs_h2c" => {
            let reply: HandshakeReply = decode_handshake_reply(text)?;
            encode_handshake_reply(&reply)?
        }
        other => panic!("unknown dir {other}"),
    };
    Ok(serde_json::from_str(&encoded).expect("encoder writes JSON"))
}

#[test]
fn every_fixture_case_agrees_with_the_typescript_copy() {
    let files = case_files();
    let mut ran = 0;
    for file in &files {
        let case = read_json(file);
        let name = case["name"].as_str().expect("case has a name");
        assert_eq!(format!("{name}.json"), *file, "file name is the case name");
        let dir = case["dir"].as_str().expect("case has a dir");
        let text = match case.get("raw") {
            Some(raw) => raw.as_str().expect("raw is a string").to_string(),
            None => serde_json::to_string(&case["wire"]).unwrap(),
        };
        let client_only = case.get("client_only") == Some(&json!(true));
        match &case["expect"] {
            Value::String(ok) if ok == "ok" => {
                // Unknown enum values and the like only parse in the TypeScript copy.
                if !client_only {
                    let again = round_trip(dir, &text).unwrap_or_else(|e| panic!("{name}: {e}"));
                    assert_eq!(again, case["wire"], "{name}: encode(decode(wire)) == wire");
                }
            }
            Value::Object(e) => {
                let want = e["error"].as_str().expect("error code");
                let err = round_trip(dir, &text).expect_err(name);
                assert_eq!(err.code.as_str(), want, "{name}: {err}");
            }
            other => panic!("{name}: bad expect {other}"),
        }
        ran += 1;
    }
    assert_eq!(ran, files.len());
    assert!(ran >= 80, "expected the full fixture set, ran {ran}");
}

#[test]
fn constants_match_fixtures_constants_json() {
    let c = read_json("constants.json");
    assert_eq!(c["v"], json!(wire::PROTOCOL_VERSION));
    assert_eq!(c["supported"], json!(wire::SUPPORTED_VERSIONS));
    assert_eq!(c["max_text_bytes"], json!(wire::MAX_TEXT_BYTES));
    assert_eq!(c["max_message_bytes"], json!(wire::MAX_MESSAGE_BYTES));
    assert_eq!(c["max_id_bytes"], json!(wire::MAX_ID_BYTES));
    assert_eq!(c["methods"], json!(Method::ALL.map(Method::as_str)));
    assert_eq!(c["event_names"], json!(wire::EVENT_NAMES));
    assert_eq!(c["scopes"], json!(Scope::ALL.map(Scope::as_str)));
    assert_eq!(c["error_codes"], json!(ErrorCode::ALL.map(ErrorCode::as_str)));
    let scopes = c["method_scope"].as_object().expect("method_scope is an object");
    assert_eq!(scopes.len(), Method::ALL.len());
    for m in Method::ALL {
        assert_eq!(scopes[m.as_str()], json!(m.scope().as_str()), "{}", m.as_str());
    }
}

/// A message padded with an unknown field to an exact size; the host decoder is tolerant, so
/// a pong has nothing else to fail on.
fn padded(ty: &str, size: usize) -> String {
    let base = json!({"v": 1, "type": ty, "pad": ""}).to_string();
    json!({"v": 1, "type": ty, "pad": "x".repeat(size - base.len())}).to_string()
}

#[test]
fn oversize_messages_are_bad_request_before_parsing() {
    assert!(decode_host(&padded("pong", wire::MAX_MESSAGE_BYTES)).is_ok());
    let err = decode_host(&padded("pong", wire::MAX_MESSAGE_BYTES + 1)).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(err.message.contains("too large"));
    let err = decode_client(&padded("ping", wire::MAX_MESSAGE_BYTES + 1)).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(err.message.contains("too large"));
}

#[test]
fn client_strings_are_capped_in_utf8_bytes() {
    let hello = |name: &str| json!({"v": 1, "type": "hello", "client_name": name, "app_version": "1"}).to_string();
    assert!(decode_client(&hello(&"x".repeat(wire::MAX_TEXT_BYTES))).is_ok());
    assert_eq!(decode_client(&hello(&"x".repeat(wire::MAX_TEXT_BYTES + 1))).unwrap_err().code, ErrorCode::BadRequest);
    let wide = decode_client(&hello(&"\u{e9}".repeat(wire::MAX_TEXT_BYTES / 2 + 1))).unwrap_err();
    assert!(wide.message.contains("too long"));
}

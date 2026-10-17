//! Decode and encode protocol v1 messages. The rules match `protocol/codec.ts`:
//!
//! - Host input (client to host) is strict: an unknown field is `bad_request`.
//! - Client input (host to client) is tolerant: unknown fields are dropped.
//! - Both check, in order: size, JSON, object, `v`, `type`, then the fields. An unknown `v`
//!   is `unsupported_version` and an unknown `type` is `unknown_type`; anything else wrong
//!   is `bad_request`.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use super::args::{Answer, CancelTaskArgs, CreateTaskArgs, StartArgs, StopArgs, TaskIdArgs, TranscriptArgs};
use super::message::{
    Call, ClientMessage, ErrorBody, HandshakeReply, HostEvent, HostMessage,
};
use super::{
    ErrorCode, MAX_ID_BYTES, MAX_MESSAGE_BYTES, MAX_TEXT_BYTES, Method, PROTOCOL_NAME,
    PROTOCOL_VERSION, SUPPORTED_VERSIONS, WireError,
};

type Obj = Map<String, Value>;

fn bad<T>(message: &str) -> Result<T, WireError> {
    Err(WireError::bad(message))
}

fn integer(v: &Value) -> Option<i128> {
    v.as_i64()
        .map(i128::from)
        .or_else(|| v.as_u64().map(i128::from))
        .or_else(|| v.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i128))
}

fn envelope(text: &str) -> Result<Obj, WireError> {
    if text.len() > MAX_MESSAGE_BYTES {
        return bad("message too large");
    }
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return bad("not JSON");
    };
    let Value::Object(o) = value else {
        return bad("bad message");
    };
    let Some(v) = o.get("v") else {
        return bad("missing v");
    };
    let Some(n) = integer(v) else {
        return bad("bad v");
    };
    if !SUPPORTED_VERSIONS.iter().any(|s| i128::from(*s) == n) {
        return Err(WireError::new(ErrorCode::UnsupportedVersion, format!("unsupported version {n}")));
    }
    Ok(o)
}

fn type_of(o: &Obj) -> Result<String, WireError> {
    match o.get("type") {
        Some(Value::String(s)) => Ok(s.clone()),
        _ => bad("missing type"),
    }
}

fn unknown_type<T>(ty: &str) -> Result<T, WireError> {
    Err(WireError::new(ErrorCode::UnknownType, format!("unknown type {ty}")))
}

fn exact(o: &Obj, keys: &[&str]) -> Result<(), WireError> {
    match o.keys().find(|k| !keys.contains(&k.as_str())) {
        Some(k) => bad(&format!("unknown field {k}")),
        None => Ok(()),
    }
}

fn string(o: &Obj, k: &str) -> Result<String, WireError> {
    match o.get(k) {
        Some(Value::String(s)) => Ok(s.clone()),
        _ => bad(&format!("bad field {k}")),
    }
}

fn from<T: DeserializeOwned>(v: Value) -> Result<T, WireError> {
    serde_json::from_value(v).or_else(|_| bad("bad payload"))
}

fn finish(v: &Value) -> Result<String, WireError> {
    let json = v.to_string();
    if json.len() > MAX_MESSAGE_BYTES {
        return bad("message too large");
    }
    Ok(json)
}

/// The body struct as an object with `v` and `type` added.
fn tagged<T: Serialize>(ty: &str, body: &T) -> Value {
    let mut o = match serde_json::to_value(body) {
        Ok(Value::Object(o)) => o,
        _ => Obj::new(),
    };
    o.insert("v".into(), json!(PROTOCOL_VERSION));
    o.insert("type".into(), json!(ty));
    Value::Object(o)
}

// ---- Client to host (strict) ----

fn cap_strings(v: &Value) -> Result<(), WireError> {
    match v {
        Value::String(s) if s.len() > MAX_TEXT_BYTES => bad("text too long"),
        Value::Array(a) => a.iter().try_for_each(cap_strings),
        Value::Object(o) => o.iter().try_for_each(|(k, x)| {
            if k.len() > MAX_TEXT_BYTES {
                return bad("text too long");
            }
            cap_strings(x)
        }),
        _ => Ok(()),
    }
}

fn parse_call(method: Method, args: Value) -> Result<Call, WireError> {
    let Value::Object(a) = &args else {
        return bad("bad args");
    };
    let no_args = |call: Call| if a.is_empty() { Ok(call) } else { bad("unknown field in args") };
    match method {
        Method::ListProjects => no_args(Call::ListProjects),
        Method::ListSessions => no_args(Call::ListSessions),
        Method::ListInbox => no_args(Call::ListInbox),
        Method::ListTasks => no_args(Call::ListTasks),
        Method::StorageStatus => no_args(Call::StorageStatus),
        Method::GetTranscript => Ok(Call::GetTranscript(from::<TranscriptArgs>(args)?)),
        Method::StopSession => Ok(Call::StopSession(from::<StopArgs>(args)?)),
        Method::GetTaskDiff => Ok(Call::GetTaskDiff(from::<TaskIdArgs>(args)?)),
        Method::LandTask => Ok(Call::LandTask(from::<TaskIdArgs>(args)?)),
        Method::DiscardTask => Ok(Call::DiscardTask(from::<TaskIdArgs>(args)?)),
        Method::RetryCleanup => Ok(Call::RetryCleanup(from::<TaskIdArgs>(args)?)),
        Method::CancelTask => Ok(Call::CancelTask(from::<CancelTaskArgs>(args)?)),
        Method::StartSession => Ok(Call::StartSession(from::<StartArgs>(args)?)),
        Method::CreateTask => Ok(Call::CreateTask(from::<CreateTaskArgs>(args)?)),
        Method::Answer => Ok(Call::Answer(from::<Answer>(args)?)),
    }
}

pub fn decode_client(text: &str) -> Result<ClientMessage, WireError> {
    let mut o = envelope(text)?;
    let msg = match type_of(&o)?.as_str() {
        "hello" => {
            exact(&o, &["v", "type", "client_name", "app_version"])?;
            ClientMessage::Hello { client_name: string(&o, "client_name")?, app_version: string(&o, "app_version")? }
        }
        "ping" => {
            exact(&o, &["v", "type"])?;
            ClientMessage::Ping
        }
        "call" => {
            exact(&o, &["v", "type", "id", "method", "args"])?;
            let id = string(&o, "id")?;
            if id.is_empty() || id.len() > MAX_ID_BYTES {
                return bad("bad call id");
            }
            let Some(method) = Method::parse(&string(&o, "method")?) else {
                return bad("bad field method");
            };
            let Some(args) = o.remove("args") else {
                return bad("bad args");
            };
            ClientMessage::Call { id, call: parse_call(method, args)? }
        }
        other => return unknown_type(other),
    };
    cap_strings(&client_value(&msg))?;
    Ok(msg)
}

fn client_value(m: &ClientMessage) -> Value {
    match m {
        ClientMessage::Hello { client_name, app_version } => {
            json!({"v": PROTOCOL_VERSION, "type": "hello", "client_name": client_name, "app_version": app_version})
        }
        ClientMessage::Ping => json!({"v": PROTOCOL_VERSION, "type": "ping"}),
        ClientMessage::Call { id, call } => json!({
            "v": PROTOCOL_VERSION,
            "type": "call",
            "id": id,
            "method": call.method().as_str(),
            "args": call.args_value(),
        }),
    }
}

pub fn encode_client(m: &ClientMessage) -> Result<String, WireError> {
    finish(&client_value(m))
}

// ---- Host to client (tolerant) ----

pub fn decode_host(text: &str) -> Result<HostMessage, WireError> {
    let mut o = envelope(text)?;
    Ok(match type_of(&o)?.as_str() {
        "welcome" => HostMessage::Welcome(from(Value::Object(o))?),
        "snapshot" => HostMessage::Snapshot(Box::new(from(Value::Object(o))?)),
        "event" => {
            let Some(seq) = o.get("seq").and_then(Value::as_u64) else {
                return bad("bad field seq");
            };
            let name = string(&o, "name")?;
            let payload = o.remove("payload").unwrap_or(Value::Null);
            let event = match name.as_str() {
                "session-updated" => HostEvent::SessionUpdated(from(payload)?),
                "inbox-updated" => HostEvent::InboxUpdated(from(payload)?),
                "transcript-appended" => HostEvent::TranscriptAppended(from(payload)?),
                "task-updated" => HostEvent::TaskUpdated(from(payload)?),
                _ => HostEvent::Unknown { name, payload },
            };
            HostMessage::Event { seq, event }
        }
        "result" => {
            let id = string(&o, "id")?;
            let outcome = match (o.remove("ok"), o.remove("error")) {
                (Some(ok), None) => Ok(ok),
                (None, Some(e)) => Err(from::<ErrorBody>(e)?),
                _ => return bad("result needs exactly one of ok and error"),
            };
            HostMessage::Result { id, outcome }
        }
        "error" => HostMessage::Error(from(Value::Object(o))?),
        "pong" => HostMessage::Pong,
        other => return unknown_type(other),
    })
}

fn event_payload(e: &HostEvent) -> Value {
    let v = match e {
        HostEvent::SessionUpdated(p) => serde_json::to_value(p),
        HostEvent::InboxUpdated(p) => serde_json::to_value(p),
        HostEvent::TranscriptAppended(p) => serde_json::to_value(p),
        HostEvent::TaskUpdated(p) => serde_json::to_value(p),
        HostEvent::Unknown { payload, .. } => return payload.clone(),
    };
    v.unwrap_or(Value::Null)
}

pub fn encode_host(m: &HostMessage) -> Result<String, WireError> {
    let v = match m {
        HostMessage::Welcome(w) => tagged("welcome", w),
        HostMessage::Snapshot(s) => tagged("snapshot", s.as_ref()),
        HostMessage::Event { seq, event } => json!({
            "v": PROTOCOL_VERSION,
            "type": "event",
            "seq": seq,
            "name": event.name(),
            "payload": event_payload(event),
        }),
        HostMessage::Result { id, outcome: Ok(ok) } => {
            json!({"v": PROTOCOL_VERSION, "type": "result", "id": id, "ok": ok})
        }
        HostMessage::Result { id, outcome: Err(e) } => {
            json!({"v": PROTOCOL_VERSION, "type": "result", "id": id, "error": {"code": e.code, "message": e.message}})
        }
        HostMessage::Error(e) => tagged("error", e),
        HostMessage::Pong => json!({"v": PROTOCOL_VERSION, "type": "pong"}),
    };
    finish(&v)
}

// ---- Handshake: plaintext frames with no `type` ----

pub fn decode_handshake_request(text: &str) -> Result<(), WireError> {
    let o = envelope(text)?;
    if o.get("proto").and_then(Value::as_str) != Some(PROTOCOL_NAME) {
        return bad("bad proto");
    }
    exact(&o, &["proto", "v"])
}

pub fn encode_handshake_request() -> String {
    json!({"proto": PROTOCOL_NAME, "v": PROTOCOL_VERSION}).to_string()
}

pub fn decode_handshake_reply(text: &str) -> Result<HandshakeReply, WireError> {
    let o = envelope(text)?;
    if o.get("type").and_then(Value::as_str) == Some("error") {
        return Ok(HandshakeReply::Error(from(Value::Object(o))?));
    }
    if o.get("proto").and_then(Value::as_str) != Some(PROTOCOL_NAME) {
        return bad("bad proto");
    }
    let supported = from::<Vec<u64>>(o.get("supported").cloned().unwrap_or(Value::Null))?;
    Ok(HandshakeReply::Accept { supported })
}

pub fn encode_handshake_reply(r: &HandshakeReply) -> Result<String, WireError> {
    let v = match r {
        HandshakeReply::Accept { supported } => {
            json!({"proto": PROTOCOL_NAME, "v": PROTOCOL_VERSION, "supported": supported})
        }
        HandshakeReply::Error(e) => tagged("error", e),
    };
    finish(&v)
}

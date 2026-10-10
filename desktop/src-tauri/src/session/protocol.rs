//! Wire messages of the claude stream-json protocol: a tolerant line parser, and the
//! builders for everything we write to the child's stdin. Shapes follow
//! `tests/fixtures/stream_json/README.md`.

use std::collections::BTreeMap;

use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// `system/init`; carries claude's own session id.
    Init { session_id: Option<String> },
    /// `assistant`: the concatenated text blocks (possibly empty) and the first tool used.
    Assistant { text: String, tool_name: Option<String> },
    /// `control_request` with subtype `can_use_tool`.
    CanUseTool { request_id: String, tool_name: String, input: Value },
    /// `control_request` with a subtype we cannot answer. Surfaced, never answered.
    UnsupportedControlRequest { request_id: Option<String>, subtype: String },
    /// `result`: end of a turn.
    Result { is_error: bool, text: String },
    /// A known message type that carries no status (hook events, tool results, rate limits).
    Ignored,
    /// Unparseable or unrecognised. Counted by the state machine, never fatal.
    Unknown,
}

pub fn parse_line(line: &str) -> Message {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Message::Unknown;
    };
    let str_of = |v: &Value| v.as_str().map(str::to_string);
    match v.get("type").and_then(Value::as_str) {
        Some("system") => match v.get("subtype").and_then(Value::as_str) {
            Some("init") => Message::Init { session_id: v.get("session_id").and_then(str_of) },
            Some(_) => Message::Ignored,
            None => Message::Unknown,
        },
        Some("assistant") => {
            let blocks = v.pointer("/message/content").and_then(Value::as_array);
            let mut text = String::new();
            let mut tool_name = None;
            for b in blocks.into_iter().flatten() {
                match b.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = b.get("text").and_then(Value::as_str) {
                            text.push_str(t);
                        }
                    }
                    Some("tool_use") if tool_name.is_none() => {
                        tool_name = b.get("name").and_then(str_of);
                    }
                    _ => {}
                }
            }
            Message::Assistant { text, tool_name }
        }
        Some("control_request") => {
            let req = &v["request"];
            match req.get("subtype").and_then(Value::as_str) {
                Some("can_use_tool") => {}
                Some(other) => {
                    return Message::UnsupportedControlRequest {
                        request_id: v.get("request_id").and_then(str_of),
                        subtype: other.to_string(),
                    };
                }
                None => return Message::Unknown,
            }
            match (v.get("request_id").and_then(str_of), req.get("tool_name").and_then(str_of)) {
                (Some(request_id), Some(tool_name)) => Message::CanUseTool {
                    request_id,
                    tool_name,
                    input: req.get("input").cloned().unwrap_or(Value::Null),
                },
                _ => Message::Unknown,
            }
        }
        Some("result") => Message::Result {
            is_error: v.get("is_error").and_then(Value::as_bool).unwrap_or(false),
            text: v.get("result").and_then(Value::as_str).unwrap_or_default().to_string(),
        },
        Some("user") | Some("rate_limit_event") => Message::Ignored,
        _ => Message::Unknown,
    }
}

/// One user turn, written to stdin.
pub fn user_message(text: &str) -> String {
    json!({"type": "user", "message": {"role": "user", "content": text}}).to_string()
}

fn control_response(request_id: &str, response: Value) -> String {
    json!({
        "type": "control_response",
        "response": {"subtype": "success", "request_id": request_id, "response": response}
    })
    .to_string()
}

pub fn allow_response(request_id: &str, original_input: &Value) -> String {
    control_response(request_id, json!({"behavior": "allow", "updatedInput": original_input}))
}

pub fn deny_response(request_id: &str, message: &str) -> String {
    control_response(request_id, json!({"behavior": "deny", "message": message}))
}

/// AskUserQuestion: allow with the original input plus `answers` keyed by question text.
pub fn question_response(
    request_id: &str,
    original_input: &Value,
    answers: &BTreeMap<String, String>,
) -> String {
    let mut input = original_input.clone();
    if !input.is_object() {
        input = json!({});
    }
    input["answers"] = json!(answers);
    control_response(request_id, json!({"behavior": "allow", "updatedInput": input}))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const FIXTURES: [(&str, &str); 4] = [
        ("permission_allow", include_str!("../../tests/fixtures/stream_json/permission_allow.jsonl")),
        ("permission_deny", include_str!("../../tests/fixtures/stream_json/permission_deny.jsonl")),
        ("ask_user_question", include_str!("../../tests/fixtures/stream_json/ask_user_question.jsonl")),
        ("multi_turn", include_str!("../../tests/fixtures/stream_json/multi_turn.jsonl")),
    ];

    /// Inbound (`in`) wire messages of a fixture, as raw JSON strings.
    fn inbound(src: &str) -> Vec<String> {
        src.lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .filter(|d| d["_dir"] == "in")
            .map(|d| d["msg"].to_string())
            .collect()
    }

    #[test]
    fn every_fixture_line_parses_without_panic_and_nothing_is_unknown() {
        for (name, src) in FIXTURES {
            let lines = inbound(src);
            assert!(!lines.is_empty(), "{name}");
            for l in lines {
                assert_ne!(parse_line(&l), Message::Unknown, "{name}: {}", &l[..l.len().min(80)]);
            }
        }
    }

    #[test]
    fn fixtures_contain_expected_message_kinds() {
        let parsed: Vec<Message> = inbound(FIXTURES[0].1).iter().map(|l| parse_line(l)).collect();
        assert!(parsed.iter().any(|m| matches!(m, Message::Init { session_id: Some(_) })));
        assert!(parsed.iter().any(|m| matches!(m, Message::CanUseTool { tool_name, .. } if tool_name == "Bash")));
        assert!(parsed.iter().any(|m| matches!(m, Message::Result { is_error: false, .. })));
    }

    #[test]
    fn tolerates_garbage_and_unknown_types() {
        assert_eq!(parse_line("not json"), Message::Unknown);
        assert_eq!(parse_line("{}"), Message::Unknown);
        assert_eq!(parse_line(r#"{"type":"brand_new"}"#), Message::Unknown);
        assert_eq!(parse_line(r#"{"type":"control_request","request_id":"a","request":{}}"#), Message::Unknown);
        assert_eq!(parse_line(r#"{"type":"system","subtype":"hook_started"}"#), Message::Ignored);
        assert_eq!(parse_line("[1,2]"), Message::Unknown);
    }

    #[test]
    fn unsupported_control_request_is_typed() {
        assert_eq!(
            parse_line(r#"{"type":"control_request","request_id":"a","request":{"subtype":"other"}}"#),
            Message::UnsupportedControlRequest { request_id: Some("a".into()), subtype: "other".into() }
        );
        assert!(matches!(
            parse_line(r#"{"type":"control_request","request":{"subtype":"hook_callback"}}"#),
            Message::UnsupportedControlRequest { request_id: None, .. }
        ));
    }

    #[test]
    fn user_message_shape() {
        let v: Value = serde_json::from_str(&user_message("hi")).unwrap();
        assert_eq!(v, json!({"type":"user","message":{"role":"user","content":"hi"}}));
    }
}

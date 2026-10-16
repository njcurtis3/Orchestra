//! The readable record of a session: what was asked, what claude said, which tools it ran
//! and what they returned, and what each turn cost. It is kept apart from `SessionState`
//! and `state.json`: tool output can hold anything in the user's project, so it lives in
//! one append-only JSONL file per session (or in memory when storage is off) and is never
//! part of a snapshot.
//!
//! `entries_from_value` is pure. `TranscriptStore` does the I/O, best effort: a failed write
//! loses transcript lines, never the session.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::state::truncate_bytes;

/// Cap on one text or tool-result field, in bytes.
const MAX_FIELD_BYTES: usize = 8 * 1024;
/// Cap on the JSON of a tool's input, in bytes.
const MAX_INPUT_BYTES: usize = 2 * 1024;
/// Cap on one session's file. Past it a `Truncated` entry is written and appends stop.
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// How many entries `get_transcript` returns.
pub const PAGE_ENTRIES: usize = 1000;
const MAX_BLOCK_TYPE_CHARS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    /// Recorded by Orchestra when it writes a prompt or reply to claude.
    UserPrompt { text: String, truncated: bool },
    AssistantText { text: String, truncated: bool },
    ToolUse { id: String, name: String, input_preview: String, truncated: bool },
    ToolResult { tool_use_id: String, is_error: bool, content: String, truncated: bool },
    /// End of a turn. The cost is the session total as claude reports it, never a sum of
    /// these entries; usage and duration are for the turn.
    TurnResult {
        is_error: bool,
        subtype: Option<String>,
        total_cost_usd: Option<f64>,
        duration_ms: Option<u64>,
        num_turns: Option<u64>,
        usage: Option<Usage>,
    },
    /// The file reached its cap; nothing after this is recorded.
    Truncated,
}

/// An entry with its place in the session's file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub seq: u64,
    pub at: u64,
    #[serde(flatten)]
    pub entry: Entry,
}

/// What the frontend gets live.
#[derive(Debug, Clone, Serialize)]
pub struct TranscriptBatch {
    pub session_id: String,
    pub entries: Vec<Record>,
}

/// What `get_transcript` returns: the last entries, and where the next one will land.
#[derive(Debug, Clone, Serialize)]
pub struct TranscriptPage {
    pub entries: Vec<Record>,
    pub next_seq: u64,
}

fn capped(s: &str, max: usize) -> (String, bool) {
    (truncate_bytes(s, max), s.len() > max)
}

pub fn user_prompt(text: &str) -> Entry {
    let (text, truncated) = capped(text, MAX_FIELD_BYTES);
    Entry::UserPrompt { text, truncated }
}

/// Plain text of a tool result: a string as is, or an array of blocks with text joined,
/// images replaced by a placeholder and anything else named by its type.
fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .map(|b| match b.get("type").and_then(Value::as_str) {
                Some("text") => b.get("text").and_then(Value::as_str).unwrap_or_default().to_string(),
                Some("image") | Some("base64") => "[image]".to_string(),
                _ if b.pointer("/source/type").and_then(Value::as_str) == Some("base64") => "[image]".to_string(),
                Some(t) => format!("[block {}]", t.chars().take(MAX_BLOCK_TYPE_CHARS).collect::<String>()),
                None => "[block]".to_string(),
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The entries one stdout line yields, in order. Lines that carry no conversation (hooks,
/// rate limits, control requests) yield none.
pub fn entries_from_value(v: &Value) -> Vec<Entry> {
    let blocks = v.pointer("/message/content").and_then(Value::as_array);
    match v.get("type").and_then(Value::as_str) {
        Some("assistant") => blocks
            .into_iter()
            .flatten()
            .filter_map(|b| match b.get("type").and_then(Value::as_str) {
                Some("text") => {
                    let t = b.get("text").and_then(Value::as_str)?;
                    if t.trim().is_empty() {
                        return None;
                    }
                    let (text, truncated) = capped(t, MAX_FIELD_BYTES);
                    Some(Entry::AssistantText { text, truncated })
                }
                Some("tool_use") => {
                    let input = b.get("input").map(Value::to_string).unwrap_or_default();
                    let (input_preview, truncated) = capped(&input, MAX_INPUT_BYTES);
                    Some(Entry::ToolUse {
                        id: b.get("id").and_then(Value::as_str).unwrap_or_default().to_string(),
                        name: b.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
                        input_preview,
                        truncated,
                    })
                }
                _ => None,
            })
            .collect(),
        Some("user") => blocks
            .into_iter()
            .flatten()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            .map(|b| {
                let (content, truncated) = capped(&result_text(b.get("content").unwrap_or(&Value::Null)), MAX_FIELD_BYTES);
                Entry::ToolResult {
                    tool_use_id: b.get("tool_use_id").and_then(Value::as_str).unwrap_or_default().to_string(),
                    is_error: b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                    content,
                    truncated,
                }
            })
            .collect(),
        Some("result") => {
            let n = |k: &str| v.get(k).and_then(Value::as_u64);
            let usage = v.get("usage").filter(|u| u.is_object()).map(|u| {
                let g = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                Usage {
                    input: g("input_tokens"),
                    output: g("output_tokens"),
                    cache_read: g("cache_read_input_tokens"),
                    cache_creation: g("cache_creation_input_tokens"),
                }
            });
            vec![Entry::TurnResult {
                is_error: v.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                subtype: v.get("subtype").and_then(Value::as_str).map(|s| s.chars().take(64).collect()),
                total_cost_usd: v.get("total_cost_usd").and_then(Value::as_f64),
                duration_ms: n("duration_ms"),
                num_turns: n("num_turns"),
                usage,
            }]
        }
        _ => Vec::new(),
    }
}

/// Session ids are `s` and digits (`Sessions::start`). Anything else never becomes a file name.
pub fn is_session_id(id: &str) -> bool {
    id.len() <= 20 && id.strip_prefix('s').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

enum Backend {
    Disk(PathBuf),
    Memory,
}

/// One session's file. Always used under its own mutex.
#[derive(Default)]
struct FileState {
    /// Set on first touch, from the file when there is one.
    loaded: bool,
    next_seq: u64,
    bytes: u64,
    /// The cap was reached: nothing more is written.
    full: bool,
    /// The file ends in a torn line (no trailing newline): the next write starts a new line.
    needs_newline: bool,
    /// The JSONL lines, when the backend is memory.
    mem: Vec<String>,
}

pub struct TranscriptStore {
    backend: Backend,
    max_file_bytes: u64,
    /// Never held together with the session manager's state.
    files: Mutex<HashMap<String, Arc<Mutex<FileState>>>>,
}

fn parse_lines<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<Record> {
    lines.filter_map(|l| serde_json::from_str::<Record>(l).ok()).collect()
}

/// A session file as found on disk. Read as bytes: a torn tail can cut a multi-byte
/// character, and a file that is not valid UTF-8 must not read as missing.
struct RawFile {
    /// Lossy decode; the cut character becomes U+FFFD and its line fails to parse.
    text: String,
    /// Length of the raw file.
    bytes: u64,
    /// The raw file is non-empty and its last byte is not a newline.
    torn: bool,
}

/// `None` when the file cannot be read, which is how a missing file reads.
fn read_raw(path: &Path) -> Option<RawFile> {
    let raw = fs::read(path).ok()?;
    Some(RawFile {
        text: String::from_utf8_lossy(&raw).into_owned(),
        bytes: raw.len() as u64,
        torn: raw.last().is_some_and(|b| *b != b'\n'),
    })
}

impl TranscriptStore {
    pub fn disk(dir: PathBuf) -> Self {
        Self::with(Backend::Disk(dir), MAX_FILE_BYTES)
    }

    /// For tests, and for a run where storage is off.
    pub fn memory() -> Self {
        Self::with(Backend::Memory, MAX_FILE_BYTES)
    }

    fn with(backend: Backend, max_file_bytes: u64) -> Self {
        Self { backend, max_file_bytes, files: Mutex::new(HashMap::new()) }
    }

    fn path(&self, id: &str) -> Option<PathBuf> {
        match &self.backend {
            Backend::Disk(dir) if is_session_id(id) => Some(dir.join(format!("{id}.jsonl"))),
            _ => None,
        }
    }

    fn file(&self, id: &str) -> Arc<Mutex<FileState>> {
        let mut files = self.files.lock().unwrap_or_else(|e| e.into_inner());
        Arc::clone(files.entry(id.to_string()).or_default())
    }

    /// Read what an earlier run left: where the numbering continues, how big the file is and
    /// whether it already hit the cap.
    fn load(&self, id: &str, st: &mut FileState) {
        if st.loaded {
            return;
        }
        st.loaded = true;
        st.next_seq = 1;
        if let Some(file) = self.path(id).and_then(|p| read_raw(&p)) {
            st.bytes = file.bytes;
            st.needs_newline = file.torn;
            // Every parseable line counts: a torn tail or a garbage line must not reset the numbering.
            let records = parse_lines(file.text.lines());
            if let Some(max) = records.iter().map(|r| r.seq).max() {
                st.next_seq = max + 1;
            }
            st.full = records.iter().any(|r| r.entry == Entry::Truncated);
        }
    }

    /// Append entries for a session, numbering them. Returns what was recorded, which is
    /// empty for a bad id or a full file. A failed disk write still returns the records, so
    /// the live view is not affected.
    pub fn append(&self, id: &str, entries: Vec<Entry>, at: u64) -> Vec<Record> {
        if entries.is_empty() || !is_session_id(id) {
            return Vec::new();
        }
        let file = self.file(id);
        let mut st = file.lock().unwrap_or_else(|e| e.into_inner());
        self.load(id, &mut st);
        let mut out = Vec::new();
        let mut text = String::new();
        for entry in entries {
            if st.full {
                break;
            }
            let mut rec = Record { seq: st.next_seq, at, entry };
            let mut line = serde_json::to_string(&rec).unwrap_or_default();
            // The newline that separates a torn tail from the first new line counts too.
            let lead = usize::from(st.needs_newline);
            if st.bytes + (lead + text.len() + line.len() + 1) as u64 > self.max_file_bytes {
                rec = Record { seq: st.next_seq, at, entry: Entry::Truncated };
                line = serde_json::to_string(&rec).unwrap_or_default();
                st.full = true;
            }
            st.next_seq += 1;
            text.push_str(&line);
            text.push('\n');
            out.push(rec);
        }
        match (&self.backend, self.path(id)) {
            (Backend::Disk(dir), Some(path)) => {
                if st.needs_newline && !text.is_empty() {
                    text.insert(0, '\n');
                }
                let _ = fs::create_dir_all(dir);
                let wrote = OpenOptions::new().create(true).append(true).open(path).and_then(|mut f| f.write_all(text.as_bytes()));
                if wrote.is_ok() {
                    st.needs_newline = false;
                }
            }
            _ => st.mem.extend(text.lines().map(str::to_string)),
        }
        st.bytes += text.len() as u64;
        out
    }

    /// The last `PAGE_ENTRIES` entries and the next seq.
    pub fn read(&self, id: &str) -> TranscriptPage {
        if !is_session_id(id) {
            return TranscriptPage { entries: Vec::new(), next_seq: 1 };
        }
        let file = self.file(id);
        let mut st = file.lock().unwrap_or_else(|e| e.into_inner());
        self.load(id, &mut st);
        let mut all = match self.path(id).and_then(|p| read_raw(&p)) {
            Some(file) => parse_lines(file.text.lines()),
            None => parse_lines(st.mem.iter().map(String::as_str)),
        };
        let from = all.len().saturating_sub(PAGE_ENTRIES);
        TranscriptPage { entries: all.split_off(from), next_seq: st.next_seq }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::protocol::tests::FIXTURES;
    use serde_json::json;

    fn fixture_entries(name: &str) -> Vec<Entry> {
        let src = FIXTURES.iter().find(|(n, _)| *n == name).unwrap().1;
        src.lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .filter(|d| d["_dir"] == "in")
            .flat_map(|d| entries_from_value(&d["msg"]))
            .collect()
    }

    fn tool_result(content: Value) -> Value {
        json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":content}]}})
    }

    fn content_of(v: Value) -> String {
        match entries_from_value(&v).remove(0) {
            Entry::ToolResult { content, .. } => content,
            e => panic!("{e:?}"),
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("orchestra-transcript-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn tool_result_accepts_string_and_array() {
        assert_eq!(content_of(tool_result(json!("plain"))), "plain");
        let blocks = json!([{"type":"text","text":"a"},{"type":"text","text":"b"}]);
        assert_eq!(content_of(tool_result(blocks)), "a\nb");
        assert_eq!(content_of(tool_result(Value::Null)), "");
    }

    #[test]
    fn image_block_is_replaced() {
        let blocks = json!([
            {"type":"text","text":"before"},
            {"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAAA"}},
            {"source":{"type":"base64","data":"BBBB"}, "type":"weird"},
            {"type":"tool_reference","tool_name":"x"},
            {"nothing":true}
        ]);
        let got = content_of(tool_result(blocks));
        assert_eq!(got, "before\n[image]\n[image]\n[block tool_reference]\n[block]");
        assert!(!got.contains("AAAA") && !got.contains("BBBB"));
    }

    #[test]
    fn entry_is_capped_on_char_boundary() {
        // 3-byte chars: the byte cap falls inside one.
        let long = "€".repeat(MAX_FIELD_BYTES);
        match entries_from_value(&tool_result(json!(long))).remove(0) {
            Entry::ToolResult { content, truncated, .. } => {
                assert!(truncated && content.len() <= MAX_FIELD_BYTES && content.chars().all(|c| c == '€'));
            }
            e => panic!("{e:?}"),
        }
        let input = json!({"command": "x".repeat(MAX_INPUT_BYTES * 2)});
        let v = json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"t","name":"Bash","input":input}]}});
        match entries_from_value(&v).remove(0) {
            Entry::ToolUse { input_preview, truncated, .. } => assert!(truncated && input_preview.len() <= MAX_INPUT_BYTES),
            e => panic!("{e:?}"),
        }
        assert!(matches!(user_prompt("short"), Entry::UserPrompt { truncated: false, .. }));
    }

    #[test]
    fn file_cap_writes_marker_then_stops() {
        let store = TranscriptStore::with(Backend::Memory, 600);
        let mut all = Vec::new();
        for i in 0..20 {
            all.extend(store.append("s1", vec![user_prompt(&format!("prompt number {i} with some padding"))], 1));
        }
        let marker = all.iter().filter(|r| r.entry == Entry::Truncated).count();
        assert_eq!(marker, 1, "exactly one marker");
        assert_eq!(all.last().unwrap().entry, Entry::Truncated, "and it is the last record");
        assert!(all.len() < 20, "appends stopped");
        assert!(store.append("s1", vec![user_prompt("more")], 2).is_empty());
        assert_eq!(store.read("s1").entries.len(), all.len());

        // The cap survives a restart.
        let d = scratch("cap");
        let disk = TranscriptStore::with(Backend::Disk(d.clone()), 600);
        for i in 0..20 {
            disk.append("s7", vec![user_prompt(&format!("prompt number {i} with some padding"))], 1);
        }
        let again = TranscriptStore::with(Backend::Disk(d.clone()), 600);
        assert!(again.append("s7", vec![user_prompt("after restart")], 3).is_empty());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn rejects_bad_session_id() {
        for bad in ["", "s", "S1", "s1a", "s-1", "../s1", "s1/../s2", "s1.jsonl", "s１", "x1", "s123456789012345678901"] {
            assert!(!is_session_id(bad), "{bad}");
        }
        assert!(is_session_id("s1") && is_session_id("s42"));
        let d = scratch("badid");
        let store = TranscriptStore::disk(d.clone());
        assert!(store.append("../escape", vec![user_prompt("x")], 1).is_empty());
        assert!(store.read("../escape").entries.is_empty());
        assert!(!d.exists(), "no directory or file was created for a bad id");
    }

    #[test]
    fn disk_numbering_continues_after_restart() {
        let d = scratch("disk");
        let a = TranscriptStore::disk(d.clone());
        assert_eq!(a.append("s3", vec![user_prompt("one"), user_prompt("two")], 5).len(), 2);
        let b = TranscriptStore::disk(d.clone());
        let r = b.append("s3", vec![user_prompt("three")], 6);
        assert_eq!(r[0].seq, 3);
        let page = b.read("s3");
        assert_eq!((page.entries.len(), page.next_seq), (3, 4));
        assert_eq!(page.entries[0].seq, 1);
        let _ = fs::remove_dir_all(&d);
    }

    fn seeded(name: &str, file: &str, body: &str) -> (PathBuf, TranscriptStore) {
        let d = scratch(name);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(file), body).unwrap();
        let store = TranscriptStore::disk(d.clone());
        (d, store)
    }

    fn complete_lines(n: u64) -> String {
        (1..=n).map(|seq| serde_json::to_string(&Record { seq, at: 1, entry: user_prompt("x") }).unwrap() + "\n").collect()
    }

    #[test]
    fn torn_tail_keeps_max_seq_and_starts_on_a_new_line() {
        let body = complete_lines(7) + "{\"seq\":8,\"at\":1,\"kind\":\"user_pr";
        let (d, store) = seeded("torn", "s5.jsonl", &body);
        let r = store.append("s5", vec![user_prompt("after")], 2);
        assert_eq!(r[0].seq, 8);
        let text = fs::read_to_string(d.join("s5.jsonl")).unwrap();
        assert!(text.ends_with('\n'));
        let last = text.lines().last().unwrap();
        assert_eq!(serde_json::from_str::<Record>(last).unwrap().seq, 8);
        assert_eq!(parse_lines(text.lines()).len(), 8, "the 7 complete lines and the new one parse");
        assert_eq!(text.lines().count(), 9, "the fragment keeps its own line");
        assert_eq!(store.read("s5").next_seq, 9);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn tear_inside_a_multibyte_char_keeps_history_and_numbering() {
        let mut raw = complete_lines(7).into_bytes();
        raw.extend_from_slice(b"{\"seq\":8,\"at\":1,\"kind\":\"user_prompt\",\"text\":\"caf");
        raw.push("\u{e9}".as_bytes()[0]); // first byte of a 2-byte char, then the file ends
        assert!(std::str::from_utf8(&raw).is_err());
        let d = scratch("utf8");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("s9.jsonl"), &raw).unwrap();
        let store = TranscriptStore::disk(d.clone());

        let mut st = FileState::default();
        store.load("s9", &mut st);
        assert_eq!(st.bytes, raw.len() as u64, "byte count is the raw length");
        assert!(st.needs_newline);
        assert_eq!(st.next_seq, 8);

        let r = store.append("s9", vec![user_prompt("after")], 2);
        assert_eq!(r[0].seq, 8);
        let bytes = fs::read(d.join("s9.jsonl")).unwrap();
        assert_eq!(bytes.len(), raw.len() + 1 + serde_json::to_string(&r[0]).unwrap().len() + 1);
        let text = String::from_utf8_lossy(&bytes);
        let last = text.lines().last().unwrap();
        assert_eq!(serde_json::from_str::<Record>(last).unwrap().seq, 8, "new record is on its own line");
        let page = store.read("s9");
        assert_eq!(page.entries.len(), 8, "7 good entries and the new one");
        assert_eq!(page.entries[0].seq, 1);
        assert_eq!(page.next_seq, 9);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn newline_before_a_torn_tail_counts_toward_the_cap() {
        let body = complete_lines(2) + "{\"seq\":3";
        let d = scratch("capnl");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("s2.jsonl"), &body).unwrap();
        let line_len = serde_json::to_string(&Record { seq: 3, at: 1, entry: user_prompt("x") }).unwrap().len() as u64;
        // Room for the line and its own newline, but not for the separator too.
        let cap = body.len() as u64 + line_len + 1;
        let store = TranscriptStore::with(Backend::Disk(d.clone()), cap);
        let r = store.append("s2", vec![user_prompt("x")], 1);
        assert_eq!(r[0].entry, Entry::Truncated);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn garbage_line_in_the_middle_does_not_reset_seq() {
        let body = complete_lines(3) + "not json at all\n" + &complete_lines(5)[complete_lines(3).len()..];
        let (d, store) = seeded("garbage", "s6.jsonl", &body);
        assert_eq!(store.append("s6", vec![user_prompt("after")], 2)[0].seq, 6);
        // A garbage last line, too: numbering still comes from the best earlier line.
        let (d2, store2) = seeded("garbage-tail", "s6.jsonl", &(complete_lines(4) + "garbage\n"));
        assert_eq!(store2.append("s6", vec![user_prompt("after")], 2)[0].seq, 5);
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&d2);
    }

    #[test]
    fn read_returns_only_the_last_page() {
        let store = TranscriptStore::memory();
        for i in 0..PAGE_ENTRIES + 5 {
            store.append("s1", vec![user_prompt(&i.to_string())], 1);
        }
        let page = store.read("s1");
        assert_eq!(page.entries.len(), PAGE_ENTRIES);
        assert_eq!(page.entries[0].seq, 6);
        assert_eq!(page.next_seq, PAGE_ENTRIES as u64 + 6);
    }

    #[test]
    fn permission_allow_fixture_yields_tool_use_and_result() {
        let entries = fixture_entries("permission_allow");
        let use_ = entries.iter().position(|e| matches!(e, Entry::ToolUse { name, .. } if name == "Bash")).unwrap();
        let res = entries.iter().position(|e| matches!(e, Entry::ToolResult { is_error: false, .. })).unwrap();
        assert!(use_ < res, "the call comes before its result");
        assert!(matches!(entries.last().unwrap(), Entry::TurnResult { is_error: false, .. }));
        if let Entry::ToolUse { input_preview, .. } = &entries[use_] {
            assert!(input_preview.contains("command"));
        }
    }

    #[test]
    fn multi_turn_fixture_cost_is_last_not_sum() {
        let costs: Vec<f64> = fixture_entries("multi_turn")
            .into_iter()
            .filter_map(|e| if let Entry::TurnResult { total_cost_usd, .. } = e { total_cost_usd } else { None })
            .collect();
        assert_eq!(costs.len(), 2);
        assert!(costs[1] > costs[0], "the reported total rises");
        assert_eq!(costs[1], 0.00400435, "the latest entry carries claude's own total");
        assert!((costs[1] - (costs[0] + costs[1])).abs() > 1e-6, "and it is not a sum");
    }

    #[test]
    fn turn_result_reads_usage_and_tolerates_absence() {
        let v = json!({"type":"result","subtype":"success","is_error":false,"duration_ms":12,"num_turns":2,"total_cost_usd":0.5,
            "usage":{"input_tokens":1,"output_tokens":2,"cache_read_input_tokens":3,"cache_creation_input_tokens":4}});
        match entries_from_value(&v).remove(0) {
            Entry::TurnResult { usage, duration_ms, num_turns, .. } => {
                assert_eq!(usage, Some(Usage { input: 1, output: 2, cache_read: 3, cache_creation: 4 }));
                assert_eq!((duration_ms, num_turns), (Some(12), Some(2)));
            }
            e => panic!("{e:?}"),
        }
        assert!(matches!(entries_from_value(&json!({"type":"result"})).remove(0), Entry::TurnResult { usage: None, .. }));
        assert!(entries_from_value(&json!({"type":"rate_limit_event"})).is_empty());
        assert!(entries_from_value(&json!([1, 2])).is_empty());
    }

    #[test]
    fn records_roundtrip_through_json() {
        let rec = Record { seq: 4, at: 9, entry: Entry::ToolResult { tool_use_id: "t".into(), is_error: true, content: "c".into(), truncated: false } };
        let s = serde_json::to_string(&rec).unwrap();
        assert!(s.contains("\"kind\":\"tool_result\"") && s.contains("\"seq\":4"));
        assert_eq!(serde_json::from_str::<Record>(&s).unwrap(), rec);
    }
}

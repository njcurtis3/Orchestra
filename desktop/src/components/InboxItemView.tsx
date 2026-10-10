import { useState } from "react";
import { answer } from "../ipc";
import { replyThenClear } from "../reply";
import { MAX_TEXT_BYTES, type Answer, type InboxItem, type Question } from "../types";

type Props = { item: InboxItem; onError: (e: unknown) => void };

const tooLong = (s: string) => new TextEncoder().encode(s).length > MAX_TEXT_BYTES;

export function InboxItemView({ item, onError }: Props) {
  const [busy, setBusy] = useState(false);

  // Resolves true only when the answer was sent.
  async function send(a: Answer): Promise<boolean> {
    if (busy) return false;
    setBusy(true);
    try {
      await answer(a);
      return true;
    } catch (e) {
      onError(e);
      return false;
    } finally {
      setBusy(false);
    }
  }

  return (
    <li className={`card kind-${item.kind}${item.expired ? " expired" : item.resolved ? " resolved" : ""}`}>
      <div className="row">
        <strong>{item.kind}</strong>
        {item.expired ? (
          <span className="badge status-expired">Expired</span>
        ) : (
          item.resolved && <span className="badge status-ended">Resolved</span>
        )}
      </div>
      <div className="meta">{new Date(item.created_at).toLocaleString()}</div>
      {item.expired && <p className="meta">This request expired when Orchestra closed and can no longer be answered.</p>}
      {item.kind === "permission" && <Permission item={item} busy={busy} send={send} />}
      {item.kind === "question" && <Questions item={item} busy={busy} send={send} />}
      {item.kind === "finished" && <Finished item={item} busy={busy} send={send} />}
      {item.kind === "error" && (
        <>
          <p className="text">{item.summary}</p>
          <ResultText item={item} />
        </>
      )}
    </li>
  );
}

type Sub = { item: InboxItem; busy: boolean; send: (a: Answer) => Promise<boolean> };

function Permission({ item, busy, send }: Sub) {
  const [message, setMessage] = useState("");
  const open = !item.resolved;
  return (
    <>
      <p className="text">{item.summary}</p>
      {item.tool_name && (
        <p>
          Tool: <code>{item.tool_name}</code>
        </p>
      )}
      {item.tool_input_preview && <pre>{item.tool_input_preview}</pre>}
      {open && (
        <>
          <label>
            Deny message (optional)
            <input type="text" value={message} onChange={(e) => setMessage(e.target.value)} />
          </label>
          <div className="actions">
            <button
              type="button"
              disabled={busy}
              onClick={() => send({ kind: "permission", item_id: item.id, decision: "allow" })}
            >
              Allow
            </button>
            <button
              type="button"
              disabled={busy || tooLong(message)}
              onClick={() =>
                send({
                  kind: "permission",
                  item_id: item.id,
                  decision: "deny",
                  ...(message.trim() ? { message } : {}),
                })
              }
            >
              Deny
            </button>
          </div>
        </>
      )}
    </>
  );
}

function Questions({ item, busy, send }: Sub) {
  const questions = item.questions ?? [];
  // picks: chosen option labels per question text; other: free text per question text.
  const [picks, setPicks] = useState<Record<string, string[]>>({});
  const [other, setOther] = useState<Record<string, string>>({});

  function toggle(q: Question, label: string, checked: boolean) {
    setPicks((p) => {
      const cur = p[q.question] ?? [];
      const next = q.multi_select
        ? checked
          ? [...cur, label]
          : cur.filter((l) => l !== label)
        : [label];
      return { ...p, [q.question]: next };
    });
  }

  function collect(): Record<string, string> {
    const out: Record<string, string> = {};
    for (const q of questions) {
      const parts = [...(picks[q.question] ?? [])];
      const o = (other[q.question] ?? "").trim();
      if (o) parts.push(o);
      if (parts.length) out[q.question] = parts.join(", ");
    }
    return out;
  }

  const answers = collect();
  const complete = questions.length > 0 && questions.every((q) => q.question in answers);
  const long = Object.values(answers).some(tooLong);

  return (
    <>
      {questions.map((q, qi) => (
        <fieldset key={qi} disabled={item.resolved}>
          <legend>{q.header ? `${q.header}: ${q.question}` : q.question}</legend>
          {q.options.map((o) => (
            <label key={o.label} className="option">
              <input
                type={q.multi_select ? "checkbox" : "radio"}
                name={`${item.id}-${qi}`}
                checked={(picks[q.question] ?? []).includes(o.label)}
                onChange={(e) => toggle(q, o.label, e.target.checked)}
              />
              <span>
                {o.label}
                {o.description && <span className="meta"> {o.description}</span>}
              </span>
            </label>
          ))}
          <label>
            Other
            <input
              type="text"
              value={other[q.question] ?? ""}
              onChange={(e) => setOther((m) => ({ ...m, [q.question]: e.target.value }))}
            />
          </label>
        </fieldset>
      ))}
      {!item.resolved && (
        <div className="actions">
          <button
            type="button"
            disabled={busy || !complete || long}
            onClick={() => send({ kind: "question", item_id: item.id, answers })}
          >
            Send answers
          </button>
        </div>
      )}
    </>
  );
}

// The summary is cut to 300 chars; this is the whole text, scrollable.
function ResultText({ item }: { item: InboxItem }) {
  if (!item.result_text || item.result_text === item.summary) return null;
  return <pre className="result-text">{item.result_text}</pre>;
}

function Finished({ item, busy, send }: Sub) {
  const [text, setText] = useState("");
  const valid = text.trim() !== "" && !tooLong(text);
  return (
    <>
      <p className="text">{item.summary}</p>
      <ResultText item={item} />
      {!item.resolved && (
        <>
          <label>
            Reply
            <textarea rows={2} value={text} onChange={(e) => setText(e.target.value)} />
          </label>
          <div className="actions">
            <button
              type="button"
              disabled={busy || !valid}
              onClick={() =>
                void replyThenClear(
                  send,
                  { kind: "reply", session_id: item.session_id, text },
                  () => setText(""),
                )
              }
            >
              Send reply
            </button>
          </div>
        </>
      )}
    </>
  );
}

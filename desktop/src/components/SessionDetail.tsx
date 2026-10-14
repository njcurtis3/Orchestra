import { useEffect, useState } from "react";
import { answer, getTranscript, onTranscriptAppended } from "../ipc";
import { canPrompt, costLabel, isLong, mergeTranscript, preview, promptNote, showsPrompt, usageLabel } from "../detail";
import { MAX_TEXT_BYTES, type SessionSummary, type TranscriptRecord } from "../types";
import { MODE_LABELS, STATUS_LABELS } from "./SessionList";

type Props = { session: SessionSummary; onBack: () => void; onError: (e: unknown) => void };

const tooLong = (s: string) => new TextEncoder().encode(s).length > MAX_TEXT_BYTES;

export function SessionDetail({ session, onBack, onError }: Props) {
  const [entries, setEntries] = useState<TranscriptRecord[]>([]);
  const id = session.id;

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | null = null;
    setEntries([]);
    // Subscribe first, then load, and merge by seq: nothing emitted during the load is lost,
    // and an entry seen both ways is shown once.
    void (async () => {
      try {
        const u = await onTranscriptAppended((batch) => {
          if (!cancelled && batch.session_id === id) setEntries((cur) => mergeTranscript(cur, batch.entries));
        }, onError);
        if (cancelled) {
          u();
          return;
        }
        unlisten = u;
        const page = await getTranscript(id);
        if (!cancelled) setEntries((cur) => mergeTranscript(cur, page.entries));
      } catch (e) {
        onError(e);
      }
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [id, onError]);

  const usage = usageLabel(entries);
  return (
    <section className="panel detail" aria-labelledby="detail-h">
      <div className="row">
        <h2 id="detail-h">{session.project_name}</h2>
        <button type="button" onClick={onBack}>
          Back to inbox
        </button>
      </div>
      <div className="meta">
        <span className={`badge status-${session.status}`}>{STATUS_LABELS[session.status]}</span>{" "}
        Model: {session.model ?? "default"} · Mode: {session.permission_mode ? MODE_LABELS[session.permission_mode] : "default"}
      </div>
      <div className="card cost">
        <div>{costLabel(entries)}</div>
        {usage && <div className="meta">{usage}</div>}
      </div>
      <ol className="list transcript" aria-label="Transcript">
        {entries.length === 0 && <li className="empty">Nothing recorded yet.</li>}
        {entries.map((r) => (
          <li key={r.seq} className={`card entry entry-${r.kind}`}>
            <Entry r={r} />
          </li>
        ))}
      </ol>
      <PromptBox session={session} onError={onError} />
    </section>
  );
}

// Plain React text only: no markdown, no HTML. Long fields start collapsed.
function Body({ text, truncated }: { text: string; truncated: boolean }) {
  const note = truncated && <span className="meta"> (shortened)</span>;
  if (!isLong(text)) {
    return (
      <p className="text">
        {text}
        {note}
      </p>
    );
  }
  return (
    <details>
      <summary>{preview(text)}</summary>
      <p className="text">
        {text}
        {note}
      </p>
    </details>
  );
}

function Entry({ r }: { r: TranscriptRecord }) {
  switch (r.kind) {
    case "user_prompt":
      return (
        <>
          <strong>You</strong>
          <Body text={r.text} truncated={r.truncated} />
        </>
      );
    case "assistant_text":
      return (
        <>
          <strong>Claude</strong>
          <Body text={r.text} truncated={r.truncated} />
        </>
      );
    case "tool_use":
      return (
        <details>
          <summary>
            <strong>Tool: {r.name}</strong> <span className="meta">{preview(r.input_preview)}</span>
          </summary>
          <pre>
            {r.input_preview}
            {r.truncated ? "\n(shortened)" : ""}
          </pre>
        </details>
      );
    case "tool_result":
      return (
        <details>
          <summary>
            <strong>{r.is_error ? "Tool error" : "Tool result"}</strong> <span className="meta">{preview(r.content)}</span>
          </summary>
          <pre>
            {r.content}
            {r.truncated ? "\n(shortened)" : ""}
          </pre>
        </details>
      );
    case "turn_result":
      return (
        <div className="meta">
          {r.is_error ? "Turn ended with an error" : "Turn finished"}
          {r.num_turns !== null && ` · ${r.num_turns} turns`}
          {r.duration_ms !== null && ` · ${(r.duration_ms / 1000).toFixed(1)} s`}
        </div>
      );
    case "truncated":
      return <div className="meta">The transcript reached its size limit. Later activity is not recorded.</div>;
  }
}

function PromptBox({ session, onError }: { session: SessionSummary; onError: (e: unknown) => void }) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  if (!showsPrompt(session.status)) return null;
  const enabled = canPrompt(session.status);
  const note = promptNote(session.status);
  const valid = text.trim() !== "" && !tooLong(text);

  async function send() {
    if (busy || !enabled || !valid) return;
    setBusy(true);
    try {
      await answer({ kind: "reply", session_id: session.id, text });
      setText("");
    } catch (e) {
      onError(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="prompt">
      <label>
        Follow-up
        <textarea
          rows={2}
          value={text}
          disabled={!enabled}
          onChange={(e) => setText(e.target.value)}
          aria-describedby={note ? "prompt-note" : undefined}
        />
      </label>
      {note && (
        <p id="prompt-note" className="meta">
          {note}
        </p>
      )}
      {session.status === "suspended" && <p className="meta">Sending resumes this paused session.</p>}
      {tooLong(text) && <p className="hint">Message is too long.</p>}
      <div className="actions">
        <button type="button" disabled={busy || !enabled || !valid} onClick={() => void send()}>
          Send
        </button>
      </div>
    </div>
  );
}

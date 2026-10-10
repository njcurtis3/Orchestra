import { useState } from "react";
import { startSession } from "../ipc";
import { MAX_TEXT_BYTES, type Project } from "../types";

type Props = { projects: Project[]; onError: (e: unknown) => void };

export function NewSession({ projects, onError }: Props) {
  const [projectId, setProjectId] = useState("");
  const [prompt, setPrompt] = useState("");
  const [busy, setBusy] = useState(false);

  const selected = projectId || projects[0]?.id || "";
  const bytes = new TextEncoder().encode(prompt).length;
  const valid = selected !== "" && prompt.trim() !== "" && bytes <= MAX_TEXT_BYTES;

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (!valid || busy) return;
    setBusy(true);
    try {
      await startSession(selected, prompt);
      setPrompt("");
    } catch (err) {
      onError(err);
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className="new-session" onSubmit={submit}>
      <label>
        Project
        <select value={selected} onChange={(e) => setProjectId(e.target.value)}>
          {projects.length === 0 && <option value="">Open a project first</option>}
          {projects.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </select>
      </label>
      <label>
        Prompt
        <textarea
          rows={2}
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
          placeholder="What should Claude do?"
        />
      </label>
      <button type="submit" disabled={!valid || busy}>
        Start session
      </button>
      {bytes > MAX_TEXT_BYTES && <p className="hint">Prompt is too long.</p>}
    </form>
  );
}

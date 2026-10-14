import { useState } from "react";
import { startSession } from "../ipc";
import { MAX_TEXT_BYTES, MODELS, PERMISSION_MODES, type Model, type PermissionMode, type Project } from "../types";

const MODE_LABELS: Record<PermissionMode, string> = {
  manual: "Manual",
  acceptEdits: "Accept edits",
  plan: "Plan",
};

type Props = { projects: Project[]; onError: (e: unknown) => void };

export function NewSession({ projects, onError }: Props) {
  const [projectId, setProjectId] = useState("");
  const [prompt, setPrompt] = useState("");
  // "" is Default: no flag, so claude uses its own setting.
  const [model, setModel] = useState<Model | "">("");
  const [mode, setMode] = useState<PermissionMode | "">("");
  const [busy, setBusy] = useState(false);

  const selected = projectId || projects[0]?.id || "";
  const bytes = new TextEncoder().encode(prompt).length;
  const valid = selected !== "" && prompt.trim() !== "" && bytes <= MAX_TEXT_BYTES;

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (!valid || busy) return;
    setBusy(true);
    try {
      await startSession(selected, prompt, model || null, mode || null);
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
        Model
        <select value={model} onChange={(e) => setModel(e.target.value as Model | "")}>
          <option value="">Default</option>
          {MODELS.map((m) => (
            <option key={m} value={m}>
              {m.charAt(0).toUpperCase() + m.slice(1)}
            </option>
          ))}
        </select>
      </label>
      <label>
        Permission mode
        <select value={mode} onChange={(e) => setMode(e.target.value as PermissionMode | "")}>
          <option value="">Default</option>
          {PERMISSION_MODES.map((m) => (
            <option key={m} value={m}>
              {MODE_LABELS[m]}
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

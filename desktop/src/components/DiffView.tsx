import { DIFF_CAP_KIB } from "../types";
import { diffLineClass, splitPatch } from "../tasks";

type Props = { patch: string; truncated: boolean };

// The patch is untrusted text. Every line is a React text node: no markup is parsed, no
// markdown, no links. The class comes from how the line starts and nothing else.
export function DiffView({ patch, truncated }: Props) {
  const lines = splitPatch(patch);
  return (
    <div>
      {truncated && (
        <p className="hint" role="status">
          Diff truncated at {DIFF_CAP_KIB} KiB; the rest is not shown.
        </p>
      )}
      {lines.length === 0 ? (
        <p className="empty">No changes to show.</p>
      ) : (
        <pre className="diff" aria-label="Diff">
          {lines.map((line, i) => (
            <div key={i} className={diffLineClass(line)}>
              {line === "" ? " " : line}
            </div>
          ))}
        </pre>
      )}
    </div>
  );
}

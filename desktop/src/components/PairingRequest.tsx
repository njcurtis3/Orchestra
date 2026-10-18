import { useEffect, useRef, useState } from "react";
import { useModalFocus } from "../focus";
import { linkConfirmPairing, linkRejectPairing } from "../ipc";
import {
  DEFAULT_SCOPES,
  SCOPE_INFO,
  acceptErrorText,
  formatCountdown,
  isExpired,
  secondsLeft,
  toggleScope,
  truncateName,
} from "../link";
import { SCOPES, type PendingPairing, type Scope } from "../types";

type Props = {
  request: PendingPairing;
  waiting: number; // requests behind this one
  onDone: (id: string) => void;
  onError: (e: unknown) => void;
};

// The trust decision: a phone that proved the pairing code asks to be paired. The name is
// whatever the phone says, so it is only ever shown as text; the fingerprint is what to compare.
export function PairingRequest({ request, waiting, onDone, onError }: Props) {
  const [scopes, setScopes] = useState<Scope[]>(DEFAULT_SCOPES);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const dialog = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, []);

  const expired = isExpired(request.expires_at, now);
  const over = expired || failure !== null;

  async function accept() {
    if (busy || over || scopes.length === 0) return;
    setBusy(true);
    try {
      await linkConfirmPairing(request.id, scopes);
      onDone(request.id);
    } catch (e) {
      setFailure(acceptErrorText(e instanceof Error ? e.message : String(e)));
      setBusy(false);
    }
  }

  async function reject() {
    if (busy) return;
    if (over) {
      onDone(request.id);
      return;
    }
    setBusy(true);
    try {
      await linkRejectPairing(request.id);
    } catch (e) {
      // The request is gone either way: it timed out or the phone left.
      onError(e);
    }
    onDone(request.id);
  }

  // Escape refuses, never accepts. The handler is replaced on every render, so it never
  // sees a stale state.
  const escape = useRef<() => void>(() => undefined);
  escape.current = () => void reject();
  useModalFocus(dialog);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") escape.current();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div className="overlay">
      <div
        className="card dialog"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="request-h"
        aria-describedby="request-d"
        tabIndex={-1}
        ref={dialog}
      >
        <h3 id="request-h">A phone wants to pair</h3>
        <div id="request-d">
          <p>
            Name it gave: <strong className="name-ellipsis" title={request.client_name}>{truncateName(request.client_name)}</strong>
          </p>
          <p>
            Its code: <span className="mono">{request.fingerprint}</span>
          </p>
          <p className="meta">Check that your phone shows the same code. Only accept a phone you are holding.</p>
        </div>
        {waiting > 0 && <p className="meta">{waiting} more waiting.</p>}

        {!over && (
          <>
            <fieldset disabled={busy}>
              <legend>What it may do</legend>
              {SCOPES.map((s) => (
                <label className="option" key={s}>
                  <input
                    type="checkbox"
                    checked={scopes.includes(s)}
                    onChange={() => setScopes((cur) => toggleScope(cur, s))}
                  />
                  <span>
                    <strong>{SCOPE_INFO[s].label}</strong> - {SCOPE_INFO[s].detail}
                  </span>
                </label>
              ))}
            </fieldset>
            <p className="meta">This request expires in {formatCountdown(secondsLeft(request.expires_at, now))}.</p>
          </>
        )}
        {expired && failure === null && <p role="status">This request expired.</p>}
        {failure !== null && (
          <p className="hint" role="alert">
            {failure}
          </p>
        )}

        <div className="actions">
          {!over && (
            <button type="button" disabled={busy || scopes.length === 0} onClick={() => void accept()}>
              Accept
            </button>
          )}
          <button type="button" disabled={busy} onClick={() => void reject()}>
            {over ? "Close" : "Reject"}
          </button>
        </div>
      </div>
    </div>
  );
}

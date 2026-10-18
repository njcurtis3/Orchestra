import { useEffect, useRef, useState } from "react";
import { linkClosePairing, linkOpenPairing } from "../ipc";
import { formatCountdown, isExpired, loopbackOnly, makeSerial, qrDataUrl, secondsLeft } from "../link";
import type { PairingView } from "../types";

type Props = { onClose: () => void; onError: (e: unknown) => void };

// Open and close go to the host one after another, in order: React may mount this twice in
// development, and a close from the first mount must not land after the open of the second.
const serial = makeSerial();

// The QR for pairing a phone. The code is a picture only: the secret it carries is never
// available to this component as text, and the view lives in this component's state alone.
export function PairingDialog({ onClose, onError }: Props) {
  const [view, setView] = useState<PairingView | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [attempt, setAttempt] = useState(0);
  const dialog = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let live = true;
    setView(null);
    serial(linkOpenPairing)
      .then((v) => {
        if (live) setView(v);
      })
      .catch((e) => {
        if (live) {
          onError(e);
          onClose();
        }
      });
    return () => {
      live = false;
      // Closing the dialog closes the host's window, and drops the view with this state.
      void serial(linkClosePairing).catch(() => undefined);
    };
    // Reopening is a new attempt; the callbacks are the parent's and do not restart it.
  }, [attempt]);

  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, []);

  useEffect(() => {
    dialog.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const expired = view !== null && isExpired(view.expires_at, now);

  return (
    <div className="overlay">
      <div className="card dialog" role="dialog" aria-modal="true" aria-labelledby="pair-h" tabIndex={-1} ref={dialog}>
        <h3 id="pair-h">Pair a phone</h3>
        {view === null && <p className="meta">Making a code...</p>}
        {view !== null && !expired && (
          <>
            <img className="qr" src={qrDataUrl(view.qr_svg)} alt="Pairing code. Scan it with the Orchestra app on your phone." />
            <p>
              Scan this with the Orchestra app on your phone. It works once, for{" "}
              <strong>{formatCountdown(secondsLeft(view.expires_at, now))}</strong>.
            </p>
            <p className="meta">
              {view.host_name} at {view.addrs.join(", ")}
            </p>
            {loopbackOnly(view.addrs) && (
              <p className="hint">Only this computer can reach 127.0.0.1, so a phone cannot use this code yet.</p>
            )}
          </>
        )}
        {expired && (
          <>
            <p role="status">This code expired.</p>
            <div className="actions">
              <button type="button" onClick={() => setAttempt((n) => n + 1)}>
                Open again
              </button>
            </div>
          </>
        )}
        <div className="actions">
          <button type="button" onClick={onClose}>
            Close
          </button>
        </div>
      </div>
    </div>
  );
}

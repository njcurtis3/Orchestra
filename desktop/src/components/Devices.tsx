import { useCallback, useEffect, useState } from "react";
import { linkListDevices, linkRevoke, linkSetScopes, onLinkDevicesChanged } from "../ipc";
import { SCOPE_INFO, toggleScope, truncateName } from "../link";
import { SCOPES, type Device, type LinkStatus } from "../types";
import { LinkSettings } from "./LinkSettings";

type Props = {
  status: LinkStatus;
  requestWaiting: boolean;
  onStatus: (s: LinkStatus) => void;
  onPair: () => void;
  onBack: () => void;
  onError: (e: unknown) => void;
};

function pairedOn(ms: number): string {
  return new Date(ms).toLocaleDateString();
}

// Link settings, pairing, and the paired devices. Devices can be changed while the link is
// off, so a lost phone can be removed without opening a socket.
export function Devices({ status, requestWaiting, onStatus, onPair, onBack, onError }: Props) {
  const [devices, setDevices] = useState<Device[] | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

  const reload = useCallback(async () => {
    try {
      setDevices(await linkListDevices());
    } catch (e) {
      onError(e);
    }
  }, [onError]);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | null = null;
    // Subscribe first so a change during the first load is not missed.
    void (async () => {
      try {
        const u = await onLinkDevicesChanged((list) => setDevices(list), onError);
        if (cancelled) u();
        else unlisten = u;
      } catch (e) {
        onError(e);
      }
      if (!cancelled) await reload();
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [onError, reload]);

  async function run(id: string, call: () => Promise<void>) {
    if (busy) return;
    setBusy(id);
    try {
      await call();
      await reload();
    } catch (e) {
      onError(e);
    } finally {
      setBusy(null);
    }
  }

  function changeScope(d: Device, scope: (typeof SCOPES)[number]) {
    const next = toggleScope(d.scopes, scope);
    if (next.length === d.scopes.length && next.every((s, i) => s === d.scopes[i])) {
      setNote(`${truncateName(d.name)} must keep at least one thing it may do. Remove it instead.`);
      return;
    }
    setNote(null);
    void run(d.id, () => linkSetScopes(d.id, next));
  }

  const pairDisabled = !status.running || requestWaiting;

  return (
    <section className="panel devices" aria-labelledby="devices-h">
      <div className="row">
        <h2 id="devices-h">Devices</h2>
        <button type="button" onClick={onBack}>
          Back
        </button>
      </div>

      <LinkSettings status={status} onStatus={onStatus} onError={onError} />

      <section className="card" aria-labelledby="pair-section-h">
        <h3 id="pair-section-h">Pair a phone</h3>
        <p className="meta">
          {status.running
            ? "Opens a code that works once, for five minutes. You will be asked to accept the phone here."
            : "Turn the link on and save first. A phone can only pair while the link is running."}
        </p>
        <div className="actions">
          <button type="button" disabled={pairDisabled} onClick={onPair}>
            Pair a phone
          </button>
        </div>
        {requestWaiting && <p className="meta">Answer the waiting request first.</p>}
      </section>

      <section aria-labelledby="paired-h">
        <h3 id="paired-h">Paired devices</h3>
        {note && <p className="hint">{note}</p>}
        {devices === null && <p className="meta">Loading...</p>}
        {devices !== null && devices.length === 0 && <p className="empty">No phones are paired yet.</p>}
        <ul className="list">
          {(devices ?? []).map((d) => (
            <li className="card" key={d.id}>
              <div className="row">
                <strong className="name-ellipsis" title={d.name}>
                  {truncateName(d.name)}
                </strong>
                <span className="meta">paired {pairedOn(d.paired_at)}</span>
              </div>
              <p className="meta">
                Its code: <span className="mono">{d.fingerprint}</span>
              </p>
              <fieldset disabled={busy === d.id}>
                <legend>What it may do</legend>
                {SCOPES.map((s) => (
                  <label className="option" key={s} title={SCOPE_INFO[s].detail}>
                    <input type="checkbox" checked={d.scopes.includes(s)} onChange={() => changeScope(d, s)} />
                    <span>{SCOPE_INFO[s].label}</span>
                  </label>
                ))}
              </fieldset>
              {removing === d.id ? (
                <div className="card confirm" role="alertdialog" aria-labelledby={`rm-${d.id}`}>
                  <p id={`rm-${d.id}`}>
                    Remove {truncateName(d.name)}? It is disconnected now and must pair again.
                  </p>
                  <div className="actions">
                    <button
                      type="button"
                      disabled={busy === d.id}
                      onClick={() => {
                        setRemoving(null);
                        void run(d.id, () => linkRevoke(d.id));
                      }}
                    >
                      Remove
                    </button>
                    <button type="button" onClick={() => setRemoving(null)}>
                      Cancel
                    </button>
                  </div>
                </div>
              ) : (
                <div className="actions">
                  <button type="button" disabled={busy === d.id} onClick={() => setRemoving(d.id)}>
                    Remove
                  </button>
                </div>
              )}
            </li>
          ))}
        </ul>
      </section>
    </section>
  );
}

import { useEffect, useState } from "react";
import { linkInterfaces, linkSetConfig } from "../ipc";
import {
  firewallText,
  interfaceLabel,
  missingText,
  needsFirewallNotice,
  parsePort,
  sameSet,
  statusPill,
} from "../link";
import type { LinkConfigChange } from "../ipc";
import type { LinkInterface, LinkStatus } from "../types";

type Props = {
  status: LinkStatus;
  onStatus: (s: LinkStatus) => void;
  onError: (e: unknown) => void;
};

// Enable switch, the addresses to listen on, the port, and what the link is doing now.
export function LinkSettings({ status, onStatus, onError }: Props) {
  const [interfaces, setInterfaces] = useState<LinkInterface[]>([]);
  const [enabled, setEnabled] = useState(status.enabled);
  const [binds, setBinds] = useState<string[]>(status.binds);
  const [portText, setPortText] = useState(String(status.port));
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);

  const savedBinds = status.binds.join(",");
  useEffect(() => {
    setEnabled(status.enabled);
    setBinds(status.binds);
    setPortText(String(status.port));
    setFormError(null);
  }, [status.enabled, status.port, savedBinds]);

  async function loadInterfaces() {
    try {
      setInterfaces(await linkInterfaces());
    } catch (e) {
      onError(e);
    }
  }
  useEffect(() => {
    void loadInterfaces();
    // Once, when the panel opens; the Refresh button asks again.
  }, []);

  const locked = status.config_readonly !== null || !status.available;
  // Only addresses that exist now can be saved; a saved one that is gone is dropped by a save.
  const present = (list: string[]) => list.filter((b) => interfaces.some((i) => i.ip === b));
  const draftBinds = present(binds);
  const gone = binds.filter((b) => !draftBinds.includes(b) && interfaces.length > 0);
  const port = parsePort(portText);
  const dirty =
    enabled !== status.enabled ||
    !sameSet(draftBinds, present(status.binds)) ||
    portText.trim() !== String(status.port);

  async function save(change: LinkConfigChange) {
    if (busy) return;
    setBusy(true);
    setFormError(null);
    try {
      onStatus(await linkSetConfig(change));
    } catch (e) {
      onError(e);
    } finally {
      setBusy(false);
    }
  }

  function onSave() {
    if (port === null) {
      setFormError("The port must be a number from 1024 to 65535.");
      return;
    }
    if (needsFirewallNotice({ enabled, binds: draftBinds }, status.firewall_notice_seen)) {
      setNotice(true);
      return;
    }
    void save({ enabled, binds: draftBinds, port, firewall_notice_seen: status.firewall_notice_seen });
  }

  function onRetry() {
    void save({
      enabled: status.enabled,
      binds: present(status.binds),
      port: status.port,
      firewall_notice_seen: status.firewall_notice_seen,
    });
  }

  function toggleBind(ip: string) {
    setBinds((cur) => (cur.includes(ip) ? cur.filter((b) => b !== ip) : [...cur, ip]));
  }

  const pill = statusPill(status);

  return (
    <section className="card" aria-labelledby="link-settings-h">
      <h3 id="link-settings-h">Link settings</h3>
      <p className="meta">
        The link lets your phone reach this computer. It is off until you turn it on, and it only listens on the
        addresses you tick.
      </p>
      {!status.available && <p className="hint">The link needs an app data folder, and there is none.</p>}
      {status.config_readonly && (
        <p className="hint" role="alert">
          The link settings file was {status.config_readonly}, so the link is off and these settings cannot be
          changed here. The file has not been touched.
        </p>
      )}

      <label className="option">
        <input
          type="checkbox"
          checked={enabled}
          disabled={locked || busy}
          onChange={(e) => setEnabled(e.target.checked)}
        />
        <span>Turn the link on</span>
      </label>

      <fieldset disabled={locked || busy}>
        <legend>Listen on</legend>
        {interfaces.map((i) => (
          <label className="option" key={i.ip}>
            <input type="checkbox" checked={binds.includes(i.ip)} onChange={() => toggleBind(i.ip)} />
            <span>{interfaceLabel(i)}</span>
          </label>
        ))}
        {interfaces.length === 0 && <p className="meta">Looking for addresses...</p>}
        <div className="actions">
          <button type="button" onClick={() => void loadInterfaces()}>
            Refresh addresses
          </button>
        </div>
      </fieldset>

      <label>
        Port
        <input
          type="text"
          inputMode="numeric"
          className="port"
          value={portText}
          disabled={locked || busy}
          aria-invalid={port === null}
          onChange={(e) => setPortText(e.target.value)}
        />
      </label>
      <p className="meta">
        Your phone remembers the address and port from the code, so keep the port the same between runs. 1024 to
        65535.
      </p>

      {gone.map((ip) => (
        <p className="hint" key={ip}>
          {missingText(ip)} It is skipped, and a save removes it from the list.
        </p>
      ))}
      {status.missing.filter((ip) => !gone.includes(ip)).map((ip) => (
        <p className="hint" key={`m-${ip}`}>
          {missingText(ip)} The link started without it.
        </p>
      ))}
      {formError && <p className="hint">{formError}</p>}

      <div className="actions">
        <button type="button" disabled={locked || busy || !dirty} onClick={onSave}>
          Save
        </button>
      </div>

      <p className={`status-line pill-text-${pill.tone}`} role="status">
        {status.running
          ? `Listening on ${status.addrs.join(", ")}.`
          : status.enabled
            ? "The link is on but not running."
            : "The link is off."}
      </p>
      {status.error && (
        <div className="hint" role="alert">
          <span>{status.error} </span>
          <button type="button" disabled={locked || busy} onClick={onRetry}>
            Retry
          </button>
        </div>
      )}
      <p className="meta">
        The host key is kept in {status.os === "windows" ? "Windows Credential Manager" : "a private file"}. If it is
        deleted, every paired phone refuses this computer as a changed host until it is paired again.
      </p>

      {notice && (
        <div className="card confirm" role="alertdialog" aria-labelledby="firewall-h">
          <h4 id="firewall-h">Before the link goes beyond this computer</h4>
          <p>{firewallText(status.os, port ?? status.port)}</p>
          <div className="actions">
            <button
              type="button"
              disabled={busy}
              onClick={() => {
                setNotice(false);
                if (port !== null) void save({ enabled, binds: draftBinds, port, firewall_notice_seen: true });
              }}
            >
              Continue
            </button>
            <button type="button" disabled={busy} onClick={() => setNotice(false)}>
              Cancel
            </button>
          </div>
        </div>
      )}
    </section>
  );
}

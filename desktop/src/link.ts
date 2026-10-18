// Pure logic for the phone link's screens, kept out of the components so it can be tested
// without a DOM: scopes, the firewall step, labels, countdowns and the view state.
import type { LinkInterface, LinkOs, LinkStatus, PendingPairing, Scope } from "./types";
import { SCOPES } from "./types";

export const LOOPBACK = "127.0.0.1";
export const MIN_PORT = 1024;
export const MAX_PORT = 65535;
export const DEFAULT_PORT = 47800;
// Shown names are cut here; the full text stays in the title attribute.
export const NAME_DISPLAY_CHARS = 64;

// ---- scopes ----

export const DEFAULT_SCOPES: Scope[] = ["read", "answer"];

export const SCOPE_INFO: Record<Scope, { label: string; detail: string }> = {
  read: { label: "Read", detail: "See sessions, tasks, the inbox and transcripts. Needed for the phone to show status." },
  answer: { label: "Answer", detail: "Answer permission prompts, questions and replies." },
  control: { label: "Control", detail: "Start and stop sessions, create and cancel tasks." },
  land: { label: "Land", detail: "Land or discard a finished task." },
};

// Flip one scope. A device must keep at least one, so removing the last one changes nothing.
// The result is always in the fixed order of SCOPES.
export function toggleScope(scopes: readonly Scope[], scope: Scope): Scope[] {
  const has = scopes.includes(scope);
  if (has && scopes.length === 1) return [...scopes];
  const next = has ? scopes.filter((s) => s !== scope) : [...scopes, scope];
  return SCOPES.filter((s) => next.includes(s));
}

// ---- the firewall step ----

// A first enable that exposes the link beyond this computer shows the notice before saving.
export function needsFirewallNotice(change: { enabled: boolean; binds: readonly string[] }, noticeSeen: boolean): boolean {
  return change.enabled && !noticeSeen && change.binds.some((b) => b !== LOOPBACK);
}

export function firewallText(os: LinkOs, port: number): string {
  switch (os) {
    case "windows":
      return (
        "Windows may ask whether Orchestra can use the network. Tick Private networks and choose Allow. " +
        "If you choose Cancel, Windows adds a rule that blocks Orchestra, and your phone cannot connect " +
        "until you allow it under Windows Security > Firewall & network protection > Allow an app through firewall."
      );
    case "macos":
      return "macOS may ask whether Orchestra can accept incoming connections. Choose Allow.";
    case "linux":
      return `If a firewall such as ufw is on, allow TCP port ${port}.`;
  }
}

// ---- addresses and the port ----

export function interfaceLabel(i: LinkInterface): string {
  switch (i.kind) {
    case "loopback":
      return `${i.ip} - this computer only`;
    case "tailscale":
      return `${i.ip} - Tailscale`;
    case "lan":
      return `${i.ip} - local network (${i.name})`;
    case "other":
      return `${i.ip} - other (${i.name})`;
  }
}

// The port as typed, or null when it is not a whole number in range.
export function parsePort(text: string): number | null {
  if (!/^\d{1,5}$/.test(text.trim())) return null;
  const n = Number(text);
  return n >= MIN_PORT && n <= MAX_PORT ? n : null;
}

export function sameSet(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((x) => b.includes(x));
}

export function missingText(ip: string): string {
  return `${ip} is not on this computer right now.`;
}

export function loopbackOnly(addrs: readonly string[]): boolean {
  return addrs.length > 0 && addrs.every((a) => a.startsWith(`${LOOPBACK}:`));
}

// ---- the header pill ----

export type PillTone = "off" | "on" | "error";

export function statusPill(s: LinkStatus | null): { text: string; tone: PillTone } {
  if (!s) return { text: "Link", tone: "off" };
  if (!s.available) return { text: "Link unavailable", tone: "error" };
  if (s.error) return { text: "Link error", tone: "error" };
  if (s.running) {
    const n = s.addrs.length;
    return { text: `Link on - ${n} ${n === 1 ? "address" : "addresses"}`, tone: "on" };
  }
  return { text: "Link off", tone: "off" };
}

// ---- time ----

export function secondsLeft(expiresAt: number, now: number): number {
  return Math.max(0, Math.ceil((expiresAt - now) / 1000));
}

export function isExpired(expiresAt: number, now: number): boolean {
  return now >= expiresAt;
}

// 299 -> "4:59".
export function formatCountdown(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

// ---- names ----

// Cut by characters, not UTF-16 units, so a surrogate pair is never split.
export function truncateName(name: string, max: number = NAME_DISPLAY_CHARS): string {
  const chars = Array.from(name);
  return chars.length <= max ? name : `${chars.slice(0, max - 1).join("")}…`;
}

// ---- accepting a request ----

// What the host's error becomes on screen. Core wording is matched, the rest passes through.
export function acceptErrorText(message: string): string {
  const m = message.toLowerCase();
  if (m.includes("no such pairing request")) return "Your phone stopped waiting. Open pairing again.";
  if (m.includes("too many paired clients")) return "32 devices are paired. Remove one first.";
  if (m.includes("already paired")) return "This device is already paired.";
  if (m.includes("not running")) return "The link is not running.";
  return message || "Could not pair this device.";
}

// ---- the QR ----

// The SVG as an <img> source. An <img> needs no inline style and no innerHTML, and the page's
// CSP already allows data: images.
export function qrDataUrl(svg: string): string {
  return `data:image/svg+xml,${encodeURIComponent(svg)}`;
}

// ---- calls that must not overlap ----

// Run async calls one after another, in the order they were made. The pairing dialog opens
// and closes the host's window; React may mount, unmount and mount it again at once.
export function makeSerial(): <T>(f: () => Promise<T>) => Promise<T> {
  let tail: Promise<unknown> = Promise.resolve();
  return <T>(f: () => Promise<T>) => {
    const run = tail.then(f, f);
    tail = run.catch(() => undefined);
    return run;
  };
}

// ---- the view state ----

export type LinkView = "work" | "devices";

export type LinkUi = {
  view: LinkView;
  status: LinkStatus | null;
  requests: PendingPairing[]; // oldest first
  pairing: boolean; // the QR dialog is open
};

export const initialLinkUi: LinkUi = { view: "work", status: null, requests: [], pairing: false };

export type LinkAction =
  | { type: "show"; view: LinkView }
  | { type: "status"; status: LinkStatus }
  | { type: "request"; request: PendingPairing }
  | { type: "requests-loaded"; requests: PendingPairing[] }
  | { type: "answered"; id: string }
  | { type: "pairing"; open: boolean };

const MAX_REQUESTS = 16;

function addRequests(current: PendingPairing[], more: PendingPairing[]): PendingPairing[] {
  const next = [...current];
  for (const r of more) {
    if (!next.some((x) => x.id === r.id)) next.push(r);
  }
  return next.slice(-MAX_REQUESTS);
}

export function linkReducer(state: LinkUi, action: LinkAction): LinkUi {
  switch (action.type) {
    case "show":
      return { ...state, view: action.view, pairing: action.view === "devices" ? state.pairing : false };
    case "status": {
      // A link that stopped has no request left to answer.
      const requests = action.status.running ? state.requests : [];
      return { ...state, status: action.status, requests, pairing: action.status.running ? state.pairing : false };
    }
    case "request":
      // The pairing window is single-use and a request consumed it: the QR is done.
      return { ...state, requests: addRequests(state.requests, [action.request]), pairing: false };
    case "requests-loaded":
      return { ...state, requests: addRequests(state.requests, action.requests) };
    case "answered":
      return { ...state, requests: state.requests.filter((r) => r.id !== action.id) };
    case "pairing":
      return { ...state, pairing: action.open && state.requests.length === 0 };
  }
}

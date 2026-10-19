// Parse and validate the pairing QR text. Strict on what the phone needs, tolerant of extra
// fields. Never throws; every refusal is a tagged result the UI maps to a copy key.
import { fromHex } from './bytes';
import { hostId } from './keys';

export type PairingOffer = {
  v: 1;
  host_id: string;
  host_name: string;
  host_key: string;
  secret: string;
  addrs: string[];
  expires_at: number;
};

export type OfferResult =
  | { kind: 'ok'; offer: PairingOffer }
  | { kind: 'bad_code' }
  | { kind: 'newer_code' }
  | { kind: 'expired_code' }
  | { kind: 'loopback_only' };

// Slack for clock drift between the desktop and the phone.
export const EXPIRY_SLACK_MS = 60_000;

const MAX_ADDRS = 8;
const MAX_NAME_BYTES = 128;
const HEX_32 = /^[0-9a-f]{32}$/;
const HEX_64 = /^[0-9a-f]{64}$/;

function parseAddr(addr: unknown): { text: string; loopback: boolean } | null {
  if (typeof addr !== 'string') return null;
  const m = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3}):(\d{1,5})$/.exec(addr);
  if (!m) return null;
  const octets = m.slice(1, 5).map(Number);
  const port = Number(m[5]);
  if (octets.some((o) => o > 255) || port < 1 || port > 65535) return null;
  return { text: addr, loopback: octets[0] === 127 };
}

// Same rule as the host: control and bidi-format characters dropped, trimmed, at most
// MAX_NAME_BYTES of UTF-8 cut on a character boundary.
function clipName(name: string): string {
  const cleaned = name
    .replace(/[\p{Cc}\u061C\u200E\u200F\u202A-\u202E\u2066-\u2069]/gu, '')
    .trim();
  const enc = new TextEncoder();
  let out = '';
  let bytes = 0;
  for (const ch of cleaned) {
    bytes += enc.encode(ch).length;
    if (bytes > MAX_NAME_BYTES) break;
    out += ch;
  }
  return out.trimEnd();
}

export function parseOffer(text: string, now: number): OfferResult {
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    return { kind: 'bad_code' };
  }
  if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) return { kind: 'bad_code' };
  const o = raw as Record<string, unknown>;

  if (typeof o.v !== 'number' || !Number.isInteger(o.v)) return { kind: 'bad_code' };
  if (o.v > 1) return { kind: 'newer_code' };
  if (o.v !== 1) return { kind: 'bad_code' };

  const { host_id, host_key, secret, host_name, addrs, expires_at } = o;
  if (typeof host_id !== 'string' || !HEX_32.test(host_id)) return { kind: 'bad_code' };
  if (typeof host_key !== 'string' || !HEX_64.test(host_key)) return { kind: 'bad_code' };
  if (typeof secret !== 'string' || !HEX_64.test(secret)) return { kind: 'bad_code' };
  if (typeof expires_at !== 'number' || !Number.isFinite(expires_at)) return { kind: 'bad_code' };
  if (!Array.isArray(addrs) || addrs.length === 0) return { kind: 'bad_code' };

  let id: string;
  try {
    id = hostId(fromHex(host_key));
  } catch {
    return { kind: 'bad_code' };
  }
  if (id !== host_id) return { kind: 'bad_code' };

  const parsed = addrs.map(parseAddr);
  if (parsed.some((a) => a === null)) return { kind: 'bad_code' };
  const usable = (parsed as { text: string; loopback: boolean }[])
    .filter((a) => !a.loopback)
    .map((a) => a.text)
    .slice(0, MAX_ADDRS);
  if (usable.length === 0) return { kind: 'loopback_only' };

  if (now > expires_at + EXPIRY_SLACK_MS) return { kind: 'expired_code' };

  return {
    kind: 'ok',
    offer: {
      v: 1,
      host_id,
      host_name: typeof host_name === 'string' ? clipName(host_name) : '',
      host_key,
      secret,
      addrs: usable,
      expires_at,
    },
  };
}

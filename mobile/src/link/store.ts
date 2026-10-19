// Paired hosts in expo-secure-store: an index of host ids plus one record per host, each
// with its own client key. Key names use only A-Z a-z 0-9 . _ - (the SecureStore rule).
import * as SecureStore from 'expo-secure-store';

export type HostRecord = {
  schema: 1;
  host_id: string;
  host_name: string;
  host_key: string;
  addrs: string[];
  client_secret_key: string;
  client_public_key: string;
  fingerprint: string;
  paired_at: number;
};

export const INDEX_KEY = 'orchestra.hosts';
export const MAX_VALUE_BYTES = 2048;
export const MAX_ADDRS = 8;

export function hostKey(hostId: string): string {
  return `orchestra.host.${hostId}`;
}

function assertSize(key: string, value: string): void {
  const size = new TextEncoder().encode(value).length;
  if (size >= MAX_VALUE_BYTES) throw new Error(`value for ${key} is ${size} bytes, over the limit`);
}

async function write(key: string, value: string): Promise<void> {
  assertSize(key, value);
  await SecureStore.setItemAsync(key, value);
}

// A missing or unparseable value is absent. With `strict` false a failed read (a keystore
// that cannot decrypt, a bad key name) is absent too: the host shows as unpaired and can be
// paired again, which makes a new key (the old one was unreadable anyway). A read that feeds
// a write is strict, so a transient failure cannot make the index look empty and be written
// back over it, which would orphan every host's secret.
async function readJson(key: string, strict = false): Promise<unknown> {
  let text: string | null;
  try {
    text = await SecureStore.getItemAsync(key);
  } catch (e) {
    if (strict) throw e;
    return undefined;
  }
  if (text === null) return undefined;
  try {
    return JSON.parse(text);
  } catch {
    return undefined;
  }
}

async function readIndex(strict = false): Promise<string[]> {
  const raw = await readJson(INDEX_KEY, strict);
  return Array.isArray(raw) ? raw.filter((x): x is string => typeof x === 'string') : [];
}

function isRecord(raw: unknown, hostId: string): raw is HostRecord {
  if (typeof raw !== 'object' || raw === null) return false;
  const r = raw as Record<string, unknown>;
  return (
    r.schema === 1 &&
    r.host_id === hostId &&
    typeof r.host_name === 'string' &&
    typeof r.host_key === 'string' &&
    Array.isArray(r.addrs) &&
    typeof r.client_secret_key === 'string' &&
    typeof r.client_public_key === 'string' &&
    typeof r.fingerprint === 'string' &&
    typeof r.paired_at === 'number'
  );
}

export async function getHost(hostId: string): Promise<HostRecord | null> {
  const raw = await readJson(hostKey(hostId));
  return isRecord(raw, hostId) ? raw : null;
}

// Only hosts that are in the index and have a readable record; a record with no index
// entry is ignored.
export async function listHosts(): Promise<HostRecord[]> {
  const out: HostRecord[] = [];
  for (const id of await readIndex()) {
    const rec = await getHost(id);
    if (rec) out.push(rec);
  }
  return out;
}

// Index updates are read-modify-write; run them one at a time.
let queue: Promise<unknown> = Promise.resolve();
function serialized<T>(task: () => Promise<T>): Promise<T> {
  const run = queue.then(task, task);
  queue = run.catch(() => undefined);
  return run;
}

// The index entry goes in before the record, so a crash between the two leaves an id with no
// record (skipped by listHosts, filled by the next save) and never a record with no id that
// nothing would delete. The client secret is written last.
export function saveHost(record: HostRecord): Promise<void> {
  return serialized(async () => {
    const rec = { ...record, addrs: record.addrs.slice(0, MAX_ADDRS) };
    const body = JSON.stringify(rec);
    assertSize(hostKey(rec.host_id), body); // throws before anything is written
    const index = await readIndex(true);
    if (!index.includes(rec.host_id)) await write(INDEX_KEY, JSON.stringify([...index, rec.host_id]));
    await write(hostKey(rec.host_id), body);
  });
}

// Delete both the record and its index entry.
export function forgetHost(hostId: string): Promise<void> {
  return serialized(async () => {
    const index = await readIndex(true);
    await SecureStore.deleteItemAsync(hostKey(hostId));
    if (index.includes(hostId)) {
      await write(INDEX_KEY, JSON.stringify(index.filter((id) => id !== hostId)));
    }
  });
}

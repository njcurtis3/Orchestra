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

async function write(key: string, value: string): Promise<void> {
  const size = new TextEncoder().encode(value).length;
  if (size >= MAX_VALUE_BYTES) throw new Error(`value for ${key} is ${size} bytes, over the limit`);
  await SecureStore.setItemAsync(key, value);
}

// A missing or unparseable value is absent.
async function readJson(key: string): Promise<unknown> {
  const text = await SecureStore.getItemAsync(key);
  if (text === null) return undefined;
  try {
    return JSON.parse(text);
  } catch {
    return undefined;
  }
}

async function readIndex(): Promise<string[]> {
  const raw = await readJson(INDEX_KEY);
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

// Record first, then the index.
export async function saveHost(record: HostRecord): Promise<void> {
  const rec = { ...record, addrs: record.addrs.slice(0, MAX_ADDRS) };
  await write(hostKey(rec.host_id), JSON.stringify(rec));
  const index = await readIndex();
  if (!index.includes(rec.host_id)) await write(INDEX_KEY, JSON.stringify([...index, rec.host_id]));
}

// Delete both the record and its index entry.
export async function forgetHost(hostId: string): Promise<void> {
  await SecureStore.deleteItemAsync(hostKey(hostId));
  const index = await readIndex();
  if (index.includes(hostId)) {
    await write(INDEX_KEY, JSON.stringify(index.filter((id) => id !== hostId)));
  }
}

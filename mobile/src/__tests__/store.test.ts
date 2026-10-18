import * as SecureStore from 'expo-secure-store';

import {
  forgetHost,
  getHost,
  hostKey,
  INDEX_KEY,
  listHosts,
  saveHost,
  type HostRecord,
} from '../link/store';

jest.mock('expo-secure-store', () => {
  const data = new Map<string, string>();
  return {
    __data: data,
    getItemAsync: jest.fn(async (k: string) => data.get(k) ?? null),
    setItemAsync: jest.fn(async (k: string, v: string) => {
      data.set(k, v);
    }),
    deleteItemAsync: jest.fn(async (k: string) => {
      data.delete(k);
    }),
  };
});

const data = (SecureStore as unknown as { __data: Map<string, string> }).__data;

const record = (id: string): HostRecord => ({
  schema: 1,
  host_id: id,
  host_name: 'Desk',
  host_key: 'a'.repeat(64),
  addrs: ['192.168.1.20:47800'],
  client_secret_key: 'b'.repeat(64),
  client_public_key: 'c'.repeat(64),
  fingerprint: '1234-5678-9abc-def0',
  paired_at: 1,
});

const ID1 = '1'.repeat(32);
const ID2 = '2'.repeat(32);

beforeEach(() => {
  data.clear();
  jest.clearAllMocks();
});

describe('host store', () => {
  it('round trips a record through the index', async () => {
    await saveHost(record(ID1));
    expect(await getHost(ID1)).toEqual(record(ID1));
    expect(await listHosts()).toEqual([record(ID1)]);
    expect(JSON.parse(data.get(INDEX_KEY)!)).toEqual([ID1]);
  });

  it('does not duplicate the index entry on a second save', async () => {
    await saveHost(record(ID1));
    await saveHost({ ...record(ID1), host_name: 'Renamed' });
    expect(JSON.parse(data.get(INDEX_KEY)!)).toEqual([ID1]);
    expect((await getHost(ID1))!.host_name).toBe('Renamed');
  });

  it('writes the record before the index', async () => {
    await saveHost(record(ID1));
    const keys = (SecureStore.setItemAsync as jest.Mock).mock.calls.map((c) => c[0]);
    expect(keys).toEqual([hostKey(ID1), INDEX_KEY]);
  });

  it('forget removes both keys and leaves other hosts', async () => {
    await saveHost(record(ID1));
    await saveHost(record(ID2));
    await forgetHost(ID1);
    expect(data.has(hostKey(ID1))).toBe(false);
    expect(JSON.parse(data.get(INDEX_KEY)!)).toEqual([ID2]);
    expect(await getHost(ID1)).toBeNull();
    expect((await listHosts()).map((h) => h.host_id)).toEqual([ID2]);
  });

  it('forgetting the only host leaves an empty index', async () => {
    await saveHost(record(ID1));
    await forgetHost(ID1);
    expect(data.has(hostKey(ID1))).toBe(false);
    expect(JSON.parse(data.get(INDEX_KEY)!)).toEqual([]);
  });

  it('uses only A-Z a-z 0-9 . _ - in key names', async () => {
    await saveHost(record(ID1));
    expect(INDEX_KEY).toMatch(/^[A-Za-z0-9._-]+$/);
    for (const k of data.keys()) expect(k).toMatch(/^[A-Za-z0-9._-]+$/);
  });

  it('refuses an oversize value before calling setItemAsync', async () => {
    await expect(saveHost({ ...record(ID1), host_name: 'x'.repeat(2100) })).rejects.toThrow(
      /over the limit/,
    );
    expect(SecureStore.setItemAsync).not.toHaveBeenCalled();
  });

  it('keeps at most 8 addresses', async () => {
    const addrs = Array.from({ length: 12 }, (_, i) => `10.0.0.${i + 1}:47800`);
    await saveHost({ ...record(ID1), addrs });
    expect((await getHost(ID1))!.addrs).toHaveLength(8);
  });

  it('treats unparseable or mismatched values as absent', async () => {
    data.set(INDEX_KEY, '{not json');
    expect(await listHosts()).toEqual([]);
    data.set(hostKey(ID1), '{not json');
    expect(await getHost(ID1)).toBeNull();
    data.set(hostKey(ID1), JSON.stringify(record(ID2)));
    expect(await getHost(ID1)).toBeNull();
  });

  it('ignores a record with no index entry', async () => {
    data.set(hostKey(ID1), JSON.stringify(record(ID1)));
    expect(await listHosts()).toEqual([]);
  });
});

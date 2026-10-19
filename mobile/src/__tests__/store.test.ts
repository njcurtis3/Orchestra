import * as ExpoCrypto from 'expo-crypto';
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
import { random } from '../link/random';

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
const ID3 = '3'.repeat(32);

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

  it('writes the index entry before the record', async () => {
    await saveHost(record(ID1));
    const keys = (SecureStore.setItemAsync as jest.Mock).mock.calls.map((c) => c[0]);
    expect(keys).toEqual([INDEX_KEY, hostKey(ID1)]);
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

  it('serializes concurrent saves and forgets so the index keeps every host', async () => {
    await Promise.all([saveHost(record(ID1)), saveHost(record(ID2)), forgetHost(ID1)]);
    expect(JSON.parse(data.get(INDEX_KEY)!)).toEqual([ID2]);
    await Promise.all([saveHost(record(ID1)), forgetHost(ID2), saveHost(record(ID3))]);
    expect(JSON.parse(data.get(INDEX_KEY)!)).toEqual([ID1, ID3]);
  });

  it('a crash after the index write leaves an id with no record, which the next save fills', async () => {
    (SecureStore.setItemAsync as jest.Mock).mockImplementationOnce(async (k: string, v: string) => {
      data.set(k, v);
    });
    (SecureStore.setItemAsync as jest.Mock).mockImplementationOnce(async () => {
      throw new Error('crash');
    });
    await expect(saveHost(record(ID1))).rejects.toThrow('crash');
    expect(data.has(hostKey(ID1))).toBe(false);
    expect(await listHosts()).toEqual([]);
    await saveHost(record(ID1));
    expect((await listHosts()).map((h) => h.host_id)).toEqual([ID1]);
    expect(JSON.parse(data.get(INDEX_KEY)!)).toEqual([ID1]);
  });

  it('treats a throwing read as absent, but never writes the index back after a failed read', async () => {
    await saveHost(record(ID1));
    (SecureStore.getItemAsync as jest.Mock).mockRejectedValueOnce(new Error('keystore'));
    expect(await getHost(ID1)).toBeNull();
    (SecureStore.getItemAsync as jest.Mock).mockRejectedValueOnce(new Error('keystore'));
    await expect(saveHost(record(ID2))).rejects.toThrow('keystore');
    expect(JSON.parse(data.get(INDEX_KEY)!)).toEqual([ID1]);
  });
});

describe('random', () => {
  afterEach(() => jest.restoreAllMocks());

  it('draws from getRandomValues and never from Math.random, even in a dev bundle', () => {
    const g = globalThis as { __DEV__?: boolean; nativeCallSyncHook?: unknown };
    const dev = g.__DEV__;
    const hook = g.nativeCallSyncHook;
    g.__DEV__ = true;
    delete g.nativeCallSyncHook;
    const math = jest.spyOn(Math, 'random');
    const values = jest.spyOn(ExpoCrypto, 'getRandomValues');
    let bytes: Uint8Array;
    try {
      bytes = random(32);
    } finally {
      g.__DEV__ = dev;
      g.nativeCallSyncHook = hook;
    }
    expect(bytes).toBeInstanceOf(Uint8Array);
    expect(bytes.length).toBe(32);
    expect(values).toHaveBeenCalledTimes(1);
    expect(math).not.toHaveBeenCalled();
  });

  it('throws rather than return the wrong shape', () => {
    jest.spyOn(ExpoCrypto, 'getRandomValues').mockReturnValue(new Uint8Array(3) as never);
    expect(() => random(32)).toThrow(/secure random/);
  });
});

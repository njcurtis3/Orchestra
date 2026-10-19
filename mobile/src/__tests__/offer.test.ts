import { parseOffer } from '../link/offer';

declare const __dirname: string;
const fs = require('fs') as { readFileSync: (path: string, encoding: 'utf8') => string };
const file = JSON.parse(
  fs.readFileSync(`${__dirname}/../../../protocol/fixtures/noise/offer.json`, 'utf8'),
);
const base = JSON.parse(file.json);
const NOW = 1_799_999_000_000;
const parse = (o: unknown, now = NOW) => parseOffer(JSON.stringify(o), now);

describe('parseOffer', () => {
  it('accepts the fixture with exactly one usable address', () => {
    const r = parseOffer(file.json, NOW);
    expect(r.kind).toBe('ok');
    if (r.kind === 'ok') {
      expect(r.offer.addrs).toEqual(['192.168.1.20:47800']);
      expect(r.offer.host_id).toBe(base.host_id);
      expect(r.offer.expires_at).toBe(1800000000000);
    }
  });

  it('tolerates extra fields', () => {
    expect(parse({ ...base, extra: [1, 2] }).kind).toBe('ok');
  });

  it('rejects text that is not JSON or not an object', () => {
    expect(parseOffer('not json', NOW).kind).toBe('bad_code');
    expect(parseOffer('[1]', NOW).kind).toBe('bad_code');
    expect(parseOffer('null', NOW).kind).toBe('bad_code');
    expect(parseOffer('https://example.com', NOW).kind).toBe('bad_code');
  });

  it('maps a newer version to newer_code and other versions to bad_code', () => {
    expect(parse({ ...base, v: 2 }).kind).toBe('newer_code');
    expect(parse({ ...base, v: 0 }).kind).toBe('bad_code');
    expect(parse({ ...base, v: '1' }).kind).toBe('bad_code');
    expect(parse({ ...base, v: 1.5 }).kind).toBe('bad_code');
  });

  it('rejects a host_id that does not match the host key', () => {
    expect(parse({ ...base, host_id: '0'.repeat(32) }).kind).toBe('bad_code');
  });

  it('rejects short, long, uppercase or missing hex', () => {
    expect(parse({ ...base, host_key: base.host_key.slice(2) }).kind).toBe('bad_code');
    expect(parse({ ...base, secret: base.secret + '00' }).kind).toBe('bad_code');
    expect(parse({ ...base, secret: base.secret.toUpperCase() }).kind).toBe('bad_code');
    expect(parse({ ...base, host_id: base.host_id.slice(1) }).kind).toBe('bad_code');
    const noSecret = { ...base };
    delete noSecret.secret;
    expect(parse(noSecret).kind).toBe('bad_code');
  });

  it('rejects empty, malformed or non-IPv4 addresses', () => {
    expect(parse({ ...base, addrs: [] }).kind).toBe('bad_code');
    expect(parse({ ...base, addrs: 'x' }).kind).toBe('bad_code');
    expect(parse({ ...base, addrs: ['192.168.1.20'] }).kind).toBe('bad_code');
    expect(parse({ ...base, addrs: ['999.1.1.1:80'] }).kind).toBe('bad_code');
    expect(parse({ ...base, addrs: ['[::1]:47800'] }).kind).toBe('bad_code');
    expect(parse({ ...base, addrs: ['192.168.1.20:0'] }).kind).toBe('bad_code');
  });

  it('reports loopback-only addresses', () => {
    expect(parse({ ...base, addrs: ['127.0.0.1:47800', '127.0.0.2:1'] }).kind).toBe(
      'loopback_only',
    );
  });

  it('expires 60 s after expires_at', () => {
    expect(parse(base, base.expires_at + 60_000).kind).toBe('ok');
    expect(parse(base, base.expires_at + 60_001).kind).toBe('expired_code');
  });

  it('rejects a non-numeric expires_at', () => {
    expect(parse({ ...base, expires_at: '1800000000000' }).kind).toBe('bad_code');
  });

  it('clips host_name to 128 bytes on a character boundary and drops control characters', () => {
    const e = '\u00e9';
    const r = parse({ ...base, host_name: '\u0007' + e.repeat(100) });
    expect(r.kind).toBe('ok');
    if (r.kind === 'ok') {
      expect(new TextEncoder().encode(r.offer.host_name).length).toBe(128);
      expect(r.offer.host_name).toBe(e.repeat(64));
    }
  });
});

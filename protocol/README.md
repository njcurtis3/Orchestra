# protocol/ — the client <-> host contract

The contract between a **host**, which runs the agents, and the **clients** that watch and
steer them. Both sides are built from this definition. This file is a decision record in
prose: each section says what was chosen and what would reopen it. The wire types are code
beside it: `types.ts`, `constants.ts` and `codec.ts` define protocol v1, and `fixtures/` holds
the JSON cases both copies are tested against.

- **One protocol, versioned here.** Every message carries the protocol version.
- **A protocol change ships in both clients together.** A change that one client cannot
  speak is not merged until the other can. Desktop and mobile release on separate channels
  (installers vs. the Play Store), so the older client must fail loudly on a version it
  does not understand, never guess.
- **The TypeScript definitions here are the source of truth.** The Rust copy is
  `orchestra_core::wire` (`core/src/wire/`) and is checked against them by tests that run the
  same `fixtures/*.json`.

## Scope

A **host** is the desktop app, or `orchestrad` (H2) on a headless server. Both run
`orchestra-core`. A **client** is the phone, or the desktop app attached to a remote host
(H3). One protocol serves both. The link is **off by default** on every host.

## Reachability

**Decided: A.** A client connects directly to an address the host binds: loopback, a LAN
interface, or a private overlay the user already runs (Tailscale or WireGuard). Away from
home means the overlay. The host binds only the interfaces named in its config or UI, never
`0.0.0.0` by default. A rented bare-metal box is reached over the user's tailnet. There is
no open public port and no server of ours.

Rejected for v1: B, a public TLS endpoint with pinned keys, because it needs an open port. C, an
SSH tunnel, because the phone has no maintained SSH client on Expo.

**Reopened if** the owner will not require an overlay for away-from-home use. Then a relay
(an ops item, a server of ours) or B behind an explicit opt-in.

## Transport

**Decided: A.** A WebSocket (`ws://`) served by `orchestra-core`, carrying a Noise protocol
channel: static X25519 keys, ChaChaPoly AEAD per frame. Rust uses the `snow` crate. The
phone uses a JS Noise implementation on `@noble` primitives, checked against the Noise test
vectors.

Why: every frame is authenticated and encrypted by construction, both ends know each other's
static key, nonces give replay and reorder protection, and it needs no CA, no certificate
pinning inside React Native's OkHttp (B's weak point), and no server of ours (C). When the
desktop is the client (H3), the Rust process owns the socket and the keys, so the webview
never holds a key and CSP `connect-src` needs no new origin for the client role.

Rejected: B, WSS with a self-signed host cert pinned at pairing plus client certs (mutual
TLS). C, a LAN WebSocket plus a relay we run for away-from-home.

**Reopened if** no maintained JS Noise implementation passes the test vectors on Hermes in
P2. Then libsodium (`crypto_kx` + `secretstream`, via `react-native-libsodium`) under the
same message layer. Or if a maintained Expo module makes pinned mutual TLS straightforward,
then B.

## Pairing

**Decided: A, with confirmation on the host.** The host (desktop window, or the `orchestrad`
terminal for H2) shows a QR code carrying `{v, host_id, host name, addresses + port, host
static public key, 128-bit one-time secret, expiry}`. The handshake is Noise `IKpsk2` with
the secret as the psk; the client sends its own static key inside it. The host then shows the
client name and key fingerprint, and the user **confirms on the host** before the client is
stored.

- The pairing window opens only on user action, lasts 5 minutes, and is single use.
- Later connections use Noise `KK` (both static keys known). An unknown client key is closed
  with no information.
- Phone: the keypair is generated on the device; the private key lives only in
  `expo-secure-store`. Desktop as client: the OS keyring.
- Host private key: the OS keyring (desktop) or a `0600` file in the `orchestrad` state
  directory (H2).
- The host key is pinned at pairing. A changed host key is refused loudly. (This was an H3
  item; it is now protocol-wide.)

Rejected: B, a short typed code with a PAKE (SPAKE2) instead of a QR. C, trust on first use
with no secret.

**Reopened if** a host cannot show a QR (no terminal QR, no screen). Then add B as a second
pairing mode.

## Authorisation

**Decided:** per-client scopes, stored on the host with the client key.

| Scope | Allows |
|---|---|
| `read` | snapshot, events, `get_transcript`, `list_*`, `get_task_diff`, `storage_status` |
| `answer` | `answer` (permission, question, reply) |
| `control` | `start_session`, `stop_session`, `create_task`, `cancel_task`, `retry_cleanup` |
| `land` | `land_task`, `discard_task` |

The phone's default is `read` + `answer`. `control` and `land` are granted per client, on the
host. `pick_project` is host-local only (it opens a native dialog) and is never remote. A call
outside the client's scope returns `error{code: forbidden}` and is logged on the host: the
method and the client, never the payload.

**Reopened if** the owner wants one trust level per client (a simpler UI). Then two levels:
`viewer` (read + answer) and `operator` (everything).

## Revocation

**Decided:** revoking a client deletes its key on the host.

- An open connection receives `error{code: revoked}` and is closed; its queued calls are
  dropped.
- Any later handshake with that key fails at the Noise layer and the socket closes with no
  message, indistinguishable from an unknown client.
- On `revoked` the client shows "Removed from <host>. Pair again." After a failed handshake
  it shows "Not paired with this host". On either, it deletes its cached snapshot and its
  keys for that host.

**Reopened if:** no reversal condition was recorded at the gate. Any change to the revoked
or unknown-key behaviour reopens this section.

## Push

**Decided: A, no real push in v1.** Notifications arrive only while the phone is connected
(app open or recently backgrounded). A `register_push{token, platform}` call and a `push`
feature flag in `welcome` are **reserved**, so A3 can add C or B without a v2.

Not chosen: B, FCM through a relay we run (a server of ours, an ops item). C, FCM sent
directly by the host with the user's own Firebase credentials (a content-free wake-up, then
the phone connects). C does not scale past the owner, because the app's FCM sender key would
have to sit on every host.

**Reopened if** the owner wants asleep-phone alerts before A3. Then C for the owner's own
host; B only with an ops plan for the relay.

## Messages

Every message is JSON `{v: 1, type, ...}`.

**Envelope and version handshake.** Before the Noise handshake the client sends a plaintext
`{proto: "orchestra", v}`. The host answers `{proto, v, supported: [1]}`, or
`error{code: unsupported_version, supported}` and closes. Both plaintext frames are bound into
the Noise prologue, so they cannot be downgraded. Inside the channel every message still
carries `v`. An unknown `v` is a loud failure (error, then close), never a guess. An unknown
`type` at a known `v` gives `error{code: unknown_type}`, not a crash.

**Client to host.**
- `hello{v, client_name, app_version}`, sent after the handshake.
- `call{v, id, method, args}`, where `method` is a Tauri command name verbatim:
  `list_projects`, `list_sessions`, `list_inbox`, `get_transcript`, `list_tasks`,
  `get_task_diff`, `storage_status`, `start_session`, `stop_session`, `answer`, `create_task`,
  `cancel_task`, `land_task`, `discard_task`, `retry_cleanup`. `args` are the same serde
  structs, with `deny_unknown_fields`.
- `ping`.

**Host to client.**
- `welcome{v, host_id, host_name, scopes, features}`.
- `snapshot{v, seq, projects, sessions, inbox, tasks, storage}` on every connect. v1 does no
  replay.
- `event{v, seq, name, payload}`, where `name` is a Tauri event name verbatim:
  `session-updated`, `inbox-updated`, `transcript-appended`, `task-updated`. Plus `notice`,
  the core's notification decision, so `orchestrad` can forward notifications (H2/A3).
- `result{v, id, ok | error}`.
- `error{v, code, message}`.
- `pong`.

**Mirror rule.** The remote surface is the local one: the same method names, the same event
names, the same payload types as `desktop/src/ipc.ts` and `types.ts`. A new Tauri command is
not remote until this directory lists it with a scope.

**Idempotency.** `call.id` is client-generated. The host remembers recent ids per client, so
an `answer` resent after a dropped link (the A2 offline queue) applies once. An already
resolved inbox item returns `error{code: already_answered}`.

**Limits.** One decoded message is at most `MAX_MESSAGE_BYTES` = 4194304 (4 MiB) of UTF-8;
text fields keep `MAX_TEXT_BYTES` = 32768. Handshake attempts are rate-limited per source. Frames are never logged. Absolute
paths never cross the link (the existing IPC rule).

**Reopened if:** no reversal condition was recorded at the gate. Any change to the message
set, the handshake or the mirror rule reopens this section; the numbers are in `constants.ts`.

## Versioning

- An **optional field** in host output, or a **new event or feature flag**, does not bump `v`.
- **Clients ignore unknown fields** and render unknown enum values (agent kind, host, status,
  PR state, transcript entry kind) as plain text. A transcript entry whose `kind` is a string
  this build does not know decodes to `{kind: "unknown", raw_kind, text, seq, at}` (`text` is
  the string `text` of the entry, else `null`; other fields are dropped), so a new entry kind
  does not bump `v`. `unknown` is reserved and is never a host entry kind.
- **Host input stays strict** (`deny_unknown_fields`), so a new call argument is gated on a
  `welcome.features` flag.
- **Removing or changing the meaning of anything bumps `v`.**

**Reopened if:** no reversal condition was recorded at the gate. Any change to these
compatibility rules reopens this section.

## Room for later

Reserved now so a later milestone does not need a v2:

- **D7:** sessions and tasks carry `agent` (a string; `claude` today).
- **Host id:** `welcome` carries `host_id`, whatever D8 decides.
- **D8:** every snapshot and event is scoped to the host of that connection. Items gain an
  optional `host` only if D8 (SSH sub-hosts) survives.
- **D6:** tasks carry `pr: null | {state, url, checks}`.
- **H2:** the `notice` event, and a read path with no desktop-only call.
- **H3:** host key pinning, Unreachable-not-Failed, and one connection per host.

**Reopened if:** no reversal condition was recorded at the gate.

## Using it

No package, no workspace, no copy step: a consumer imports the files by relative path.
`protocol/` is erasable TypeScript with relative, extensionless imports and no construct that
Babel compiles to a `@babel/runtime` helper (no `class`, no iterable destructuring), because
it has no `node_modules` of its own.

- **Mobile** adds `../protocol` to `watchFolders` in `metro.config.js` (Expo watches only its
  own folder outside a workspace, and Metro will not resolve a file it does not watch). Its
  jest suite runs the same fixtures.
- **Desktop** does not import it at runtime yet; `src/protocol.test.ts` is the canonical
  fixture run and asserts that `src/types.ts` stays assignable to the wire types.
- **Rust** is `orchestra_core::wire`, tested against `fixtures/` by `core/tests/wire_fixtures.rs`.
- A **fixture case** is one file: `{name, dir, wire | raw, expect, client_only?, decoded_wire?}`.
  `dir` is `c2h`, `h2c` or the plaintext `hs_c2h`, `hs_h2c`. `expect` is `"ok"` (decode, then
  encode, gives back `wire`) or `{error: code}`. A `client_only` case holds a tolerance the
  Rust copy does not model (an unknown enum value, an extra field); Rust only checks that it is
  JSON. `decoded_wire` is what a lossy decode re-encodes to.

## What P2 decided

Wire types in code (`types.ts`, `codec.ts`), the Rust copy (`orchestra_core::wire`) and the
shared fixtures. Where the sections above left a gap:

- **Size.** `MAX_MESSAGE_BYTES` = 4194304 (4 MiB) for one decoded JSON message, checked
  first, before parsing. One snapshot carries every inbox item, and an item can hold several
  32 KiB text fields. Splitting a message across Noise frames (65535 bytes each) is P3's.
- **Field types.** `v` is the integer 1. `call.id` is a non-empty string of at most 128
  bytes. `seq` is a non-negative integer; the codec does not check its order (P3). Every
  string a client sends is at most `MAX_TEXT_BYTES`. `result` has exactly one of `ok`
  (any JSON, `null` for unit commands) and `error{code, message}`. The handshake frames are
  `{proto: "orchestra", v}` and `{proto, v, supported: [1]}`, with no `type`. The 15 methods
  above are the whole method set; `pick_project` is not one.
- **Error codes.** A closed set on the host: `unsupported_version`, `unknown_type`,
  `bad_request`, `forbidden`, `revoked`, `already_answered`, `failed`. `bad_request` covers
  malformed JSON, a missing or non-integer `v`, a wrong field type, an unknown field, an
  unknown method, an oversize message and an over-long string. `failed` is a command's
  error string, mapped in P3. A client keeps a code it does not know as plain text.
- **Tolerance.** Host input is strict: an unknown field is `bad_request`. Client input
  ignores unknown fields, keeps unknown enum values (status, kind, model, scope, feature,
  error code, transcript entry kind) as the strings they arrived as, reads an unknown event
  name as an unknown event rather than an error, and reads an unknown transcript entry kind as
  the `unknown` fallback rather than failing the event. An unknown `v` is
  `unsupported_version` in both directions, checked before `type`, `proto` and every field.
- **Not typed yet.** The `notice` payload (H2), `pr` (D6) and `register_push`. `agent` and
  `host` are declared optional on the payloads; the core does not emit them today.
- **No `ack`.** `result{id}` acknowledges a call.

Still for P3: the choice between the two JS crypto options and the scope UI.

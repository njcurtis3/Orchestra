# Orchestra

Orchestra is a desktop command center and Android remote for coding agents (Claude Code
first) running on the user's own computer. The desktop app runs and tracks agents, shows
their status, and collects their questions and finished work in an inbox; larger work runs
through an orchestrated graph of agents (scout, architect, human gate, builders, reviewers,
integrator) defined in `fleet/`. The Android app is a remote: notifications, status, and
answering agents. Agents never run on the phone. Product references: One (getone.one) for
the inbox/status/router model, and the owner's earlier omaorchestra (hooks-based session
tracking, task queue, per-task worktrees, MCP profiles).

Standalone app under the repos/ umbrella. Never import from a sibling app; see ../graph_agents/CLAUDE.md.

## Layout

| Path | What | Stack |
|---|---|---|
| `desktop/` | Desktop app; Rust core in `desktop/src-tauri`, UI in `desktop/src` | Tauri 2, React, TypeScript, Vite |
| `mobile/` | Android remote | Expo (React Native, TypeScript), expo-router |
| `protocol/` | Desktop <-> phone message format | TypeScript (source of truth), Rust copy in desktop |
| `fleet/` | Vendored orchestration model | synced from upstream, **never hand-edited** |
| `.github/workflows/` | Release matrix (desktop) | GitHub Actions, tauri-action |

There is no root `package.json` or workspace. `desktop/` and `mobile/` each install on
their own.

## Commands

Prerequisites: Node 22+, and for the desktop app a Rust toolchain (`rustup`) plus the
Tauri Windows prerequisites (MSVC build tools, WebView2).

```bash
# desktop
cd desktop && npm install
npm run tauri dev          # run the app (needs Rust)
npm run build              # typecheck + build the frontend only (no Rust needed)
npm test                   # frontend tests (vitest)
cd src-tauri && cargo test # Rust tests

# mobile
cd mobile && npm install
npx expo start             # dev server; also generates expo-env.d.ts (gitignored)
npx tsc --noEmit           # typecheck; run `npx expo start` once first
npx expo-doctor            # dependency/config check
npx expo install <pkg>     # always, instead of npm install <pkg>
```

Deploy is not set up: desktop releases come from
`.github/workflows/desktop-release.yml` (tag `desktop-v*`, currently unsigned drafts);
Android builds go through EAS (`eas.json`). Signing keys, the EAS project, the updater
key and store listings are created behind the ops gate, never at scaffold time.

## Architecture

- **Desktop is the host.** It runs agents locally, tracks their state through the agents'
  own hooks, and holds the inbox. State lives on the desktop machine.
- **Mobile is a client** of the desktop over the protocol in `protocol/`. It holds no agent
  state of its own beyond a cache.
- **`fleet/` is the orchestration model** the desktop executes: agent definitions, skills,
  run-state schema, verify/brief scripts.

## Constraints

- **`fleet/` is vendored and one-way.** It is filled only by a sync run from upstream, which
  records the upstream commit in `fleet/SOURCE`. Never hand-edit it; fix upstream and sync.
  Nothing in this repo may read a path outside the repo to get fleet files — the app must
  build, test and ship with no other directory on disk.
- **One protocol, both clients.** Any change to `protocol/` ships in desktop and mobile in
  the same change, versioned in `protocol/`. An older client fails loudly on an unknown
  version.
- **Windows first** for desktop. There is no Mac: macOS builds, signing and notarization
  run on the GitHub Actions macOS runner only.
- **No commit carries Claude attribution**, and the author is always the owner.

## Native targets

Copied from the umbrella's native conventions at scaffold time (2026-10-09); owned here
and allowed to drift.

### Desktop (Tauri 2)

- **Capabilities are an allowlist.** `src-tauri/capabilities/` grants each window only the
  permissions it uses. Currently `core:default` only. fs scopes are narrowed to the app's
  data dir or a named folder (never `$HOME/**`, `/**`, a drive root); user-chosen files come
  through the dialog plugin. Shell/spawn is limited to named programs with fixed args;
  opener to expected `https` URLs. A window loading remote content gets its own, smaller
  capability.
- **CSP is set** in `tauri.conf.json` (`app.security.csp`), never `null`, no `unsafe-eval`,
  no `unsafe-inline` for scripts. `connect-src` lists real origins plus `ipc:
  http://ipc.localhost`. When the desktop<->phone link lands, add its origin explicitly.
- **IPC commands validate every argument** in Rust (typed serde structs/enums). No frontend
  string is used as a path or a shell command; paths resolve against a base dir and reject
  `..` and absolute paths. Errors do not leak absolute paths or secrets.
  One recorded exception: an absolute project folder chosen in the native dialog is stored
  by Rust in `app_data_dir/state.json` (with the sessions and inbox), never accepted from or
  sent to the frontend, and re-validated with `is_dir` on load.
- **Window state persists** (`tauri-plugin-window-state`, registered on the builder under
  `cfg(desktop)`; the frontend never calls it, so no capability is granted). The plugin
  restores size and position; `src/window.rs` then checks the title bar is reachable on a
  current monitor and otherwise clamps to the primary monitor and centres the window.
- **Menus follow the OS**; accelerators use `CmdOrCtrl`.
- **Webviews differ**: WebView2 (Windows), WKWebView (macOS), WebKitGTK (Linux, the floor).
  Check web features against all three.
- **Signed releases only.** Updater pubkey in `tauri.conf.json`, private key only in CI
  secrets, `bundle.createUpdaterArtifacts: true`. Windows Authenticode; macOS Developer ID +
  notarization on the macOS runner. An unsigned build is not a release.

### Mobile (Expo, Android first)

- **Continuous Native Generation.** `ios/` and `android/` are generated and gitignored;
  native config lives in `app.json` and config plugins.
- **`eas.json` keeps `development`, `preview`, `production`**, each with a channel.
  (`development` uses a dev client: add `expo-dev-client` before the first dev build.)
- **`runtimeVersion` policy is `fingerprint`.** OTA updates carry JS and assets only; any
  native module, config plugin, permission or SDK change ships as a new build.
- **Safe areas** via `react-native-safe-area-context`; no hard-coded bar or notch heights.
- **Touch targets >= 48dp on Android** (>= 44pt iOS), with >= 8dp spacing; use `hitSlop`.
- **Android back** pops, closes, then exits; a custom `BackHandler` returns `true` only when
  it handled the event.
- **Network is assumed absent**: every remote-data screen has loading, error and offline
  states, connectivity read from `expo-network`; offline writes are queued or visibly refused.
  This matters doubly here — the phone is useless without the desktop, and must say so.
- **Permissions** (notifications first): rationale before the prompt, a denied path with
  `Linking.openSettings()`, declarations only in `app.json`.
- **Secrets** (pairing keys, tokens) only in `expo-secure-store`. Never `AsyncStorage`, never
  `EXPO_PUBLIC_*`.
- **Accessibility**: `accessibilityLabel`/`accessibilityRole` on every control; font scaling
  stays on.

Sources, for a human reader: `../graph_agents/conventions/native-desktop.md` and
`../graph_agents/conventions/native-mobile.md`. Nothing here reads them.

<h1 align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/logo-dark.svg">
    <img src="docs/assets/logo-light.svg" alt="Orchestra" height="72">
  </picture>
</h1>

<p align="center">
  <i>A desktop command center for the coding agents on your own computer: see who is working, who needs you, and what each one changed.</i>
</p>

<h4 align="center">
  <img src="https://img.shields.io/badge/platform-Windows-17181c.svg?style=flat-square&logo=windows&logoColor=white" alt="Windows" style="height: 20px;">
  <img src="https://img.shields.io/badge/Tauri-2-8f8c84.svg?style=flat-square&logo=tauri&logoColor=white" alt="Tauri 2" style="height: 20px;">
  <img src="https://img.shields.io/badge/Rust-2024-8f8c84.svg?style=flat-square&logo=rust&logoColor=white" alt="Rust 2024 edition" style="height: 20px;">
  <img src="https://img.shields.io/badge/React-19-8f8c84.svg?style=flat-square&logo=react&logoColor=white" alt="React 19" style="height: 20px;">
  <br>
  <img src="https://img.shields.io/badge/agent-Claude%20Code-8f8c84.svg?style=flat-square" alt="Claude Code" style="height: 20px;">
  <img src="https://img.shields.io/badge/mobile-Expo%20(Android)-8f8c84.svg?style=flat-square&logo=expo&logoColor=white" alt="Expo, Android" style="height: 20px;">
  <img src="https://img.shields.io/badge/status-pre--alpha-c9a227.svg?style=flat-square" alt="status: pre-alpha" style="height: 20px;">
  <a href="LICENSE">
    <img src="https://img.shields.io/badge/license-all%20rights%20reserved-8f8c84.svg?style=flat-square" alt="license: all rights reserved" style="height: 20px;">
  </a>
</h4>

<p align="center">
  <a href="#install"><b>Install</b></a> ·
  <a href="#using-it">Using it</a> ·
  <a href="#supported-versions">Supported versions</a> ·
  <a href="#documentation">Documentation</a> ·
  <a href="#development">Development</a>
</p>

## Introduction

Orchestra runs Claude Code sessions on your projects and keeps track of them
for you. It shows which sessions are working, which are waiting for your
permission or an answer, and which are done or failed. It collects their
questions in one inbox, so you answer them there instead of hunting through
terminal windows.

It also gives out the work. You queue tasks, and each one runs in its own git
worktree, so nothing touches your real checkout until you have reviewed the
diff and chosen to land it. Everything runs locally, and state lives in the
app's data folder on your machine.

An Android remote is planned: it will show notifications and status, and let
you answer agents from your phone. Agents will never run on the phone.

> [!WARNING]
> **Orchestra is pre-alpha.** The desktop app runs on Windows and is used on
> one machine so far. There are no signed releases yet, so you build it from
> a checkout. The state format may still change between versions; when it
> does, the app migrates older state forward and an older build refuses newer
> state rather than misreading it. **The Android remote does not work yet.**

<details open>
<summary>
 Features
</summary> <br />

<table>
  <tr>
    <td width="50%"><b>Sessions at a glance</b><br>Start Claude Code in any project folder and see each session's status: working, needs you, done or failed. A session that hits something the app can't handle stays flagged until you look at it.</td>
    <td width="50%"><b>One inbox</b><br>Permission requests and questions from every session land in a single inbox, and you answer them there.</td>
  </tr>
  <tr>
    <td><b>Survives a restart</b><br>Sessions and the inbox are saved. Quitting pauses running sessions, and replying to one later resumes the same conversation. The window reopens where you left it.</td>
    <td><b>Session detail</b><br>Read everything a session did: Claude's messages, each tool it ran and what came back, and what it cost. Send a follow-up to a finished session.</td>
  </tr>
  <tr>
    <td><b>Model and permissions per session</b><br>Pick Opus, Sonnet or Fable and a permission mode (manual, accept edits or plan) from a fixed list, so no free text reaches the command line.</td>
    <td><b>Notifications</b><br>A desktop notification when a session needs you or finishes, so you don't have to watch the app.</td>
  </tr>
  <tr>
    <td><b>Tasks in isolated worktrees</b><br>Queued tasks run two at a time, each on its own branch and git worktree. Review the full diff and every commit message, then land it (a <code>--no-ff</code> merge) or discard it.</td>
    <td><b>Landing that refuses when something is off</b><br>Land stops if your checkout moved to another branch, has uncommitted changes, or if any task commit carries Claude attribution. Cleanup never deletes outside the task's own worktree, even through a junction or symlink.</td>
  </tr>
  <tr>
    <td colspan="2"><b>Fleets, planned</b><br>Run larger work through a graph of agents (a scout, an architect, builders and reviewers) on top of tasks, with a plan you approve before anything is built. The model is vendored in <code>fleet/</code>; running it from the app is not built yet.</td>
  </tr>
</table>

</details>

## Install

<details open>
<summary>
 From a checkout
</summary> <br />

There are no releases yet. Building needs Node 22 or later, a Rust toolchain
(`rustup`), the [Tauri prerequisites for Windows](https://v2.tauri.app/start/prerequisites/)
(MSVC build tools and WebView2), and Git for Windows. Tasks run in git
worktrees, so they need Git; plain sessions do not. Claude Code must be
installed and signed in.

```bash
git clone https://github.com/njcurtis3/Orchestra
cd Orchestra/desktop
npm install
npm run tauri dev          # run the app
npm run tauri build        # or build an unsigned installer for this machine
```

To update, `git pull` and run it again.

</details>

<details>
<summary>
 Release builds
</summary> <br />

`.github/workflows/desktop-release.yml` builds Windows and macOS on a
`desktop-v*` tag. Code signing and the updater key are not set up yet, so
it produces unsigned draft builds, and those are not releases.

</details>

## Using it

1. **Open project** and pick its folder in the folder dialog.
2. **Start session** with a prompt, a model and a permission mode, and
   watch its status in the list.
3. **Answer the inbox** when a session asks for permission or has a
   question. Open a session to read its full transcript, see its cost, or
   send a follow-up.
4. **Queue task** when you want the work kept off your checkout. When it
   finishes, open it, read the diff under **Review**, then **Land** or
   **Discard**.

Orchestra keeps its state, transcripts and task worktrees in the app's data
folder (`%APPDATA%\com.telosrg.orchestra` on Windows).

<a id="supported-versions"></a>
<details>
<summary>
 Supported versions
</summary> <br />

| | Tested with | Needs |
|---|---|---|
| Windows | 11 | Windows first. macOS builds on CI only; Linux is not tried |
| Claude Code | 2.1 | |
| Git | 2.51 (Git for Windows) | for tasks only |
| Node | 22.17 | 22 or later |
| Rust | 1.99 | the 2024 edition |

</details>

## Documentation

| Part | What |
|---|---|
| [`desktop/`](desktop) | The Tauri 2 app: a thin shell in `desktop/src-tauri` over the Rust host engine in [`core/`](core), React and TypeScript UI in `desktop/src`. Runs and tracks agents and hosts the inbox. |
| [`mobile/`](mobile) | The Expo app for Android: the remote. Notifications, status and answers. Not functional yet. |
| [`protocol/`](protocol/README.md) | The message format between desktop and phone: TypeScript types and codec, shared JSON fixtures, and the decisions behind them. |
| [`fleet/`](fleet/README.md) | The vendored orchestration model. Synced from upstream and never edited by hand. |
| [`CLAUDE.md`](CLAUDE.md) | Architecture, constraints, and the security rules the app is built to |

## Development

```bash
cd desktop
npm run build              # typecheck and build the frontend (no Rust needed)
npm test                   # frontend tests (vitest)
cd ..
cargo test --workspace  # Rust tests (core and shell)

cd mobile
npx expo start             # dev server
npx tsc --noEmit           # typecheck; run `npx expo start` once first
```

`desktop/` and `mobile/` each install on their own; there is no root
workspace.

## License

Copyright (c) 2026 Nathan Curtis. All rights reserved.

Orchestra is proprietary. It may not be copied, reproduced, modified or
distributed, in whole or in part, without Nathan Curtis's prior written
consent. See [LICENSE](LICENSE).

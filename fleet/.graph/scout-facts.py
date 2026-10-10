#!/usr/bin/env python
"""Compute the facts a scout would otherwise re-derive by hand, every run.

    python @FLEET@/.graph/scout-facts.py

Reports on the project root: the git repository that contains the current directory, or
the current directory itself when it is not inside one. An optional positional argument is
accepted and ignored. It reads nothing but the project itself.

What it prints: whether the target is a git repository (the fact that decides the graph's
shape), the branch, HEAD, how many files are uncommitted, the commit identity, the stack
observed on disk, and a `native:` line only when native-app evidence is found on disk
(Expo -> native-mobile, Tauri -> native-desktop). When the target is not a git repository
it prints exactly `NOT A GIT REPO -> diamond forced to single-loop`.

WHY THIS EXISTS, and why it is a script rather than a cache
-----------------------------------------------------------
A per-project fact *store* was designed first and rejected on evidence. Scout facts rot:
two that a scout had recorded were checked against reality after the run and had become
false.

  "this directory is NOT a git repository"   - someone ran `git init` afterwards
  "the project has N top-level directories"  - N changed

So scouts do repeat work, but the facts they repeat most are the ones that rot fastest. A
cache would have handed a later run a confident, wrong answer to exactly the question that
decides the graph's SHAPE: git-repo status picks single-loop or diamond, so a stale
`false` there produces a diamond with no isolation and no rollback. That is worse than
re-deriving.

These facts are also cheap: one `git rev-parse` and a handful of file-existence checks.
Caching a one-command answer to save a one-command call is the expensive path.

So: compute them fresh, every time, deterministically. This script is structurally
incapable of going stale, because it stores nothing.

WHAT THIS DOES NOT DO
---------------------
It does not read source, form judgments, or find the thing that will break the plan. That
is the scout's actual job and the part worth spending a model on. This only clears the
mechanical questions off its desk first - the ones with one correct answer that a `git`
call or a file-existence check already knows.

It reports what IS. Every "missing"/"absent" line is a fact, not a complaint; the scout
decides whether any of them matter to the task at hand.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path


# Files that answer "how is this app built and tested" without opening any of
# them. Presence is the fact; the scout reads the ones the task touches.
STACK_MARKERS = [
    ("package.json", "node"),
    ("pnpm-workspace.yaml", "pnpm workspace"),
    ("requirements.txt", "python"),
    ("pyproject.toml", "python"),
    ("pytest.ini", "pytest"),
    ("Cargo.toml", "rust"),
    ("go.mod", "go"),
    ("Makefile", "make"),
    ("Dockerfile", "docker"),
    (".github/workflows", "github actions"),
]


# Native-app detection walks the root and two levels down (a monorepo may keep its
# Expo app at apps/mobile) and never enters build output or vendored trees.
NATIVE_SKIP = {"node_modules", ".git", "target", "dist", "build", ".expo", "ios", "android"}
NATIVE_MAX_DEPTH = 2


def git(args: list[str], cwd: str | Path) -> tuple[int, str]:
    try:
        proc = subprocess.run(
            ["git", *args],
            cwd=str(cwd),
            capture_output=True,
            text=True,
            timeout=15,
        )
        return proc.returncode, (proc.stdout or proc.stderr).strip()
    except (OSError, subprocess.SubprocessError) as exc:
        return 1, f"git unavailable: {exc}"


def repo_facts(path: Path) -> dict:
    """The git questions, answered now rather than remembered.

    `is_git_repo` is the one that decides graph shape, so it is reported first
    and never inferred from anything but git itself.
    """
    if not path.exists():
        return {"exists": False, "is_git_repo": False}

    code, _ = git(["rev-parse", "--is-inside-work-tree"], path)
    if code != 0:
        return {"exists": True, "is_git_repo": False}

    _, head = git(["rev-parse", "--short", "HEAD"], path)
    _, branch = git(["rev-parse", "--abbrev-ref", "HEAD"], path)
    _, status = git(["status", "--porcelain"], path)
    _, remote = git(["remote", "get-url", "origin"], path)
    _, author = git(["config", "user.name"], path)
    _, email = git(["config", "user.email"], path)

    dirty = [ln for ln in status.splitlines() if ln.strip()]
    return {
        "exists": True,
        "is_git_repo": True,
        "head": head,
        "branch": branch,
        "dirty_files": len(dirty),
        "clean": not dirty,
        "remote": remote if remote and "fatal" not in remote.lower() else None,
        # Identity is per-repo here and has been wrong before, so it is a fact
        # worth surfacing before any node commits under it.
        "commit_identity": f"{author} <{email}>" if author else None,
    }


def _read_json(path: Path):
    """(data, ok). A file that will not parse is evidence, never a crash."""
    try:
        return json.loads(path.read_text(encoding="utf-8")), True
    except (OSError, ValueError):
        return None, False


def _dir_native(d: Path) -> dict:
    """Evidence for one directory, keyed by kind. Only what is in `d` itself."""
    found = {"expo": [], "tauri": [], "unparseable": []}

    app_json = d / "app.json"
    if app_json.is_file():
        data, ok = _read_json(app_json)
        if not ok:
            found["expo"].append("app.json unparseable")
        elif isinstance(data, dict) and "expo" in data:
            found["expo"].append("app.json has expo key")
        # a bare React Native app.json has no 'expo' key and is not evidence
    for name in ("app.config.js", "app.config.ts", "app.config.json", "eas.json"):
        if (d / name).is_file():
            found["expo"].append(name)

    pkg = d / "package.json"
    if pkg.is_file():
        data, ok = _read_json(pkg)
        if not ok:
            # a parse problem, not evidence of either stack
            found["unparseable"].append("package.json unparseable")
        elif isinstance(data, dict):
            deps = {}
            for key in ("dependencies", "devDependencies"):
                if isinstance(data.get(key), dict):
                    deps.update(data[key])
            if "expo" in deps:
                found["expo"].append(f"package.json dep expo {deps['expo']}")
            for name in sorted(deps):
                if name.startswith("@tauri-apps/"):
                    found["tauri"].append(f"package.json dep {name} {deps[name]}")

    if (d / "src-tauri").is_dir():
        found["tauri"].append("src-tauri/ dir")
    for name in ("tauri.conf.json", "tauri.conf.json5", "Tauri.toml"):
        if (d / name).is_file():
            found["tauri"].append(name)
    return found


def native_facts(path: Path) -> list[dict]:
    """Expo / Tauri evidence under an app: root, then depth <= 2.

    Each hit is {kind, dir, evidence[]}; `dir` is the project root joined with the
    sub-path. Read-only: it only lists and reads files.
    """
    path = Path(path)
    hits: list[dict] = []
    if not path.is_dir():
        return hits

    def visit(d: Path, depth: int) -> None:
        found = _dir_native(d)
        for kind in ("expo", "tauri", "unparseable"):
            if found[kind]:
                hits.append({"kind": kind, "dir": d.as_posix(), "evidence": found[kind]})
        if depth >= NATIVE_MAX_DEPTH:
            return
        try:
            children = sorted(c for c in d.iterdir() if c.is_dir())
        except OSError:
            return
        for c in children:
            # src-tauri is already evidence on its parent; its config is not a second app
            if c.name not in NATIVE_SKIP and c.name != "src-tauri":
                visit(c, depth + 1)

    visit(path, 0)
    return hits


NATIVE_LABEL = {"expo": "native-mobile", "tauri": "native-desktop"}


def project_root() -> tuple[Path, bool]:
    """(root, is_git_repo): the repo containing the cwd, else the cwd itself."""
    cwd = Path.cwd()
    code, top = git(["rev-parse", "--show-toplevel"], cwd)
    if code == 0 and top:
        return Path(top), True
    return cwd, False


def observed_stack(root: Path) -> list[str]:
    return [label for marker, label in STACK_MARKERS if (root / marker).exists()]


def render(root: Path, is_repo: bool) -> str:
    # Output stays ASCII on purpose: it is read off a console by a cheap model.
    lines = [f"PROJECT: {root.as_posix()}"]
    if not is_repo:
        # Quoted verbatim by the feature-graph skill; keep the text exact.
        lines.append("NOT A GIT REPO -> diamond forced to single-loop")
    else:
        g = repo_facts(root)
        head, branch = g["head"], g["branch"]
        if not head or head.lower().startswith("fatal"):
            head = "no commits yet"
            code, name = git(["symbolic-ref", "--short", "HEAD"], root)
            if code == 0 and name:
                branch = name
        state = "clean" if g["clean"] else f"{g['dirty_files']} uncommitted file(s)"
        lines.append(f"  git:         {branch} @ {head}, {state}")
        lines.append(f"  identity:    {g['commit_identity']}")
        if g["remote"] and not g["remote"].lower().startswith(("error", "fatal")):
            lines.append(f"  remote:      {g['remote']}")
    lines.append(f"  stack:       observed={observed_stack(root)}")
    hits = native_facts(root)
    native = [
        f"{NATIVE_LABEL[h['kind']]} at {Path(h['dir']).relative_to(root).as_posix()} "
        f"({', '.join(h['evidence'])})"
        for h in hits
        if h["kind"] in NATIVE_LABEL
    ]
    if native:
        lines.append(f"  native:      {'; '.join(native)}")
    for h in hits:
        if h["kind"] == "unparseable":
            lines.append(f"  ! {', '.join(h['evidence'])} at {Path(h['dir']).relative_to(root).as_posix()}")
    return "\n".join(lines)


def main(argv: list[str]) -> None:
    """The arguments are accepted and ignored: the target is always the project root."""
    root, is_repo = project_root()
    print(render(root, is_repo))


if __name__ == "__main__":
    main(sys.argv[1:])

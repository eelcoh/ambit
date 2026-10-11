#!/usr/bin/env python3
"""Checks what each agent-owned app crate (every workspace member under apps/) can pull in.
Human-owned.

- An app has no build script and is not a proc macro: both run at build time with full authority.
- Everything an app depends on (normal and build edges, transitively; not dev) is in its
  allowed-deps.txt, and nothing listed there is unused.
- Every dependency comes from crates.io or is a workspace member: an allowlisted name from a git
  repo or a stray path is still a different crate.
"""
import json
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
# The targets the app is built for. Dependencies behind other cfgs don't end up in an artifact.
TARGETS = ["x86_64-unknown-linux-gnu", "x86_64-unknown-hermit"]

result = subprocess.run(
    ["cargo", "metadata", "--format-version", "1", "--locked"]
    + [arg for t in TARGETS for arg in ("--filter-platform", t)],
    cwd=ROOT, capture_output=True, text=True,
)
if result.returncode != 0:
    print("app checks failed: cargo metadata failed\n" + result.stderr, end="")
    sys.exit(1)
meta = json.loads(result.stdout)
packages = {p["id"]: p for p in meta["packages"]}
members = set(meta["workspace_members"])
nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
APPS = ROOT / "apps"
apps = sorted(i for i in members if pathlib.Path(packages[i]["manifest_path"]).parent.parent == APPS)
# Every directory with a manifest under apps/ must be a member, or it escapes these checks.
dirs = {p.parent for p in APPS.glob("*/Cargo.toml")}
member_dirs = {pathlib.Path(packages[i]["manifest_path"]).parent for i in apps}


def check(app):
    name = packages[app]["name"]
    errors = []
    for target in packages[app]["targets"]:
        if set(target["kind"]) & {"custom-build", "proc-macro"}:
            errors.append(f"must not have a {'/'.join(target['kind'])} target ({target['src_path']})")

    # Everything reachable from the app over normal and build edges.
    seen, todo = set(), [app]
    while todo:
        for dep in nodes[todo.pop()]["deps"]:
            if any(k["kind"] in (None, "build") for k in dep["dep_kinds"]) and dep["pkg"] not in seen:
                seen.add(dep["pkg"])
                todo.append(dep["pkg"])

    for pkg in sorted(seen):
        p = packages[pkg]
        if pkg not in members and p["source"] != CRATES_IO:
            errors.append(f"{p['name']} {p['version']} comes from {p['source'] or p['manifest_path']}, "
                          "not crates.io or the workspace")

    actual = {packages[p]["name"] for p in seen}
    listing = pathlib.Path(packages[app]["manifest_path"]).parent / "allowed-deps.txt"
    lines = listing.read_text().splitlines() if listing.exists() else []
    allowed = {l.strip() for l in lines if l.strip() and not l.startswith("#")}
    rel = listing.relative_to(ROOT)
    for dep in sorted(actual - allowed):
        errors.append(f"{dep} is not in {rel}")
    for dep in sorted(allowed - actual):
        errors.append(f"{rel} lists {dep}, which {name} no longer uses")
    return name, len(actual), errors


failed = False
for d in sorted(dirs - member_dirs):
    print(f"app checks failed: {d.relative_to(ROOT)} is not a workspace member")
    failed = True
for app in apps:
    name, count, errors = check(app)
    if errors:
        print(f"app checks failed for {name}:")
        print("\n".join(f"  {e}" for e in errors))
        failed = True
    else:
        print(f"app checks: {name} OK ({count} dependencies, all allowlisted, crates.io or workspace)")
sys.exit(1 if failed else 0)

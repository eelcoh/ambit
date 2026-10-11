#!/usr/bin/env python3
"""Checks what the agent-owned app crate can pull in. Human-owned.

- The app has no build script and is not a proc macro: both run at build time with full authority.
- Everything the app depends on (normal and build edges, transitively; not dev) is in
  app/allowed-deps.txt, and nothing listed there is unused.
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
app = next(i for i in members if packages[i]["name"] == "app")

errors = []

for target in packages[app]["targets"]:
    if set(target["kind"]) & {"custom-build", "proc-macro"}:
        errors.append(f"app must not have a {'/'.join(target['kind'])} target ({target['src_path']})")

# Everything reachable from app over normal and build edges.
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
lines = (ROOT / "app/allowed-deps.txt").read_text().splitlines()
allowed = {l.strip() for l in lines if l.strip() and not l.startswith("#")}
for name in sorted(actual - allowed):
    errors.append(f"{name} is not in app/allowed-deps.txt")
for name in sorted(allowed - actual):
    errors.append(f"app/allowed-deps.txt lists {name}, which app no longer uses")

if errors:
    print("app checks failed:")
    print("\n".join(f"  {e}" for e in errors))
    sys.exit(1)
print(f"app checks: OK ({len(actual)} dependencies, all allowlisted, crates.io or workspace)")

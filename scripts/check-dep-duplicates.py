#!/usr/bin/env python3
"""Fail if a dependency this workspace names directly is behind a newer,
semver-incompatible line already present in the resolved graph.

Not `cargo deny check bans`: an --all-features resolve here is ~700 crates with
~50 duplicate pairs, nearly all between third-party crates we cannot influence,
so a skip list would rot the way the stale floors did.

The fixable duplicates are the ones where our own floor is what lags — which is
what every finding in the dependency audit was:

    tokio-tungstenite  we said 0.24, axum pulled 0.29
    webpki-roots       we said 0.26, which is a shim re-exporting 1.0
    reqwest            we said 0.12, opentelemetry-otlp pulled 0.13
    sha2               we said 0.10, tokio-postgres pulled 0.11

The reverse — a third party stuck on an older line than ours — is reported but
not gated: only that crate can collapse it.

    python3 scripts/check-dep-duplicates.py
"""

import json
import subprocess
import sys
from collections import defaultdict

ALLOWED = {
    # serde_derive is on syn 3, our two proc-macro crates on syn 2. Collapsing
    # this would remove a whole syn compile from the default consumer graph,
    # but it is a ~5k-line migration across a broad syn surface — its own
    # change, with the derive crate's tests driving it.
    "syn",
}


def parse(v):
    """`1.2.3-rc.1` -> (1, 2, 3). Prerelease suffixes are irrelevant here."""
    core = v.split("-")[0].split("+")[0]
    parts = [int(p) for p in core.split(".")[:3]]
    return tuple(parts + [0] * (3 - len(parts)))


def req_floor(req):
    """The floor of a requirement, e.g. `^0.29` -> (0, 29, 0)."""
    return parse(req.lstrip("^>=~ ").split(",")[0].strip())


def line(v):
    """Cargo's compatibility line: `1.x` for 1.0 and up, `0.y` below it."""
    return (v[0],) if v[0] > 0 else (0, v[1])


def main():
    meta = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--all-features", "--format-version", "1"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )

    members = set(meta["workspace_members"])
    versions = defaultdict(set)
    for pkg in meta["packages"]:
        versions[pkg["name"]].add(pkg["version"])

    direct = {}
    for pkg in meta["packages"]:
        if pkg["id"] in members:
            for dep in pkg["dependencies"]:
                direct.setdefault(dep["name"], set()).add(dep["req"])

    ours, theirs = [], []
    for name, vs in sorted(versions.items()):
        if len(vs) > 1:
            entry = (name, sorted(vs, key=parse), sorted(direct.get(name, ())))
            (ours if name in direct else theirs).append(entry)

    if theirs:
        print(f"note: {len(theirs)} duplicate(s) between third-party crates "
              f"(not gated): {', '.join(n for n, _, _ in theirs)}\n")

    failures = []
    for name, vs, reqs in ours:
        ours_line = max(line(req_floor(r)) for r in reqs)
        newest = max(line(parse(v)) for v in vs)
        if newest <= ours_line:
            print(f"behind  {name}: resolved {', '.join(vs)} — we require "
                  f"{', '.join(reqs)}; another crate is on an older line, "
                  f"not ours to collapse")
        elif name in ALLOWED:
            print(f"allowed {name}: resolved {', '.join(vs)} — we require "
                  f"{', '.join(reqs)}")
        else:
            failures.append(name)
            print(f"ERROR   {name}: resolved {', '.join(vs)} — we require "
                  f"{', '.join(reqs)}, but a newer incompatible line is "
                  f"already in the tree")

    if failures:
        print(
            "\nA crate we name directly is behind a newer line already in the "
            "tree, so both\nget compiled. Raise our floor to that line, or add "
            "it to ALLOWED above with\nthe reason it cannot be collapsed. See "
            "the dependency policy in CLAUDE.md.",
            file=sys.stderr,
        )
        return 1

    print("\nok: no direct dependency is behind a newer line in the tree.")
    return 0


if __name__ == "__main__":
    sys.exit(main())

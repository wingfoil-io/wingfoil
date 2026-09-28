#!/usr/bin/env python3
"""cache-secret-audit: find GitHub Actions layouts where a secret can land in a
PR-readable build cache.

Background: https://blog.rust-lang.org/2026/09/21/github-actions-leaking-secrets-when-miri-output-is-cached/

Single file, one dependency (PyYAML). Run with --help for usage.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from dataclasses import asdict, dataclass
from pathlib import Path

import yaml

__version__ = "0.1.0"

SEVERITIES = {"low": 0, "medium": 1, "high": 2}

# --------------------------------------------------------------------------
# YAML loading with line numbers
# --------------------------------------------------------------------------


class LineDict(dict):
    """A mapping that remembers the 1-based line of itself and of each key."""

    line: int = 0
    key_lines: dict

    def line_of(self, key) -> int:
        return self.key_lines.get(key, self.line)


class _Loader(yaml.SafeLoader):
    pass


# YAML 1.1 reads a bare `on:` key as boolean True. Workflows are not YAML 1.1
# documents in spirit, so drop the yes/no/on/off booleans and keep true/false.
_Loader.yaml_implicit_resolvers = {
    ch: [(tag, rx) for tag, rx in resolvers if tag != "tag:yaml.org,2002:bool"]
    for ch, resolvers in yaml.SafeLoader.yaml_implicit_resolvers.items()
}
_Loader.add_implicit_resolver(
    "tag:yaml.org,2002:bool",
    re.compile(r"^(?:true|True|TRUE|false|False|FALSE)$"),
    list("tTfF"),
)


def _construct_mapping(loader, node, deep=False):
    loader.flatten_mapping(node)
    out = LineDict()
    out.line = node.start_mark.line + 1
    out.key_lines = {}
    for key_node, value_node in node.value:
        key = loader.construct_object(key_node, deep=True)
        out[key] = loader.construct_object(value_node, deep=True)
        out.key_lines[key] = key_node.start_mark.line + 1
    return out


_Loader.add_constructor(
    yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, _construct_mapping
)


class ParseError(Exception):
    pass


def load_workflow(path: Path) -> LineDict:
    try:
        doc = yaml.load(path.read_text(encoding="utf-8"), Loader=_Loader)
    except (yaml.YAMLError, UnicodeDecodeError, OSError) as exc:
        raise ParseError(f"{path}: {exc}") from exc
    if not isinstance(doc, LineDict):
        raise ParseError(f"{path}: top level is not a mapping")
    return doc


# --------------------------------------------------------------------------
# Expression helpers
# --------------------------------------------------------------------------

# `secrets.NAME` and `secrets['NAME']` / `secrets["NAME"]`.
_SECRET_RE = re.compile(
    r"""\bsecrets\s*(?:\.\s*([A-Za-z_][A-Za-z0-9_-]*)|\[\s*['"]([^'"]+)['"]\s*\])"""
)
_EXPR_RE = re.compile(r"\$\{\{(.*?)\}\}", re.S)

# GITHUB_TOKEN is minted per job, scoped by the workflow's `permissions:`,
# and expires when the job ends. A copy persisted into a cache is dead before
# anyone can read it back, so it is not the secret this tool is looking for.
_EXEMPT_SECRETS = {"GITHUB_TOKEN"}


def secret_names(value) -> list[str]:
    """Names of non-exempt secrets referenced inside `${{ }}` in value."""
    if not isinstance(value, str):
        return []
    names = []
    for expr in _EXPR_RE.findall(value):
        for dotted, indexed in _SECRET_RE.findall(expr):
            name = dotted or indexed
            if name.upper() not in _EXEMPT_SECRETS:
                names.append(name)
    return names


def env_secrets(env) -> list[tuple[str, int, list[str]]]:
    """(key, line, secret names) for each env entry that carries a secret.

    `env` may also be a single expression string (`env: ${{ fromJSON(...) }}`);
    that is reported under the key `<env>`.
    """
    if isinstance(env, LineDict):
        out = []
        for key, val in env.items():
            names = secret_names(val if isinstance(val, str) else None)
            if names:
                out.append((str(key), env.line_of(key), names))
        return out
    if isinstance(env, str):
        names = secret_names(env)
        return [("<env>", 0, names)] if names else []
    return []


def _strip(expr) -> str:
    s = str(expr).strip()
    m = re.fullmatch(r"\$\{\{(.*)\}\}", s, re.S)
    if m:
        s = m.group(1)
    return " ".join(s.split())


_GUARD_ATOMS = [
    re.compile(r"""^github\.ref\s*==\s*['"]refs/heads/(?:main|master)['"]$"""),
    re.compile(r"""^['"]refs/heads/(?:main|master)['"]\s*==\s*github\.ref$"""),
    re.compile(r"""^github\.event_name\s*==\s*['"]push['"]$"""),
    re.compile(r"""^['"]push['"]\s*==\s*github\.event_name$"""),
]
_NOT_PR_ATOM = re.compile(
    r"""^github\.event_name\s*!=\s*['"]pull_request['"]$"""
)


def _unparen(s: str) -> str:
    s = s.strip()
    while s.startswith("(") and s.endswith(")"):
        depth = 0
        for i, ch in enumerate(s):
            depth += ch == "("
            depth -= ch == ")"
            if depth == 0 and i != len(s) - 1:
                return s
        s = s[1:-1].strip()
    return s


def _split_top(s: str, op: str) -> list[str]:
    parts, depth, cur, i = [], 0, "", 0
    while i < len(s):
        if s[i] == "(":
            depth += 1
        elif s[i] == ")":
            depth -= 1
        if depth == 0 and s.startswith(op, i):
            parts.append(cur)
            cur, i = "", i + len(op)
            continue
        cur += s[i]
        i += 1
    parts.append(cur)
    return [p.strip() for p in parts]


def excludes_pr(expr, pr_target: bool) -> bool:
    """True if `expr` is only true outside pull-request events.

    Conservative: each `||` branch must contain, as an `&&` conjunct, a
    positive test for `refs/heads/main|master` or `event_name == 'push'`.
    `event_name != 'pull_request'` counts only when `pull_request_target`
    is not a trigger. Anything with a `!` negation of a group is rejected.
    """
    if expr is None or isinstance(expr, bool):
        return False
    s = _strip(expr)
    if re.search(r"!\s*\(", s):
        return False
    for branch in _split_top(_unparen(s), "||"):
        conj = [_unparen(c) for c in _split_top(_unparen(branch), "&&")]
        ok = any(a.match(c) for c in conj for a in _GUARD_ATOMS) or (
            not pr_target and any(_NOT_PR_ATOM.match(c) for c in conj)
        )
        if not ok:
            return False
    return True


def is_false(value) -> bool:
    return value is False or _strip(value).lower() == "false"


# --------------------------------------------------------------------------
# Workflow shape
# --------------------------------------------------------------------------

PR_EVENTS = ("pull_request", "pull_request_target", "workflow_call")


def triggers(doc: LineDict) -> list[str]:
    on = doc.get("on", doc.get(True))
    if isinstance(on, str):
        return [on]
    if isinstance(on, list):
        return [str(x) for x in on]
    if isinstance(on, dict):
        return [str(k) for k in on]
    return []


def pr_trigger(doc: LineDict) -> str | None:
    t = triggers(doc)
    for ev in PR_EVENTS:
        if ev in t:
            return ev
    return None


def jobs(doc: LineDict):
    j = doc.get("jobs")
    if isinstance(j, LineDict):
        for name, job in j.items():
            if isinstance(job, LineDict):
                yield str(name), job


def steps(job: LineDict):
    s = job.get("steps")
    if isinstance(s, list):
        for step in s:
            if isinstance(step, LineDict):
                yield step


def action_of(step: LineDict) -> str:
    uses = step.get("uses")
    if not isinstance(uses, str):
        return ""
    return uses.split("@", 1)[0].strip().lower()


CACHE_ACTIONS = ("swatinem/rust-cache", "actions/cache", "actions/cache/save")

_CARGO_RE = re.compile(r"(?<![\w$./-])cargo\s+(?:\+\S+\s+)?([\w-]+)")
_MIRI_RE = re.compile(r"(?<![\w$./-])cargo\s+(?:\+\S+\s+)?miri\b")

# `cargo publish` and friends need the registry token in their environment;
# that is the documented way to pass it. Exempt only that exact shape: a
# CARGO_REGISTRY(IES_*)_TOKEN in the *step's own* env, a run that invokes no
# cargo subcommand other than these, and no other secret in scope.
_REGISTRY_CMDS = {"publish", "login", "owner", "yank", "logout"}
_REGISTRY_TOKEN = re.compile(r"^CARGO_REGISTR(?:Y|IES_[A-Z0-9_-]+)_TOKEN$")


# --------------------------------------------------------------------------
# Findings
# --------------------------------------------------------------------------


@dataclass
class Finding:
    rule: str
    name: str
    severity: str
    file: str
    line: int
    message: str
    why: str
    fix: str


RULES = {
    "R1": (
        "secret-in-job-env",
        "high",
        "A secret placed in workflow- or job-level `env:` is exported to every "
        "step of the job, including every build script, proc macro, test and "
        "tool that step runs. Any of them can write its environment into "
        "`target/` (Miri did exactly this), and in a PR-reachable workflow "
        "that directory is what the cache step saves or restores. Once a "
        "secret is in the cache, anyone who can open a PR can restore it and "
        "print it. Scoping the secret to the one step that needs it keeps it "
        "out of every other process.",
        "# Before: job-level env\n"
        "jobs:\n"
        "  test:\n"
        "    env:\n"
        "      MY_TOKEN: ${{ secrets.MY_TOKEN }}\n"
        "# After: only the step that uses it sees it\n"
        "    steps:\n"
        "      - name: Upload\n"
        "        env:\n"
        "          MY_TOKEN: ${{ secrets.MY_TOKEN }}\n"
        "        run: ./upload.sh",
    ),
    "R2": (
        "pr-cache-save",
        "medium",
        "A cache that PR runs can write is a cache an untrusted branch can "
        "poison: the next trusted run on main restores whatever the PR put "
        "there. It also means PR runs and main runs share one artifact "
        "stream, so anything a main run leaked into the cache is one PR away "
        "from being read. Save only from the default branch; PR runs "
        "restore and never write.",
        "- uses: Swatinem/rust-cache@<sha>  # v2\n"
        "  with:\n"
        "    save-if: ${{ github.ref == 'refs/heads/main' }}\n"
        "# actions/cache: lookup-only on PRs, or split restore/save:\n"
        "- uses: actions/cache/save@<sha>  # v4\n"
        "  if: github.ref == 'refs/heads/main'",
    ),
    "R3": (
        "cargo-with-secret-in-scope",
        "high",
        "`cargo` compiles and runs code you did not write: build scripts and "
        "proc macros from every dependency, plus your tests. All of it "
        "inherits the step's environment, and some of it writes that "
        "environment to disk under `target/`. If `target/` is cached (it "
        "almost always is in Rust CI), the secret travels with it to every "
        "later run that restores the cache, including PR runs. This is the "
        "mechanism of the Miri disclosure, and it does not depend on the "
        "trigger: a push-only job on main that leaks is read back by a PR.",
        "# Keep cargo steps secret-free; hand the secret only to the step\n"
        "# that needs it, and never one that runs cargo.\n"
        "- run: cargo test\n"
        "- name: Upload coverage\n"
        "  env:\n"
        "    CODECOV_TOKEN: ${{ secrets.CODECOV_TOKEN }}\n"
        "  run: ./upload.sh",
    ),
    "R4": (
        "miri-with-cache",
        "high",
        "Before the nightly of 2026-09-22, `cargo miri` stored the full "
        "process environment under `target/`. Any cache step in the same "
        "workflow can persist that into the Actions cache. Even on a fixed "
        "toolchain the combination is worth removing: Miri output is cheap "
        "to rebuild and not worth the exposure.",
        "# Run Miri without a build cache and without secrets in scope.\n"
        "jobs:\n"
        "  miri:\n"
        "    steps:\n"
        "      - uses: actions/checkout@<sha>  # v4\n"
        "      - run: rustup +nightly component add miri\n"
        "      - run: cargo +nightly miri test",
    ),
}


def _mk(rule: str, file: str, line: int, message: str) -> Finding:
    name, severity, why, fix = RULES[rule]
    return Finding(rule, name, severity, file, line, message, why, fix)


def check_workflow(doc: LineDict, file: str) -> list[Finding]:
    out: list[Finding] = []
    trig = pr_trigger(doc)
    pr_target = "pull_request_target" in triggers(doc)
    wf_env = env_secrets(doc.get("env"))

    # R1: secrets in workflow/job env of a PR-reachable workflow.
    if trig:
        for key, line, names in wf_env:
            out.append(_mk("R1", file, line or doc.line_of("env"),
                f"workflow-level env `{key}` references secrets.{names[0]}; "
                f"workflow is PR-reachable via `{trig}`"))
        for jname, job in jobs(doc):
            for key, line, names in env_secrets(job.get("env")):
                out.append(_mk("R1", file, line or job.line_of("env"),
                    f"job `{jname}` env `{key}` references secrets.{names[0]}; "
                    f"workflow is PR-reachable via `{trig}`"))

    # R2: cache writable from a PR run.
    if trig:
        for jname, job in jobs(doc):
            job_guarded = excludes_pr(job.get("if"), pr_target)
            for step in steps(job):
                act = action_of(step)
                if act not in CACHE_ACTIONS:
                    continue
                if job_guarded or excludes_pr(step.get("if"), pr_target):
                    continue
                with_ = step.get("with") if isinstance(step.get("with"), dict) else {}
                line = step.line_of("uses")
                if act == "swatinem/rust-cache":
                    save_if = with_.get("save-if")
                    if save_if is None:
                        msg = "has no `save-if`, so PR runs save the cache"
                    elif is_false(save_if) or excludes_pr(save_if, pr_target):
                        continue
                    else:
                        msg = f"`save-if: {save_if}` does not restrict saves to main"
                elif act == "actions/cache":
                    lo = with_.get("lookup-only")
                    if lo is True or (isinstance(lo, str) and _strip(lo).lower() == "true"):
                        continue
                    msg = "saves on PR runs (no `lookup-only: true`, no PR-excluding `if:`)"
                else:
                    msg = "runs on PR events (no PR-excluding `if:`)"
                out.append(_mk("R2", file, line,
                    f"job `{jname}`: `{step.get('uses')}` {msg}; "
                    f"workflow is PR-reachable via `{trig}`"))

    # R3: cargo runs with a secret in scope (any trigger).
    for jname, job in jobs(doc):
        job_env = env_secrets(job.get("env"))
        for step in steps(job):
            run = step.get("run")
            if not isinstance(run, str):
                continue
            subcmds = _CARGO_RE.findall(run)
            if not subcmds:
                continue
            step_env = env_secrets(step.get("env"))
            inline = secret_names(run)
            scoped = (
                [("workflow env", k, n) for k, _, n in wf_env]
                + [("job env", k, n) for k, _, n in job_env]
                + [("step env", k, n) for k, _, n in step_env]
                + ([("run:", "<inline>", inline)] if inline else [])
            )
            if not scoped:
                continue
            if (
                set(subcmds) <= _REGISTRY_CMDS
                and all(where == "step env" and _REGISTRY_TOKEN.match(k)
                        for where, k, _ in scoped)
            ):
                continue
            where, key, names = scoped[0]
            out.append(_mk("R3", file, step.line_of("run"),
                f"job `{jname}`: step runs `cargo {subcmds[0]}` with "
                f"secrets.{names[0]} in scope ({where} `{key}`)"))

    # R4: cargo miri + any cache step in the same workflow.
    miri_line = None
    cache_line = None
    for _, job in jobs(doc):
        for step in steps(job):
            run = step.get("run")
            if miri_line is None and isinstance(run, str) and _MIRI_RE.search(run):
                miri_line = step.line_of("run")
            if cache_line is None and action_of(step) in CACHE_ACTIONS:
                cache_line = step.line_of("uses")
    if miri_line is not None and cache_line is not None:
        out.append(_mk("R4", file, miri_line,
            f"workflow runs `cargo miri` and has a cache step at line {cache_line}"))

    return out


# --------------------------------------------------------------------------
# CLI
# --------------------------------------------------------------------------


def collect(paths: list[str]) -> list[Path]:
    files: list[Path] = []
    for p in map(Path, paths):
        if p.is_dir():
            files += sorted(x for x in p.iterdir()
                            if x.suffix in (".yml", ".yaml") and x.is_file())
        elif p.is_file():
            files.append(p)
        else:
            raise ParseError(f"{p}: no such file or directory")
    return files


def audit(paths: list[str]) -> list[Finding]:
    findings: list[Finding] = []
    for f in collect(paths):
        findings += check_workflow(load_workflow(f), str(f))
    return findings


def _esc_data(s: str) -> str:
    return s.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


def _esc_prop(s: str) -> str:
    return _esc_data(s).replace(":", "%3A").replace(",", "%2C")


def _table(findings: list[Finding]) -> str:
    if not findings:
        return "No findings.\n"
    rows = ["| Rule | Severity | Location | Finding |", "|---|---|---|---|"]
    for f in findings:
        msg = f.message.replace("|", "\\|")
        rows.append(f"| {f.rule} {f.name} | {f.severity} | {f.file}:{f.line} | {msg} |")
    return "\n".join(rows) + "\n"


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="cache-secret-audit", description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("paths", nargs="*", default=[".github/workflows"],
                    help="workflow files or directories (default: .github/workflows)")
    ap.add_argument("--format", choices=["github", "json"], default="github")
    ap.add_argument("--min-severity", choices=list(SEVERITIES), default="medium")
    ap.add_argument("--no-fail", action="store_true",
                    help="report findings but exit 0")
    ap.add_argument("--version", action="version", version=__version__)
    args = ap.parse_args(argv)

    try:
        findings = audit(args.paths)
    except ParseError as exc:
        print(f"::error title=cache-secret-audit parse error::{_esc_data(str(exc))}")
        print(f"parse error: {exc}", file=sys.stderr)
        return 2

    floor = SEVERITIES[args.min_severity]
    failing = [f for f in findings if SEVERITIES[f.severity] >= floor]

    if args.format == "json":
        print(json.dumps([asdict(f) for f in findings], indent=2))
    else:
        for f in findings:
            level = "error" if SEVERITIES[f.severity] >= floor else "warning"
            body = f"{f.message}\n\nWhy: {f.why}\n\nFix:\n{f.fix}"
            print(f"::{level} file={_esc_prop(f.file)},line={f.line},"
                  f"title={_esc_prop(f.rule + ' ' + f.name)}::{_esc_data(body)}")
        table = _table(findings)
        print()
        print(table, end="")
        summary = os.environ.get("GITHUB_STEP_SUMMARY")
        if summary:
            with open(summary, "a", encoding="utf-8") as fh:
                fh.write("## cache-secret-audit\n\n" + table + "\n")

    if failing and not args.no_fail:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

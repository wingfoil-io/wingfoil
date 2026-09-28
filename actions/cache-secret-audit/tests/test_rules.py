import json
import re
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

import cache_secret_audit as csa  # noqa: E402

FIXTURES = ROOT / "tests" / "fixtures"
RULE_BY_NAME = {name: rule for rule, (name, *_) in csa.RULES.items()}


def yml(d):
    return sorted(p for p in (FIXTURES / d).iterdir() if p.suffix in (".yml", ".yaml"))


def rules_for(path):
    return [f.rule for f in csa.audit([str(path)])]


def expected_rules(path):
    """Rule IDs a fail fixture must produce, read from its filename.

    Either a leading `r1+r3-` style prefix, or a rule name
    (`secret-in-job-env-...`). `miri-repro` is the disclosure layout and
    must trip all four.
    """
    stem = path.stem
    if stem == "miri-repro":
        return {"R1", "R2", "R3", "R4"}
    m = re.match(r"^((?:r\d\+)*r\d)-", stem)
    if m:
        return {r.upper() for r in m.group(1).split("+")}
    return {rule for name, rule in RULE_BY_NAME.items() if stem.startswith(name)}


@pytest.mark.parametrize("path", yml("pass"), ids=lambda p: p.name)
def test_pass_fixtures_are_clean(path):
    assert csa.audit([str(path)]) == []


@pytest.mark.parametrize("path", yml("fail"), ids=lambda p: p.name)
def test_fail_fixtures_fire_exactly_their_rules(path):
    want = expected_rules(path)
    assert want, f"{path.name}: filename names no rule"
    assert set(rules_for(path)) == want


def test_line_numbers_point_at_the_offending_key():
    by_rule = {f.rule: f.line for f in csa.audit([str(FIXTURES / "fail" / "miri-repro.yml")])}
    assert by_rule == {"R1": 8, "R2": 11, "R3": 13, "R4": 13}


def test_github_token_is_exempt():
    assert csa.secret_names("${{ secrets.GITHUB_TOKEN }}") == []
    assert csa.secret_names("${{ github.token }}") == []
    assert csa.secret_names("${{ secrets.GITHUB_TOKEN || secrets.PAT }}") == ["PAT"]


@pytest.mark.parametrize("expr,ok", [
    ("${{ github.ref == 'refs/heads/main' }}", True),
    ("github.ref == 'refs/heads/master'", True),
    ("${{ github.event_name == 'push' && success() }}", True),
    ("${{ (github.ref == 'refs/heads/main') || github.event_name == 'push' }}", True),
    ("${{ github.event_name != 'pull_request' }}", True),
    ("${{ github.ref == 'refs/heads/main' || true }}", False),
    ("${{ github.ref != 'refs/heads/main' }}", False),
    ("${{ !(github.event_name == 'push') }}", False),
    ("${{ always() }}", False),
    (None, False),
])
def test_pr_guard(expr, ok):
    assert csa.excludes_pr(expr, pr_target=False) is ok


def test_not_pull_request_does_not_exclude_pull_request_target():
    assert not csa.excludes_pr("github.event_name != 'pull_request'", pr_target=True)


def test_on_key_is_not_yaml11_boolean(tmp_path):
    p = tmp_path / "w.yml"
    p.write_text("on: pull_request\njobs: {}\n")
    assert csa.triggers(csa.load_workflow(p)) == ["pull_request"]


def run_cli(args, capsys):
    code = csa.main(args)
    return code, capsys.readouterr().out


def test_exit_codes(tmp_path, capsys):
    assert run_cli([str(FIXTURES / "pass")], capsys)[0] == 0
    assert run_cli([str(FIXTURES / "fail")], capsys)[0] == 1
    assert run_cli([str(FIXTURES / "fail"), "--no-fail"], capsys)[0] == 0
    # R2 is medium: --min-severity high lets a medium-only file through.
    only_r2 = str(FIXTURES / "fail" / "pr-cache-save-no-save-if.yml")
    assert run_cli([only_r2, "--min-severity", "high"], capsys)[0] == 0
    bad = tmp_path / "bad.yml"
    bad.write_text("on: [push\n")
    assert run_cli([str(bad)], capsys)[0] == 2
    assert run_cli([str(tmp_path / "missing")], capsys)[0] == 2


def test_json_output(capsys):
    code, out = run_cli([str(FIXTURES / "fail" / "miri-repro.yml"), "--format", "json"], capsys)
    assert code == 1
    data = json.loads(out)
    assert sorted(d["rule"] for d in data) == ["R1", "R2", "R3", "R4"]
    for d in data:
        assert d["why"] and d["fix"] and d["line"] > 0


def test_annotations_and_step_summary(tmp_path, monkeypatch, capsys):
    summary = tmp_path / "summary.md"
    monkeypatch.setenv("GITHUB_STEP_SUMMARY", str(summary))
    code, out = run_cli([str(FIXTURES / "fail" / "miri-repro.yml")], capsys)
    assert code == 1
    assert re.search(r"^::error file=.*miri-repro\.yml,line=8,title=R1 secret-in-job-env::", out, re.M)
    # Multi-line bodies are escaped onto one annotation line.
    assert all("\n" not in line for line in out.splitlines())
    assert "| R4 miri-with-cache | high |" in summary.read_text()

---
date: 2026-04-06
topic: pr176-skill-audit-plan
status: active
pr: 176
branch: feat/iceoryx2-v2
---

# Plan: PR #176 Review + Audit (Skills + Agents)

Goal: run a repeatable, tool-assisted review and audit of PR #176 using Codex “skills” and agent reviewers, and produce actionable findings suitable for merge readiness.

Scope:
- PR: `wingfoil-io/wingfoil#176`
- Branch: `feat/iceoryx2-v2`
- Primary diff focus: `iceoryx2-beta` adapter, Python bindings, and CI operability changes.

Artifacts:
- Branch tracking: `plan.md`
- Requirements: `docs/requirements/2026-04-05-iceoryx2-adapter-requirements.md`
- Design: `docs/design/2026-04-05-iceoryx2-adapter-design.md`
- Readiness snapshot: `docs/design/2026-04-06-iceoryx2-v2-readiness-review.md`
- PR closure plan: `docs/plans/2026-04-05-003-iceoryx2-pr176-comment-closure-plan.md`

## Current Snapshot (2026-04-06)

- PR head SHA: `6a146f8`
- `mergeable`: `MERGEABLE`
- Latest reviews:
  - `0-jake-0`: `COMMENTED` (2026-04-05)
  - `github-advanced-security`: `COMMENTED` (2026-04-06)
- Checks:
  - Upstream Actions runs are present and executed.
  - Latest run IDs: 24029016531 (CI), 24029016502 (iceoryx2 (Local) Integration Tests)

## Findings Snapshot (2026-04-06)

This section summarizes the report-only audit findings from multiple lenses (correctness, security, API contract, reliability, testing).

### P0 / Merge Blockers (Decisions Required)

- **Python build enables `iceoryx2-beta` by default** (`wingfoil-python/pyproject.toml`)\n
  - Risk: likely makes the Python package effectively Linux/POSIX-only (iceoryx2 deps), which can be an unexpected packaging/API contract change.\n
  - Decision: either explicitly document “Python is Linux-only” or make `iceoryx2-beta` opt-in for Python builds.

Decision (2026-04-06):
- `iceoryx2-beta` is now **opt-in** for Python builds by default. CI explicitly enables it when running Python tests.

### P1 / Should Fix

- **Threaded/Signaled subscriber thread failures can become silent starvation** (`wingfoil/src/adapters/iceoryx2/read.rs`)\n
  - Current behavior: thread errors are logged but not propagated to the graph via `Message::Error`.\n
  - Impact: graph can keep running while adapter silently produces no data.\n
  - Suggested fix: on thread error, send `Message::Error(Arc<anyhow::Error>)` to the channel (best-effort), then exit.

- **Workflow naming vs behavior mismatch** (`.github/workflows/iceoryx2-integration.yml`)\n
  - The workflow does not run `-- --ignored`, so IPC tests remain unexecuted by CI.\n
  - Action: either rename to clarify it’s Local-only or add a conditional job to run ignored IPC tests when the environment supports shared memory IPC.

### P2 / Document or Harden

- **Resource exhaustion footguns** (history + slice sizes)\n
  - Add bounds (or clear docs) for `history_size` and `initial_max_slice_len` (DoS risk if config is user-controlled).\n
  - Document service-name capability model for IPC (no auth; rely on OS/container isolation).

- **Python test coverage regression risk**\n
  - Large deletions from `wingfoil-python/tests/test_streams.py` reduce non-iceoryx2 regression coverage.\n
  - If intentional, document rationale; otherwise restore a small core suite.

### P3 / Nice-to-have

- Logging/diagnosability improvements:\n
  - rate-limited debug logging for swallowed `update_connections()` errors\n
  - more explicit handling of non-realtime `Message` variants in receiver drains

## Phase 1: Establish Review Inputs (Deterministic)

Commands:
- `gh pr view 176 --repo wingfoil-io/wingfoil --json title,state,mergeable,updatedAt,headRefOid,headRefName`
- `gh run list --repo wingfoil-io/wingfoil --branch feat/iceoryx2-v2 --limit 10`
- Diff base refresh:
  - `git fetch upstream main`
  - `git diff --stat upstream/main..HEAD`

Acceptance:
- We know the exact head SHA and whether upstream runs are executing or blocked (`action_required`).
- We know the size/scope of the diff relative to `upstream/main`.

## Phase 2: Structured Code Review (Skill: `ce:review`)

Run the review skill in report-only mode first (no mutation):
- `ce:review mode:report-only base:upstream/main plan:docs/requirements/2026-04-05-iceoryx2-adapter-requirements.md`

Output expectations:
- Severity-ranked findings (P0–P3)
- Contract/API break risks called out explicitly
- Test gaps or flakiness risks called out explicitly

Follow-up:
- If there are actionable `safe_auto` fixes, re-run in interactive mode and apply only safe fixes:
  - `ce:review base:upstream/main plan:docs/requirements/2026-04-05-iceoryx2-adapter-requirements.md`

## Phase 3: Targeted Audits (Agent Lenses)

Run targeted audits against the diff scope (don’t broad-audit unrelated subsystems):

- **Correctness / concurrency / lifecycles**
  - Focus: thread lifecycle join guarantees, shutdown behavior, connection update timing, `collapse()` semantics in tests.
- **Security**
  - Focus: IPC/shared-memory assumptions, input validation at Python boundary, service-name validation, error context leakage (OK but ensure no secrets).
- **API contract**
  - Focus: public Rust API (re-exports, feature gating), Python API surface (argument defaults, error types).
- **Reliability / operability**
  - Focus: diagnosability of failures (structured error context), deploy docs, CI ergonomics, pre-push hook stability.

Acceptance:
- Each audit yields a short findings list and maps findings to either “must fix” vs “nice-to-have”.

## Phase 4: Merge Readiness Output

Update the closure plan snapshot:
- `docs/plans/2026-04-05-003-iceoryx2-pr176-comment-closure-plan.md`

Checklist:
- Local verification commands are recorded (Rust, clippy, Python, ignored IPC tests).
- Upstream workflow status is recorded (runs executed or `action_required`).
- Any residual risk is explicitly stated (e.g., IPC env sensitivity).

## Phase 5: Reviewer Replies (Minimal Friction)

For each blocker/recommended item:
- Reply with:
  - what changed
  - where (file path)
  - how verified (test/command)
  - what remains (if anything)

Success criteria:
- A maintainer can skim the PR thread and confirm every blocker is handled without re-deriving context from code.

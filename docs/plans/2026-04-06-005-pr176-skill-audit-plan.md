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

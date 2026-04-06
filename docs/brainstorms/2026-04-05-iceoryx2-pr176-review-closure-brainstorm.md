---
date: 2026-04-05
topic: iceoryx2-pr176-review-closure
status: active
pr: 176
---

# iceoryx2 PR #176 — Review Closure Brainstorm

## Problem Frame

The `iceoryx2` adapter work on branch `feat/iceoryx2-v2` is functionally large and touches:
- Rust adapter API + concurrency
- Python bindings
- CI workflows and integration tests
- Documentation (requirements/spec/design)

PR #176 is open and has reviewer feedback plus failing checks. We need a **repeatable loop** to:
1) collect new comments (issue comments + reviews),
2) map them to concrete actions (or explicit deferrals),
3) update docs to reflect final decisions,
4) reply to reviewers with crisp closure notes,
5) get CI green and land.

## Goals

- Make “what changed” and “what we decided” obvious for reviewers and future maintainers.
- Close reviewer threads with **explicit mapping**: comment → action → commit/files → verification.
- Keep `Local` tests as the CI default; keep `Ipc` tests opt-in or separately gated.
- Ensure failure modes are diagnosable: contract mismatch vs service open failures vs env constraints.

## Non-Goals

- Implement every “nice to have” suggestion (e.g., generalized idle strategies) before merge.
- Expand scope beyond `iceoryx2` adapter + bindings + tests + docs + CI needed for merge.
- Solve upstream/shared-memory platform issues beyond documented deployment constraints.

## Review Closure Loop (Proposed)

### Intake

- Use `gh pr view 176 --comments` to capture issue comments.
- Use `gh pr view 176 --json latestReviews` to capture review feedback.
- Use `gh pr checks 176` to capture failing checks and their URLs.

### Triage

Classify each item as one of:
- **Blocker:** must fix before merge
- **Recommended:** fix if low-risk / high leverage; otherwise defer explicitly
- **Nice to have:** defer unless it materially improves correctness/operability

### Closure

For each item, produce one of:
- **Fixed** (link to files / test)
- **Won’t fix** (reason + follow-up plan link)
- **Needs decision** (explicit decision request)

## Success Criteria

- PR #176 is mergeable (no conflicts) and CI required checks pass.
- Reviewer “Blockers” have an explicit closure response.
- Requirements/spec/design docs reflect the final behavior (modes, contracts, defaults, error surfaces, test gating).

## Open Questions

- What is the project’s effective MSRV policy (given `edition = "2024"`)? If MSRV is high, “`is_multiple_of()` is unstable” is moot; if MSRV is lower, prefer `%` in hot paths to avoid churn in CI.
- Should “idle strategy” become a first-class API now (risk: scope creep), or be tracked as a follow-up after merge?

## Current Decision (Proposed)

- Prefer conservative compatibility in hot paths unless MSRV is explicitly documented: replace `is_multiple_of()` with `%` checks to avoid toolchain churn for reviewers/CI.
- Treat “IdleStrategy” as a follow-up unless it becomes necessary to close a correctness/operability blocker.

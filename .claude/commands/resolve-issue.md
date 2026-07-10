Resolve an open GitHub issue in `wingfoil-io/wingfoil` end-to-end: review the open issues, pick the highest-priority *resolvable* one (or the specific issue named in `$ARGUMENTS`), propose a fix, implement it, verify it, commit, and open a pull request. This is the issue-resolution workflow — follow every step in order.

`$ARGUMENTS` is optional. If it names an issue number (e.g. `140`) resolve that issue directly, skipping selection in step 2. If empty, run the full triage-and-select flow.

## 0. Guardrails

- Obey `CLAUDE.md`. In particular: production code must not call `.unwrap()` (use `?` / `.expect("invariant: …")`), and the pre-commit checklist (`cargo fmt --all`, `cargo lint`, `cargo lint-all`) must pass before any commit.
- Never edit on `main`. Do all work on the branch you were assigned for the session; if none was assigned, create `resolve-issue-<number>` from an up-to-date `main`.
- Do exactly one issue per invocation. When asked to "do the next issue", re-invoke this skill — do not batch several issues into one diff.
- Do not invent labels, milestones, or assignees. Do not close the issue yourself; the merged PR does that via a closing keyword.

## 1. Review all open issues

- Call `mcp__github__list_issues` (state `OPEN`, `perPage` up to 100). The result can be large — if it exceeds the token limit it is written to a file; parse that file with `python3`/`jq` to extract `number`, `title`, and `labels` rather than re-reading raw.
- Also check the structured org **Priority** field via `mcp__github__list_issue_fields` and `field_filters` (values `Urgent` > `High` > `Medium` > `Low`). If that field is unset across issues, fall back to the `priority: <urgent|high|medium|low>` **labels**.

## 2. Pick the highest-priority *resolvable* issue

Rank by priority first, then break ties by tractability. An issue is **resolvable in one PR** only if all of these hold:

- It is self-contained enough to implement, test, and verify in a single focused diff.
- It does **not** depend on a heavy new external service, a large new dependency stack, or network/hardware unavailable in this environment (e.g. a new ML runtime, a live broker, GPU) that would prevent a green, verifiable build here.
- Its acceptance criteria are concrete enough to know when it is done.

Selection rule: walk the priority bands from highest to lowest and pick the first issue that is resolvable in one PR. If the highest-priority issues are all `size: large` epics or blocked on external systems, **say so explicitly** in your proposal, then select the highest-priority issue that *is* resolvable. Never pick a larger, lower-priority issue over a resolvable higher-priority one, and never silently downgrade — always state which higher-priority issues you skipped and why.

Read the chosen issue in full with `mcp__github__issue_read` (`method: "get"`), including its comments, before proposing anything.

## 3. Propose the fix

Before writing code, ground the proposal in the actual source: locate the files and current signatures named in the issue and confirm the issue's description still matches reality (code moves; parts may already be done). Then post a short proposal to the user containing:

- **Issue** — number, title, priority, and the one-line problem.
- **Skipped** — any higher-priority issues you passed over, with the reason (only when you did not pick the top one).
- **Plan** — the concrete change: files to touch, signatures to add/alter, behaviour, and any backwards-compatibility implications (a new required parameter on a public fn is a breaking change — call it out and update all call sites/examples).
- **Verification** — the tests you will add and how you will exercise the change (historical-mode run, new unit test, example still builds).

Keep it tight. Then **stop and ask the user for permission to implement** — always. Do not begin writing code until the user has explicitly approved the proposal. Present the plan and wait for a clear go-ahead (use `AskUserQuestion` to confirm, and to resolve any genuinely load-bearing decision that is ambiguous). If the user asks for changes, revise the proposal and ask again. Only proceed to step 4 once the user has approved.

## 4. Implement

- Follow the codebase's existing patterns (mirror the closest analogous node/adapter; see `CLAUDE.md` → Custom Nodes, Error Handling, Testing Conventions).
- Add unit tests using historical mode for determinism (`RunMode::HistoricalFrom(NanoTime::ZERO)`), with hand-verified expected values.
- Update every affected call site — examples, benches, Python bindings, docs — so the whole workspace still builds.

## 5. Verify (do not skip)

Run, in order, and fix anything that fails before committing:

```bash
cargo fmt --all
cargo lint          # default features
cargo lint-all      # all features — catches feature-gated code
cargo test -p wingfoil    # plus any feature flags your change touches
```

Actually exercise the new behaviour (run the relevant test or example and read the output) — a passing typecheck is not verification. If the change is feature-gated, test with that feature enabled.

## 6. Commit and push

- Stage only files relevant to this issue. Write a clear message referencing the issue, e.g. `Add buffer_size to produce_async and kdb_read (#140)`.
- End the commit body with the `Co-Authored-By` / `Claude-Session` trailers required for this session.
- Push with `git push -u origin <branch>`; retry with exponential backoff on network errors only.

## 7. Open the pull request

- Only open a PR when the change is complete and verified. Check for a PR template (`.github/pull_request_template.md` / `.github/PULL_REQUEST_TEMPLATE.md`); if present, mirror its headings.
- Title: concise summary. Body: what changed, why, how it was verified, and a closing keyword (`Closes #<number>`) so the merge closes the issue.
- Report the PR number and URL back to the user, then offer to watch it for CI/review activity via `subscribe_pr_activity`.

## 8. Next issue

Stop after one issue. If the user asks for the next one, re-invoke this skill from step 1 (the just-resolved issue will now be excluded because its PR is open). Respect the session's branch rules — if you must keep all work on one assigned branch, note that separate PRs may not be possible and confirm the batching approach with the user before stacking a second unrelated change onto the same branch.

Triage every open GitHub issue in `wingfoil-io/wingfoil`: find ones that are already done and can be closed, verify each has the right labels, curate a healthy set of `good first issue`s, and flag any issue that no longer makes sense or that we don't actually want to fix. This is a **review-and-report** workflow — it audits the issue tracker and proposes changes; it does not resolve issues (use `/resolve-issue` for that) and it never mutates an issue until you have approved the change.

`$ARGUMENTS` is optional. If it names one or more issue numbers (e.g. `140 148`), audit only those. If empty, audit every open issue.

## 0. Guardrails

- **Read-only by default.** Producing the report costs nothing and is always safe. Closing, relabeling, or editing an issue is outward-facing and visible to everyone watching the repo — do **not** apply any such change until the user has explicitly approved it (see step 7).
- **Do not invent labels, milestones, or assignees.** Only propose labels that already exist; verify with `mcp__github__get_label` before proposing any label you are not certain of. The label vocabulary in this repo includes: `enhancement`, `bug`, `io-adapter`, `performance`, `monitoring`, `good first issue`, and the `priority: <urgent|high|medium|low>` / `size: <small|medium|large>` families. There is also a structured org **Priority** field (`mcp__github__list_issue_fields`) — prefer it over `priority:` labels when both exist.
- **Never close an issue on a hunch.** "Complete" means you found the merged PR / commit / current source that satisfies its acceptance criteria. Cite the evidence; if you cannot, it is not a close candidate — it is a "needs a human to confirm" candidate.
- Do not resolve, branch, or write code here. This skill only audits and, with approval, applies label/state changes.

## 1. Pull every open issue

- Call `mcp__github__list_issues` (state `OPEN`, `perPage` up to 100). The result is large and will likely be written to a file — parse it with `python3`/`jq` for `number`, `title`, `labels`, `body`, `created_at`, `updated_at` rather than re-reading raw. Note that `minimal_output: true` **drops labels**, so use the full output (or fetch labels per-issue) when the audit needs them.
- Also fetch the structured **Priority** field values via `mcp__github__list_issue_fields` if you will comment on prioritization.
- Build a working table: `number | title | labels | age | one-line summary`. This table anchors every pass below.

## 2. Pass A — completed / closable

For each issue, decide whether it is already satisfied by merged work:

- Search the codebase for the symbols, files, or behavior the issue asks for (`mcp__github__search_code`, or grep the local checkout).
- Search merged PRs and recent commits that reference the issue number (`mcp__github__search_pull_requests` / `search_issues` with `is:merged`, `git log --grep`).
- Read the issue's own comments — someone may have already noted it's done, or superseded.

Classify each as: **Done** (cite the PR/commit/source that closes it — recommend closing with `state_reason: completed`), **Superseded/duplicate** (cite the issue it duplicates — recommend `not_planned`), or **Still open**. Only Done/Superseded issues become close proposals, each with its evidence.

## 3. Pass B — labels

For every issue check the labels are correct and complete:

- **Type**: every issue should carry exactly one of `enhancement` / `bug`. Adapter issues additionally carry `io-adapter` (this repo filters on it — flag any adapter issue missing it). Perf work carries `performance`; monitoring/observability work carries `monitoring`.
- **Priority & size**: flag issues missing a priority signal (label or org field) and, for `enhancement`s, a `size:` estimate — but only *propose* a value where the body makes it obvious; otherwise mark it "needs triage" rather than guessing.
- **Wrong/stale labels**: flag labels that no longer match the issue's current scope.

Output the delta only: for each issue, the labels to **add** and to **remove**, or "labels OK".

## 4. Pass C — good-first-issue health

Assess the tracker's supply of newcomer-friendly work:

- List issues currently carrying `good first issue`.
- Independently scan for issues that *should* qualify — small, self-contained, clear acceptance criteria, no dependency on a heavy external service/hardware, touching a well-trodden part of the codebase (a single node/operator, a Python binding for an existing Rust fn, a localized cleanup). Cross-reference the tractability bar in `/resolve-issue` step 2.
- Report: the current count, which existing `good first issue`s no longer qualify (too big/stale — propose removing the label), and a ranked shortlist of issues to **promote** to `good first issue`, each with one line on why it's approachable. Aim to surface a healthy spread (roughly 3–6 live ones) across different areas so a newcomer has real choice.

## 5. Pass D — does it still make sense / do we want it?

For each issue, sanity-check that it's still worth doing:

- **Still real?** Does the described problem/feature still match the current code, or has the code moved on (partial fixes, refactors, renamed APIs)? Flag drift.
- **Worth fixing?** Is it aligned with the project's direction, or is it speculative, out of scope, vague to the point of being unactionable, or contradicted by a later decision? Note anything that reads like it should be **closed as `not_planned`** or **needs the author to clarify scope** before it's actionable.
- Watch for the self-flagged ones — e.g. an issue whose body says "filed here but belongs in another repo / tracker" should be surfaced for the user to relocate or close.

Be honest and specific; the value of this pass is catching the issues nobody wants to admit are dead.

## 6. Report

Post a single, skimmable report to the user, organized by the four passes, e.g.:

- **Closable (N)** — `#123 title` → done by `#456` / `commit abc` (evidence).
- **Label fixes (N)** — `#123` → +`io-adapter`, −`priority: low`.
- **Good first issues** — current: N live. Promote: `#123` (why). Demote: `#456` (why).
- **Questionable (N)** — `#123` → likely `not_planned` because …; `#789` → belongs in `kes` repo.

Lead with the highest-signal items. Keep each line to one sentence of evidence. Do not bury a "close this" among label nits.

## 7. Apply changes — only with approval

Stop after the report and **ask the user which proposals to apply** (use `AskUserQuestion` for the batch: close set, label set, good-first-issue set). Never apply proactively. Once approved, and only for the approved subset:

- **Close**: `mcp__github__issue_write` with the appropriate `state_reason` (`completed` vs `not_planned`). Leave a one-line closing comment citing the evidence so the trail is clear.
- **Relabel**: `mcp__github__issue_write` to add/remove exactly the approved labels. Verify each label exists first.
- Re-report what was actually changed (issue numbers + the action taken). If a mutation fails, say so plainly rather than assuming success.

Leave everything the user did not approve untouched.

---
name: auto
description: >-
  Eager autonomous-completion mode for working through a sequence of tasks
  (resolving a batch of issues, executing a multi-step plan, clearing a
  backlog). Activate when the user says to work through issues/tasks "on
  auto", "autonomously", "eagerly", "one after another", "don't ask", or
  "keep going without me". Changes the working posture: complete as much as
  possible without pausing for approval, open pull requests automatically
  (no permission prompt), and start the next branch/PR immediately instead of
  waiting for the previous one to merge. Does NOT relax the codebase's
  quality gates (fmt/lint/tests, verification).
---

# auto — eager autonomous completion

`auto` is a **posture modifier**, not a standalone workflow. It wraps the
normal task loop (e.g. `/resolve-issue`, working through a plan, clearing a
list of open issues) and changes *how* you drive it, not *what* the underlying
work is. Activating `auto` is the user's explicit, standing authorization for
the three behaviours below — you do not need to re-ask for them per task.

## What changes while auto is active

1. **Work eagerly, end to end.** When there is a sequence of tasks in front of
   you — a batch of open issues, the steps of a plan the user handed you —
   carry it forward as far as you can in one go. Do not stop after a single
   unit and wait to be told "next"; pick the next resolvable item and continue.
   Stop only when the sequence is exhausted, the remaining items are genuinely
   blocked (see *Where you still stop*), or the user interrupts.

2. **Open the PR automatically — do not ask.** The base rule "don't open a PR
   unless explicitly asked" and `/resolve-issue`'s "stop and ask permission to
   implement" are **satisfied by the user activating `auto`**. Once a change is
   complete and verified, commit, push, and open the PR without a permission
   prompt. Report the PR number/URL and keep moving.

3. **Don't wait for merge before the next branch.** A PR being open is not a
   reason to idle. Immediately branch for the next task and start it. Each task
   gets its **own fresh branch cut from up-to-date `main`**
   (`git checkout main && git pull origin main && git checkout -b <descriptive-name>`)
   and its **own PR** — do not stack unrelated changes onto one branch. If the
   session pinned you to a single assigned branch, activating `auto` is the
   explicit permission to create additional per-task branches; prefer one
   branch + one PR per task so reviews stay independent.

## Where you still stop (auto does not override these)

`auto` removes *approval friction and serialization*, not *judgement and
quality*. You must still:

- **Keep every quality gate green.** Run the full pre-commit checklist before
  each commit — `cargo fmt --all`, `cargo lint` (default features),
  `cargo lint-all` (all features — catches feature-gated code), and the
  relevant `cargo test` targets. Actually exercise the change (run the test or
  example and read the output); a passing typecheck is not verification. Never
  open a PR on red.
- **Follow the codebase rules.** Never edit on `main`; production code must not
  call `.unwrap()` (use `?` / `.expect("invariant: …")`); add tests in
  historical mode (`RunMode::HistoricalFrom(NanoTime::ZERO)`) with
  hand-verified expected values; update every affected call site (examples,
  benches, Python bindings) so the whole workspace still builds.
- **Escalate genuine ambiguity.** If a task has a load-bearing decision with
  no clear default, the acceptance criteria are unclear, or a change would
  restructure the public API/architecture, pause and ask (`AskUserQuestion`)
  rather than guessing — even in auto mode. Eager ≠ reckless.
- **Skip, don't force, the intractable.** An item that needs an unavailable
  service, a large new dependency stack, or a change you cannot verify here is
  not "resolvable in one PR". Note that you skipped it and why, then move to
  the next resolvable item.

## Operating loop

For each item in the sequence:

1. Select the next resolvable item (highest priority first, per the underlying
   workflow's selection rule).
2. Implement it, mirroring the closest existing node/adapter and the
   error-handling rules in `CLAUDE.md`.
3. Verify: run the quality gates above and exercise the behaviour.
4. Commit on a fresh per-task branch (with the session's required commit
   trailers), push with `git push -u origin <branch>`.
5. Open the PR automatically with a body that says what changed, why, how it
   was verified, and a `Closes #<n>` keyword when it resolves an issue.
6. Without waiting for CI or merge, go to step 1 for the next item.

Keep a short running tally so the user can see progress (item → branch → PR
URL → status). When the sequence is done, summarise every PR you opened.

## De-activating

Auto stays in effect for the rest of the session's batch unless the user says
to stop, slow down, or go back to asking per step. When they do, revert to the
default cautious posture (ask before opening PRs, one unit at a time).

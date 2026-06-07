Fetch GitHub Actions CI logs for `$ARGUMENTS` and surface the failure. `$ARGUMENTS` may be a job ID, a run ID, a branch name, or a PR number — resolve it to a failed job ID, then dump the relevant log lines. Read-only: never push, comment, or rerun workflows.

This skill wraps `scripts/ci-logs.sh`, which resolves a job's signed log URL via the GitHub API and downloads it. The signed URL carries its own auth, so the bearer header is intentionally **not** forwarded to it.

## 1. Resolve `$ARGUMENTS` to a failed job ID

`scripts/ci-logs.sh` takes a **job** ID, not a run ID. Resolve whatever the user gave you:

- **Full Actions URL** (e.g. `.../actions/runs/<run_id>/job/<job_id>?pr=<n>`) —
  parse the IDs from the **path only**. Use the `job/<job_id>` segment directly
  as the job ID; if only `runs/<run_id>` is present, resolve it as a Run ID below.
  **Ignore `?pr=` and every other query parameter.** The `?pr=` value is a UI
  scoping hint, not part of the job's identity, and it can be flat-out wrong — a
  run is associated with the PR whose branch triggered it, which need not match
  whatever `?pr=` the user copied. Trust the path's job/run IDs over the query
  string, always. (A mismatched `?pr=` is also why the web page 404s even though
  the run and job exist.)
- **Bare numeric job ID** (the script's native input) — use it directly.
- **Run ID** — list its jobs and pick the failed one:
  `GET /repos/wingfoil-io/wingfoil/actions/runs/<run_id>/jobs`
- **Branch name** — find the latest run, then its failed job:
  `GET /repos/wingfoil-io/wingfoil/actions/runs?branch=<branch>&per_page=1`
- **Bare PR number** (not from a URL) — read the PR's head branch
  (`mcp__github__pull_request_read`), then resolve as a branch. Do **not** derive
  a PR number from a URL's `?pr=` — see the Full Actions URL note above.
- **Nothing / "the failing one"** — list recent failures and pick the most recent:
  `GET /repos/wingfoil-io/wingfoil/actions/runs?per_page=10&status=failure`

Prefer the `mcp__github__*` tools for PR/branch metadata. There is no MCP tool for workflow runs or job logs in this repo's server, so the run/job lookups above and the log download itself go through the API via `curl` with `$GH_TOKEN`. Confirm the job's `conclusion` is `failure` and note which step failed before fetching.

## 2. Fetch the logs

```bash
scripts/ci-logs.sh <job_id>
```

Pipe through `tail -60` (or `grep` for `error\|panicked\|FAILED\|assertion`) rather than dumping the full log into the conversation. Lead with the failing step name and the smallest excerpt that explains the failure.

## 3. Network caveat (Claude Code on the web)

GitHub serves Actions logs as a short-lived signed URL on `*.blob.core.windows.net`. In a remote/web environment the network policy may allowlist `api.github.com` but not blob storage — the second `curl` then fails with `Host not in allowlist` and no log body is returned.

If that happens, do not treat it as a script bug. Report it and offer the options:
1. Run `scripts/ci-logs.sh <job_id>` somewhere with open egress (it works as-is).
2. Loosen the environment's network policy to allow `*.blob.core.windows.net`, then rerun (see https://code.claude.com/docs/en/claude-code-on-the-web).

Either way, still report what you learned from the API: failing workflow, branch, job, and which step failed.

## 4. Reporting

- One-line summary: workflow → job → failing step.
- The minimal log excerpt that pinpoints the cause.
- A suggested next step (the fix, or a rerun) — but do not act on it unless asked. This skill only reads.

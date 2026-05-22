# LinkedIn — `/new-adapter` skill

## Post

One of the things we kept hitting while adding adapters to Wingfoil — Kafka, websockets, KDB, the usual — was that the answers to "how should this be structured?" are the same every time, but they're easy to get subtly wrong.

Threading model. Where the feature flag goes. How the integration test starts its container. Whether the Python binding gets done now or later. None of it is hard, but a year later you find one adapter that does it differently and you're not sure why.

So we wrote it down as a Claude Code skill: `/new-adapter`. It's a 15-step checklist that produces a new adapter with a consistent file layout, one of three vetted threading models (async, channel, spin), feature-gated integration tests via testcontainers, PyO3 bindings, and CI wired up.

There's a smaller companion, `/new-adapter-issue`, that just files the scoping issue on GitHub with the same structure.

This is honestly more useful for our own future selves than for any one new adapter. If you're maintaining a library with a similar adapter pattern, the skill files are short and probably worth a skim.

## First comment

- `/new-adapter` skill: https://github.com/wingfoil-io/wingfoil/blob/main/.claude/commands/new-adapter.md
- `/new-adapter-issue` skill: https://github.com/wingfoil-io/wingfoil/blob/main/.claude/commands/new-adapter-issue.md
- Existing adapters as reference: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/src/adapters
- Claude Code skills docs: https://docs.claude.com/en/docs/claude-code/skills

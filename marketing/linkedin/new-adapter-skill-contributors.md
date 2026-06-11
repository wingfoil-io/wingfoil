# LinkedIn — `/new-adapter` skill (call for contributors)

## Post

We're opening up I/O adapter contributions to Wingfoil, and there's now a tool that makes it worth your time.

Some background. An adapter — Kafka, etcd, ZMQ, KDB+, and the rest — used to take us about a month to land. Most of that wasn't the protocol code. It was the surrounding decisions: where the feature flag goes, which threading model fits the client library, how the integration test starts a container, whether the Python binding ships in the same PR. We wrote those decisions down as a Claude Code skill, `/new-adapter`, reverse-engineered from the adapters we'd already built by hand. The last few took a weekend each.

The part worth flagging for anyone thinking about contributing: the skill didn't only make adapters faster, it made them more consistent, and that turned out to matter more. Same file layout, same test scaffolding, same feature gating every time. A reviewer who's read one adapter can read the next without relearning where things live, and the "why is this one shaped differently" class of bug mostly stopped happening.

So the ask. If there's a transport or store you want Wingfoil to talk to, run `/new-adapter <name>` and build it. You don't have to reverse-engineer our conventions first — the skill carries them, including the bits that are easy to forget, like CI registration and the example index. We have a handful specced out as issues already if you'd rather pick one up than start cold, and the other open issues are fair game too.

Links in the first comment.

## First comment

- `/new-adapter` skill: https://github.com/wingfoil-io/wingfoil/blob/main/.claude/commands/new-adapter.md
- Existing adapters as reference: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/src/adapters
- Adapters already specced out:
  - `ws_client` (outbound WebSocket feeds): https://github.com/wingfoil-io/wingfoil/issues/148
  - `sql` (relational DBs): https://github.com/wingfoil-io/wingfoil/issues/105
  - `arrow` (Arrow IPC + Parquet): https://github.com/wingfoil-io/wingfoil/issues/104
  - `candle` (neural inference on streams): https://github.com/wingfoil-io/wingfoil/issues/239
  - `augurs` (time-series analytics): https://github.com/wingfoil-io/wingfoil/issues/238
- All open adapter issues: https://github.com/wingfoil-io/wingfoil/issues?q=is%3Aissue+is%3Aopen+label%3Aio-adapter
- Claude Code skills docs: https://docs.claude.com/en/docs/claude-code/skills

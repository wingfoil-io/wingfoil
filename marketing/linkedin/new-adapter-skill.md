# LinkedIn — `/new-adapter` skill

## Post

A while back we sat down and counted how long it actually took us to add a new I/O adapter to Wingfoil. The honest answer was about a month — most of which wasn't writing protocol code, it was rediscovering decisions: where the feature flag goes, which threading model fits, how to start an integration test container, whether the Python binding lands now or later.

We turned that into a Claude Code skill called `/new-adapter`. The last few adapters we've shipped took a weekend each.

The way we built it was to reverse-engineer the steps from adapters we'd already implemented by hand, looking at what they had in common and writing that down. The skill isn't finished — every time we hit a case that doesn't fit the existing template (a protocol that needs its own threading shape, an adapter that skips Python bindings for now), we go back and update the skill to describe the new variant and when to reach for it.

The thing we didn't expect is that the skill made the adapters better, not just faster. Consistency turns out to be a quality multiplier — when every adapter has the same file layout, the same test scaffolding, the same feature gating, reviews are easier and the "why is this one different?" class of bugs mostly stops happening.

Short version: a checklist we wrote down so future-us would stop relearning the same lessons.

## First comment

- `/new-adapter` skill: https://github.com/wingfoil-io/wingfoil/blob/main/.claude/commands/new-adapter.md
- Existing adapters as reference: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/src/adapters
- Claude Code skills docs: https://docs.claude.com/en/docs/claude-code/skills

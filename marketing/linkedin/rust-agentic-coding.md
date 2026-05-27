# LinkedIn — Rust and agentic coding

## Post

Last week Bun merged a 6,700-commit PR that ports roughly 960,000 lines of Zig to Rust — generated end-to-end by Claude agents in about six days. The new branch passes the existing test suite on every platform, shrinks the binary by 3–8 MB, and fixes a handful of memory leaks along the way.

It's a striking data point, and it's got us talking again about why we picked Rust for Wingfoil — especially in light of how much of the day-to-day coding now happens with an agent in the loop.

Our take, briefly: the strictness we used to think of as friction has turned into the most useful collaborator we've got.

When an agent writes a chunk of code that's almost right but quietly wrong — a value moved into a closure that's then used again, an `Rc<T>` shared across a thread boundary that would be a data race at runtime, a match arm that silently falls through when a new enum variant gets added — the compiler stops it at the door. The agent reads the error, fixes it, and tries again. The loop is tight, deterministic, and the feedback is precise: a line number and a sentence of English.

The alternative loop — write code, run it, observe the wrong behaviour, narrow down where it went wrong, write a guess at a fix, run again — is the one that burns tokens and wall-clock time. Compiler-as-collaborator turns out to be genuinely much cheaper.

It's not pure upside. The Bun port reportedly carries around 13,000 `unsafe` blocks where a comparable native-Rust runtime like uv has closer to 70. That's the natural shape of a fast mechanical translation — you transliterate first, refactor for idiom later — and it's worth watching whether the Bun team gradually walks those numbers down.

It's also not a point against Zig. Zig is a thoughtful language with a strong community. But it's a fair question worth sitting with: as more code gets written with agent assistance, does the balance tilt toward languages whose type systems catch more before runtime? Or is the gap smaller than it looks once test generation is automated too?

Genuinely interested in how others are thinking about this.

## First comment

- Wingfoil (the Rust stream-processing graph we're building on this thesis): https://github.com/wingfoil-io/wingfoil
- Bun: https://github.com/oven-sh/bun
- The Register's write-up of the merge: https://www.theregister.com/devops/2026/05/14/anthropics-bun-rust-rewrite-merged-at-speed-of-ai/
- DevClass coverage: https://www.devclass.com/software/2026/05/11/anthrophics-bun-team-trials-port-from-zig-to-rust/

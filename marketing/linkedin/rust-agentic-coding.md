# LinkedIn — Rust and agentic coding

## Post

The news that Bun is moving parts of itself from Zig to Rust got us talking again about why we picked Rust for Wingfoil — especially in light of how much of the day-to-day coding now happens with an agent in the loop.

Our take, briefly: the strictness we used to think of as friction has turned into the most useful collaborator we've got.

When an agent writes a chunk of code that's almost right but quietly wrong — a borrow that won't survive an async boundary, a `&str` where the API wanted a `String`, a match arm that missed a variant — the compiler stops it at the door. The agent reads the error, fixes the issue, and tries again. The loop is tight, deterministic, and the feedback is precise: a line number and a sentence of English.

The alternative loop — write code, run it, observe the wrong behaviour, narrow down where it went wrong, write a guess at a fix, run again — is the one that burns tokens and wall-clock time. Compiler-as-collaborator turns out to be genuinely much cheaper.

This isn't a point against Zig. Zig is a thoughtful language with a strong community and good ideas about comptime and metaprogramming. It's a fair question, though: as more code gets written with agent assistance, does the balance tilt toward languages whose type systems catch more before runtime? Or is the gap smaller than it looks once test generation is automated too?

Genuinely interested in how others are thinking about this.

## First comment

- Wingfoil (the Rust stream-processing graph we're building on this thesis): https://github.com/wingfoil-io/wingfoil
- Bun: https://github.com/oven-sh/bun
- Zig: https://github.com/ziglang/zig

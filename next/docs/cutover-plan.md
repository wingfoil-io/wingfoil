# Cutover plan — replacing the legacy tree with Wingfoil Next

Wingfoil Next is being built to replace the legacy `wingfoil` tree wholesale.
This document holds the two goals that govern that cutover and the current
status. The phase-by-phase roadmap, the capability matrix, and the gates live
in [`port-plan.md`](port-plan.md).

## Goals

### 1. A strict superset of legacy wingfoil — including examples

Before cutover, everything the legacy tree offers must exist here: every
node/operator, every adapter, every run mode and execution pattern, the
examples, benchmarks, language bindings and docs. Where next deliberately
deviates (e.g. by-design `compiled()` restrictions), the deviation is
documented in the capability matrix in [`port-plan.md`](port-plan.md) — never
left implicit. Anything legacy does that next cannot do (or has not explicitly
ruled out) is a cutover blocker.

### 2. Ready to swap out the legacy tree wholesale

The `next/` folder mirrors the legacy repo root — `README`, `LICENSE`,
`CONTRIBUTING`, `docs/`, and the crates under `crates/` — so the eventual
cutover is a directory promotion, not a re-organisation. Until then, the
legacy crates keep shipping untouched and serve as the permanent parity oracle
for the port.

## Status

Porting is in progress, phase by phase, with the legacy test suite as the
parity oracle — see [`port-plan.md`](port-plan.md) for the live ✅/🟡/⬜ state.
The port can pause at any phase boundary with everything shipped still
correct; the legacy crates remain the production engine until the superset
objective above is met.

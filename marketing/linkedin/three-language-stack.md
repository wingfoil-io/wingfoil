# LinkedIn — three-language stack (Rust / Python / TypeScript)

## Post

Here's a position I'll defend: for a greenfield project in 2026, three languages cover almost everything that matters, and the rest are mostly kept alive by inertia rather than merit.

- **Rust for the systems layer** — C and C++ performance with memory safety out of the box. Infrastructure, high-throughput services, anything resource-constrained.
- **Python for data and orchestration** — not fast, but the non-negotiable interface for AI/ML, data science, and prototyping. That isn't changing soon.
- **TypeScript for the interface** — it owns the browser, and will until WebAssembly is ready to displace the frontend frameworks.

That's the split we landed on at Wingfoil. Wingfoil is a graph-based stream processing framework: the core is Rust for predictable latency and memory safety, the runtime is exposed to Python through PyO3 so data engineers and scientists can drive it natively, and the edge integrates with JS/TS to move data in and out of browser and web environments. Performance where it matters, productivity where it counts.

So where does that leave C++, Java, C#, and Go for *new* work? Strip away the marketing and I think the honest reasons usually come down to two, and neither is technical:

- **Codebase inertia** — there's a large, working footprint you have to extend or integrate with.
- **Talent constraints** — the hiring pool in your org or region is weighted toward those ecosystems.

Both are real, and on a given Monday they're often decisive. But they're constraints you've inherited, not capabilities the language uniquely brings. On a clean slate, Rust + Python + TypeScript handles almost any modern requirement with less sprawl and sharper specialization.

Tell me where this breaks. What's the workload — a real one, not a hypothetical — where reaching for the JVM, Go, or C++ on a new project is the *technical* call and not the inherited one?

## First comment

- Wingfoil on GitHub (a star helps if the architecture is useful to you): https://github.com/wingfoil-io/wingfoil/
- Python bindings via PyO3: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil-python
- WebSocket / JS edge: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/src/adapters/web

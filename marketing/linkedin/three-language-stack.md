# LinkedIn — three-language stack (Rust / Python / TypeScript)

## Post

Rust, Python, and TypeScript have quietly become the foundation a lot of new projects build on. Not because they crowd everything else out, but because between them they cover three distinct jobs well.

- **Rust for the systems layer** — bare-metal performance with memory safety out of the box. It keeps showing up in infrastructure, high-throughput services, and anywhere resources are tight.
- **Python for data and orchestration** — not the fastest at raw execution, but the default interface for AI/ML, data science, and prototyping, and that isn't changing soon.
- **TypeScript for the interface** — it owns the browser, and will until WebAssembly is ready to displace the frontend frameworks.

That's the split we landed on at Wingfoil. Wingfoil is a graph-based stream processing framework: the core is Rust for predictable latency and memory safety, the runtime is exposed to Python through PyO3 so data engineers and scientists can drive it natively, and the edge integrates with JS/TS to move data in and out of browser and web environments. Performance where it matters, productivity where it counts.

To be clear, this isn't a claim that three is the magic number, or that the rest are on the way out. Plenty of solid systems run on Java, C#, C, C++, and Go for good reasons — some technical, some about the codebase and team you already have, which are legitimate engineering inputs, not excuses.

So the open question, not the rhetorical one: if Rust, Python, and TypeScript are the foundation, what else earns a place at the table, and for which jobs? Go for network services? Something for the JVM ecosystem you can't easily give up? Curious where other people draw the line and why.

## First comment

- Wingfoil on GitHub (a star helps if the architecture is useful to you): https://github.com/wingfoil-io/wingfoil/
- Python bindings via PyO3: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil-python
- WebSocket / JS edge: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/src/adapters/web

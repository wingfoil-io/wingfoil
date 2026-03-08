# LinkedIn Post Draft

---

**Wingfoil is going big on data.**

We've been heads-down building, and I'm excited to share what's landed recently in [wingfoil](https://github.com/wingfoil-io/wingfoil) — our Rust stream processing library for high-performance DAG-based pipelines.

**What's shipped:**

We've landed a production-ready **KDB+ adapter** — connect your streams directly to KDB+ time-series databases with full symbol interning.

This is production-grade — designed for the latency and throughput demands of real-time financial data.

---

**Huge shoutout to our contributors who are making it happen:**

- **Zohaib Hassan** — Pandas integration for seamless Python interop
- **Aditya Shirsat** — time-based windowing and Python examples
- **Matvey Ezhov** — code quality improvements across the board

Open source only works because of people like you. Thank you.

---

**Where we're going next — and where we need help:**

We're looking for contributors to tackle:

- **ZMQ** — our messaging layer is in beta and we're pushing it to [production-ready](https://github.com/wingfoil-io/wingfoil/issues/52). That includes adding [**service discovery**](https://github.com/wingfoil-io/wingfoil/issues/103) for dynamic node registration and topology management.
- [**KDB+ caching**](https://github.com/wingfoil-io/wingfoil/issues/90) — smarter replay and snapshot support
- [**Binary file I/O**](https://github.com/wingfoil-io/wingfoil/issues/104) — high-speed serialisation adapters (Arrow, Parquet, etc.)
- [**SQL I/O**](https://github.com/wingfoil-io/wingfoil/issues/105) — stream to/from relational databases
- [**Kafka I/O**](https://github.com/wingfoil-io/wingfoil/issues/23) — first-class event streaming integration

If you work in quant finance, real-time data, or just love high-performance Rust — we'd love your help. We're especially keen to hear from **PyO3** and **FPGA** specialists. Drop a comment, open an issue, join us on [Discord](https://discord.gg/WfZwpQnZUA) or just star the repo.

Rust + streams + data. Let's build.

🔗 https://github.com/wingfoil-io/wingfoil

#Rust #StreamProcessing #BigData #OpenSource #KDB #Kafka #FinTech #DataEngineering

---
*Draft — tags to add before posting: @Zohaib Hassan, @Aditya Shirsat, @Matvey Ezhov (find their LinkedIn profiles to tag directly)*

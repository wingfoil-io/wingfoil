# LinkedIn Post Draft

---

## POST (no links — paste this as the post itself)

**Wingfoil is going big on data.**

We've been heads-down building, and I'm excited to share what's landed recently in wingfoil — our Rust stream processing library for high-performance DAG-based pipelines.

**What's shipped:**

We've landed a production-ready **KDB+ adapter** — connect your streams directly to KDB+ time-series databases with full symbol interning. This is production-grade — designed for the latency and throughput demands of real-time financial data.

---

**Huge shoutout to our contributors who are making wingfoil happen:**

- **Zohaib Hassan** — Pandas integration for seamless Python interop
- **Aditya Shirsat** — time-based windowing and Python examples
- **Matvey Ezhov** — code quality improvements across the board

Open source only works because of people like you. Thank you.

---

**Where we're going next — and where we need help:**

We're looking for contributors to tackle:

- **ZMQ** — our messaging layer is in beta and we're pushing it to production-ready. That includes adding **service discovery** for dynamic service registration.
- **KDB+ caching** — smarter replay and snapshot support
- **Binary file I/O** — high-speed serialisation adapters (Arrow, Parquet, etc.)
- **SQL I/O** — stream to/from relational databases
- **Kafka I/O** — streaming integration
- **wingfoil-python full parity** — every node and I/O adapter in the Rust core exposed to Python
- **Python showcase** — define a type and pipeline in Rust, run it from Python, and crunch the results with pandas, scikit-learn, and plotly/matplotlib
- **JS/TS browser integration** — two proposals under evaluation: run wingfoil natively in-browser via WASM, or stream typed Rust data to a reactive JS frontend

If you work in quant finance, real-time data, or just love high-performance Rust — we'd love your help.

We're especially keen to hear from specialists in:
  * **FPGA**
  * **JS/TS**
  * **PyO3**

New to open source or Rust? We have good first issues tagged — including adding EWMA, inspect, and throttle to the Python bindings. All issues are linked in the first comment.

Drop a comment, open an issue, join us on Discord or just star the repo.

Rust + streams + data. Let's build.

#Rust #StreamProcessing #BigData #OpenSource #KDB #Kafka #FinTech #DataEngineering

---

## FIRST COMMENT (post this immediately after — paste all links here)

🔗 Repo: https://github.com/wingfoil-io/wingfoil
💬 Discord: https://discord.gg/WfZwpQnZUA

**What's in progress:**
- ZMQ production-ready: https://github.com/wingfoil-io/wingfoil/issues/52
- ZMQ service discovery: https://github.com/wingfoil-io/wingfoil/issues/103
- KDB+ caching: https://github.com/wingfoil-io/wingfoil/issues/90
- Binary file I/O (Arrow, Parquet): https://github.com/wingfoil-io/wingfoil/issues/104
- SQL I/O: https://github.com/wingfoil-io/wingfoil/issues/105
- Kafka I/O: https://github.com/wingfoil-io/wingfoil/issues/23
- wingfoil-python full parity: https://github.com/wingfoil-io/wingfoil/issues/106
- Python showcase (pandas + scikit-learn + plotly): https://github.com/wingfoil-io/wingfoil/issues/107
- JS/TS browser integration: https://github.com/wingfoil-io/wingfoil/issues/110

**Good first issues:**
- Add EWMA node: https://github.com/wingfoil-io/wingfoil/issues/111
- Python binding for inspect(): https://github.com/wingfoil-io/wingfoil/issues/112
- Python binding for throttle(): https://github.com/wingfoil-io/wingfoil/issues/113

---
*Draft — tags to add before posting: @Zohaib Hassan, @Aditya Shirsat, @Matvey Ezhov (find their LinkedIn profiles to tag directly)*

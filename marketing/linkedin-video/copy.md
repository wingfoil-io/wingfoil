# Post copy

Upload `out/wingfoil_linkedin.mp4` **natively** to LinkedIn. A native video is
distributed far more widely than a post that links out to one, which is also why
the repo link goes in the first comment rather than the post body.

## Post

> Most teams write their backtest twice.
>
> One codebase replays history. Another one runs live. They start identical and
> they do not stay that way — a fill model gets fixed in one and not the other, a
> timestamp is handled differently, and six months later the backtest is
> describing a system you don't actually run.
>
> wingfoil is a Rust stream-processing library built so that can't happen. You
> describe the graph once. The same definition runs as an instant deterministic
> replay for backtesting, and as a live system paced by the wall clock. Same
> nodes, same values, same order — the run mode is an argument, not a rewrite.
>
> The 33 seconds below is the whole idea: one graph — a limit order book fanned
> out to each side of the top and recombined into a quote — run both ways. The
> market data is a real NASDAQ sample, and the terminal output is captured from
> actually running it, not mocked up.
>
> Repo link in the comments 👇

## First comment

Post it immediately after publishing, while the post is being distributed. It
carries the link the post body and the closing card both promise.

> Code: https://github.com/wingfoil-io/wingfoil — Rust, Apache-2.0.
>
> The example in the video is `crates/wingfoil/examples/core/top_of_book`: a
> limit order book driven by real NASDAQ AAPL messages. The LOBSTER sample is
> committed, so it runs straight from a clone with no data to go and find.
>
> cargo run --release --example top_of_book
> cargo run --release --example top_of_book -- realtime
>
> One graph definition, two run modes. The first replays 09:30–10:30 — 91,997
> messages, 15,387 quote changes — as fast as the CPU can walk the graph. The
> second feeds the same messages at their original pace, so it takes the hour.
> Same quotes, same order; only the clock changes. The terminal output in the
> video is captured from those two commands, and the capture fails the build if
> the run modes ever stop agreeing.
>
> If you want to point it at something real, the adapters are Aeron, iceoryx2,
> Kafka, Redis, ZeroMQ, FIX, kdb+, Postgres, WebSocket, Prometheus and OTLP.
> There are Python bindings too, if the research half of your stack lives there.

## Upload checklist

- [ ] Native upload — attach the MP4, don't paste a video link
- [ ] Attach `out/tutorial_li.srt` as the caption file (LinkedIn accepts SRT on
      video uploads; the video also has captions burned in, so a viewer with
      LinkedIn's captions off still reads it)
- [ ] Repo link in the **first comment**, posted immediately after publishing
- [ ] Check the square crop in LinkedIn's mobile preview before publishing

## Notes on the claims made

**The video makes no performance claim, deliberately.** Earlier cuts did, and
every version was withdrawn for the same reason: on this workload the framework
is not what the number measures. Of a 156 ms replay, roughly 56 ms is the
`lobster` order book and 80 ms is stdout — swapping `println!` for a
`BufWriter` moved the supposed "engine" figure by 30%. Any headline number here
would be a claim about I/O wearing wingfoil's name. If a commenter asks how
fast it is, the honest answer is the decomposition, plus `cargo bench` in the
repo for per-node figures with their workload attached.

The numbers that *are* quoted — 91,997 messages, 15,387 quote changes, the
09:30–10:30 span — are properties of the committed sample and the graph, not of
a machine, so they reproduce anywhere. Keep it that way: nothing machine
specific in the post or the comment.

The video says the quotes and their order are identical across run modes, and
that the pacing and the clock are what differ. That is what the engine does and
what the capture script asserts on every build. It does **not** claim the
engine *timestamps* match across modes — under `RunMode::RealTime` engine time
is the wall clock, so they don't. See the README section "What scenes 4 and 5
actually show" before rewording any of this.

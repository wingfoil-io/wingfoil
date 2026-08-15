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
> The 45 seconds below is the whole idea: one graph — a limit order book fanned
> out to each side of the top and recombined into a quote — run both ways. The
> market data is a real NASDAQ sample, and the terminal output is captured from
> actually running it, not mocked up.
>
> On the fan-out benchmark in the repo, a 103-node graph runs a full cycle in
> 32ns compiled with nitro, against 1.57µs interpreted. Those are measured, not
> quoted — the video renders them straight out of criterion.
>
> Repo link in the comments 👇

## First comment

> Code and docs: https://github.com/wingfoil-io/wingfoil
>
> The example in the video is `crates/wingfoil/examples/core/top_of_book` — real
> NASDAQ AAPL messages from the LOBSTER sample. Run it yourself both ways:
>
> `cargo run --example top_of_book`
> `cargo run --example top_of_book -- realtime`
>
> Same quotes, same order. Only the clock changes.

## Upload checklist

- [ ] Native upload — attach the MP4, don't paste a video link
- [ ] Attach `out/tutorial_li.srt` as the caption file (LinkedIn accepts SRT on
      video uploads; the video also has captions burned in, so a viewer with
      LinkedIn's captions off still reads it)
- [ ] Repo link in the **first comment**, posted immediately after publishing
- [ ] Check the square crop in LinkedIn's mobile preview before publishing

## Notes on the claims made

The performance numbers on scene 3 are measured on the machine that rendered
the video, from the `fanout` group of `benches/tiers.rs` — 103 nodes, fan out
and recombine. Both the workload and the node count are on screen, labelled as
the benchmark's, because the graph animated beside them is five boxes and
twelve nodes. If anyone conflates the two, that is the answer. They are
**not** a claim about the order book graph the other scenes show, and there is
deliberately no comparison against another library: a cross-library ratio needs
its workload attached to mean anything.

Absolute times are machine-specific and were taken on a shared cloud host,
which tends to punish the interpreted tier hardest. If the 49× is challenged,
that is the honest answer — re-run `npm run bench` on a quiet machine.

The video says the quotes and their order are identical across run modes, and
that the pacing and the clock are what differ. That is what the engine does and
what the capture script asserts on every build. It does **not** claim the
engine *timestamps* match across modes — under `RunMode::RealTime` engine time
is the wall clock, so they don't. See the README section "What scenes 4 and 5
actually show" before rewording any of this.

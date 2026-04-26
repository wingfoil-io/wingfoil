# latency_e2e — Pulumi deployments

Two independent stacks, deployed separately, with different goals:

| Stack | Purpose | Cost | When to use |
|---|---|---|---|
| [`fargate/`](./fargate) | Always-on public demo | ~$0.10/hr | "Click here to see wingfoil run" |
| [`baremetal/`](./baremetal) | On-demand perf showcase | ~$4.28/hr while running | "Look at these microsecond numbers" |

The Fargate stack is what you point a public URL at — cheap, always
available, but the per-hop latency dashboard is dominated by hypervisor
jitter and CFS preemption, so the wingfoil-specific numbers look
unimpressive.

The bare-metal stack is what you spin up before a customer call or a
benchmark post — it has isolated CPU cores (`isolcpus=`), pinned graph
threads, no hypervisor between you and the silicon. The numbers on this
one are defensible. Tear it down (or `aws ec2 stop-instances`) when
you're done.

See each subdirectory's README for the deploy steps.

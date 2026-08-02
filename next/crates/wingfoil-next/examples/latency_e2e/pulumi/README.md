# latency_e2e — Pulumi deployments

Three independent stacks, deployed separately, with different goals:

| Stack | Purpose | Cost | When to use |
|---|---|---|---|
| [`fargate/`](./fargate) | Always-on public demo, zero-effort restarts | ~$0.06/hr (~$45/mo) | "Click here to see wingfoil run; I don't want to think about ops" |
| [`ec2-spot/`](./ec2-spot) | Always-on public demo, cheaper | ~$0.015/hr (~$12/mo) | "Click here, but I'd rather pay 4× less and accept ~60-90s reclaim gaps" |
| [`baremetal/`](./baremetal) | On-demand perf showcase | ~$4.28/hr while running | "Look at these microsecond numbers" |

The **Fargate** stack is the lowest-effort always-on option — ECS handles
restarts, but you pay for the ALB and Fargate's compute premium.

The **EC2 Spot** stack runs the same five containers on a single t3.small
Spot instance with a Packer-baked AMI (Docker + pre-pulled images). Single-AZ
ASG of size 1 in `eu-west-2a` (London, near LMAX LD4). Reclaims trigger an
auto-launch ~60-90s later; a Grafana banner shows the countdown when the
2-minute warning fires.

The **bare-metal** stack is what you spin up before a customer call or a
benchmark post — it has isolated CPU cores (`isolcpus=`), pinned graph
threads, no hypervisor between you and the silicon. The numbers on this
one are defensible. Tear it down (or `aws ec2 stop-instances`) when
you're done.

See each subdirectory's README for the deploy steps.

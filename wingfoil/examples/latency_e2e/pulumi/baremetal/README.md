# latency_e2e — bare-metal Pulumi stack

On-demand EC2 bare-metal deployment that showcases real wingfoil per-hop
performance: isolated CPU cores, no hypervisor jitter, sub-microsecond
intra-process hops via iceoryx2.

Companion to the always-on `../fargate/` stack — bring this one up when
you want defensible perf numbers, tear it down (or stop the instance to
keep the EIP) when you don't.

## What you get

| | bare-metal stack | fargate stack |
|---|---|---|
| per-hop p50 | 100 ns – 1 μs | 1 – 10 μs |
| per-hop p99 | 2 – 10 μs | 100 μs – 10 ms |
| p99.9 tail | tens of μs | hundreds of ms (CFS preemption) |
| cost | ~$4.28/hr (`c7i.metal-24xl`, eu-west-2) | ~$0.10/hr |
| cold start | 5–8 min | 30–60 s |
| use it for | showcasing perf | always-on demo |

The numbers above are estimates, not measurements. The point is the
order-of-magnitude story: bare metal is where the wingfoil pitch — sub-μs
intra-process hops — actually shows up.

## Layout

* `__main__.py` — VPC, EC2 bare metal, EIP, S3 bucket, secrets, IAM,
  wake-on-demand Lambda
* `user_data.sh` — first-boot bootstrap: grub `isolcpus=`, reboot, sync
  binaries from S3, write systemd units pinned to isolated cores, start
  observability containers on housekeeping cores, install idle-stop timer
* `idle_watch.sh` — per-minute self-stop check (uptime ≥ 10 min &&
  active sessions == 0 for ≥ 10 min)
* `wake_lambda.py` — Lambda handler serving the wake page + JSON API
* `Pulumi.yaml` — config schema (region, instance type, core layout)

## One-time setup

```bash
cd wingfoil/examples/latency_e2e/pulumi/baremetal
python3 -m venv venv && source venv/bin/activate
pip install -r requirements.txt
pulumi stack init demo
pulumi config set --secret lmax_username <YOUR_LMAX_USERNAME>
pulumi config set --secret lmax_password <YOUR_LMAX_PASSWORD>
```

Optional overrides:

```bash
# Different region (default eu-west-2 for low-latency LMAX London RTT)
pulumi config set aws:region eu-west-1

# Cheaper bare metal (previous-gen)
pulumi config set instance_type c5n.metal

# Wider isolation set (e.g. for NUMA-aware variants)
pulumi config set isolated_cores 2,3,4,5,6,7

# Restrict ingress to your office IP
pulumi config set ingress_cidr 203.0.113.4/32
```

## Each spin-up

```bash
# 1. Build release binaries (Pulumi reads them from target/release/examples)
cargo build --release -p wingfoil --example latency_e2e_ws_server \
    --features "web,iceoryx2-beta,prometheus,otlp"
cargo build --release -p wingfoil --example latency_e2e_fix_gw \
    --features "fix,iceoryx2-beta"

# 2. Bring the stack up — uploads binaries to S3, provisions infra
pulumi up

# 3. Wait ~5–8 min for the box to bootstrap
#    (cold boot + grub rewrite + reboot + service start + LMAX FIX logon)
pulumi stack output ws_server_url
# → http://<eip>:8080

# 4. Open the URL, click start, watch real per-hop numbers
```

## Wake from the browser

`pulumi up` exports a `wake_url` — a public HTTPS Lambda function URL
that serves a "wake the perf box" page:

```bash
pulumi stack output wake_url
# → https://<id>.lambda-url.eu-west-2.on.aws/
```

Anyone with that URL can:

* see the current state (`stopped` / `pending` / `running` / `stopping`),
* click a button to start the box if it's stopped,
* follow a link to the demo once the box reports `running`.

The Lambda's IAM role only permits `ec2:StartInstances` against this
specific instance ARN, so a leaked URL can't escalate. It also can't
*stop* the box — the auto-stop daemon on the box itself handles that.

## Spend cap

The wake URL is unauthenticated by default. The real cost limiter is
already on the box — the idle-stop daemon stops the instance ≥20 min
after each wake, so a single attacker can't burn more than
~$4.30/hour even with a perfect attack pattern (the EC2 state machine
makes `POST /wake` a no-op when the instance is already running).

On top of that, set an AWS Budget so you'll get an email if cost ever
runs away anyway:

```bash
pulumi config set alert_email you@example.com
pulumi config set monthly_budget_usd 50    # default
pulumi up
```

Two notifications: 80% forecast (warning) and 100% actual (breach), both
emailed direct (no SNS confirmation needed). Skipped entirely if
`alert_email` is unset.

If you ever want it to *act* on a breach instead of just emailing you
(e.g. auto-stop the instance), AWS Budgets supports Budget Actions
natively — ~10 lines of additional Pulumi to attach an `EC2:StopInstances`
action when the threshold hits 100%.

**Want zero unauthenticated access instead?**

* Add a `wake_token` query-param check in `wake_lambda.py` (~10 lines).
  The token becomes the credential — share the URL with the token in
  it.

## Stop without destroying

The box stops itself when idle (see below). If you want to pause it
manually instead:

```bash
INSTANCE_ID=$(pulumi stack output instance_id)
aws ec2 stop-instances --instance-ids "$INSTANCE_ID"
# resume:
aws ec2 start-instances --instance-ids "$INSTANCE_ID"
```

While stopped you pay the EBS volume (~$5/month for 64 GB gp3) plus the
Elastic IP (~$3.60/month — AWS charges for IPv4 addresses regardless of
attachment state). `pulumi destroy` removes everything, including those.

## Auto-stop when idle

A 1-minute systemd timer on the box runs `/opt/wingfoil/idle_watch.sh`,
which self-stops the instance when **both**:

* uptime ≥ 10 minutes (so a freshly-started box has time to be used), and
* the local prometheus exporter has reported `latency_e2e_active_sessions == 0`
  for ≥ 10 consecutive minutes.

Tune the thresholds by editing the constants at the top of
`idle_watch.sh` and re-running `pulumi up` — the script ships in S3 and
gets re-pulled on next boot.

The instance role grants `ec2:StopInstances` scoped via tag condition
(`Project=…, Stack=…`) so the box can stop *itself* but not other
instances in the account.

## How the perf box is wired

* Cores `0,1` — kernel housekeeping + observability containers (cpuset)
* Core `2` — `wingfoil-ws-server.service`, graph thread pinned via
  `WINGFOIL_PIN_GRAPH=2` and systemd `CPUAffinity=2`
* Core `4` — `wingfoil-fix-gw.service`, pinned via `WINGFOIL_PIN_GRAPH=4`
  and `CPUAffinity=4`
* Cores `3,5` — reserved (left isolated; available for future per-thread
  pinning of adapter workers)

Grub is configured with `isolcpus=2-5 nohz_full=2-5 rcu_nocbs=2-5`, which
removes the cores from the general scheduler, silences the periodic
timer tick, and offloads RCU callbacks. The systemd units' `CPUAffinity`
is belt-and-braces alongside the `WINGFOIL_PIN_GRAPH` env var read by
`shared::pin_current_from_env` in the binaries themselves.

## Verifying it actually works

```bash
ssh ubuntu@<public_ip>

# isolcpus applied?
cat /proc/cmdline   # should show isolcpus=2,3,4,5

# graph thread pinned to core 2?
ps -eLo pid,tid,psr,comm | grep ws_server   # PSR column = 2

# no other kernel work on the hot core?
cat /proc/interrupts | head -1               # column header
grep -E "^[A-Z0-9]+:" /proc/interrupts | awk '{print $3}' | sort | uniq -c
# core 2 column should be near zero
```

## Troubleshooting

**Bootstrap stuck.** SSH in and check `/var/lib/wingfoil-bootstrap-stage`
(states: `init` → `installed` → `running`) and `journalctl -u cloud-final`.
The script logs to `/var/log/cloud-init-output.log`.

**isolcpus not applied.** `/proc/cmdline` should contain it after the
post-grub reboot. If not, `update-grub` may have failed — check
`journalctl -b 0 -u cloud-final`.

**Binaries fail to start.** `journalctl -u wingfoil-ws-server -f` for
ws_server, ditto fix_gw. Most common cause: missing LMAX creds — the
secrets manager fetch in user_data must have succeeded.

**Browser sees no fills.** Check the FIX session status in the fix_gw
journal — LMAX demo creds expire and need refreshing periodically.

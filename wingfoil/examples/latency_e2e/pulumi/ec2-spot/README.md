# Wingfoil Latency E2E — EC2 Spot Deployment

Always-on deployment of the latency_e2e demo on a single **t3.small Spot**
instance with a **Packer-baked AMI**. Sibling stack to `fargate/` (high-cost,
auto-restart) and `baremetal/` (perf demo, on-demand).

## Why this stack exists

| Stack | Cost / mo (London) | Reclaim downtime | Best for |
|---|---|---|---|
| `fargate/` | ~$45 | ~5 min (ECS auto-restart) | Zero-effort always-on |
| **`ec2-spot/` (this)** | **~$11–13** | **~60–90s** (ASG + baked AMI) | **Cheap always-on** |
| `baremetal/` | ~$3,000+ (when running) | N/A — on-demand | Sub-µs perf showcase |

The Packer-baked AMI is what gets reclaim recovery from ~5 min (vanilla AL2023
+ `dnf install docker` + image pulls) down to ~60–90 s (just `docker compose up`
on cached images).

## Architecture

```
                   ┌──────────────────────────┐
   public IP ──── EIP ───── ASG (size=1, single AZ) ─── Spot t3.small
                                                            │
                                                  ┌─────────┴─────────┐
                                                  │  Docker Compose    │
                                                  │  ws_server  :8080  │
                                                  │  fix_gw     ─→ LMAX│
                                                  │  prometheus :9090  │
                                                  │  tempo      :4318  │
                                                  │  grafana    :3000  │
                                                  └─────────┬──────────┘
                                                            │
                                                spot_watcher (host process) :9092
                                                            │
                                                  Prometheus scrapes :9092
                                                            │
                                                Grafana banner shows countdown
```

- **EIP is reattached on every boot** by `user_data.sh` via
  `aws ec2 associate-address`. URL stays stable across reclaims.
- **`spot_watcher.py`** runs as a systemd unit, polling
  `http://169.254.169.254/latest/meta-data/spot/instance-action` every 5 s and
  exposing `wingfoil_spot_termination_seconds_remaining` on `:9092`. Prometheus
  scrapes it; the Grafana dashboard shows a red banner with the countdown.
- **Single AZ** (default `eu-west-2a`) — keeps EIP/EBS reattach simple. London
  region is chosen for proximity to LMAX LD4 (Slough).

## Prerequisites

1. **Pulumi CLI** — [Install Pulumi](https://www.pulumi.com/docs/install/)
2. **AWS credentials** with permission to create VPC/EC2/IAM/Secrets Manager
3. **Five service images** in ECR (or Docker Hub) — see
   `wingfoil/examples/latency_e2e/Dockerfile.*` and
   `.github/workflows/build-latency-e2e-images.yml`
4. **A baked AMI** — built via the `Build latency_e2e AMI (ec2-spot)` GitHub
   Action or the local Packer steps below
5. **LMAX FIX credentials** — username and password for the demo environment

## 1. Build the AMI

### Option A — GitHub Action (recommended)

Trigger `Build latency_e2e AMI (ec2-spot)` from the Actions tab. It runs Packer
against ECR-hosted images and writes the resulting AMI ID to SSM Parameter
Store at `/wingfoil/latency-e2e/ec2-spot/ami_id`.

Retrieve it for the Pulumi config step:

```bash
aws ssm get-parameter \
  --name /wingfoil/latency-e2e/ec2-spot/ami_id \
  --region eu-west-2 \
  --query 'Parameter.Value' --output text
```

### Option B — Local Packer build

```bash
cd wingfoil/examples/latency_e2e/pulumi/ec2-spot/packer

# One-time
packer init wingfoil-latency.pkr.hcl

packer build \
  -var "region=eu-west-2" \
  -var "ws_server_image=<registry>/wingfoil/ws-server:latest" \
  -var "fix_gw_image=<registry>/wingfoil/fix-gw:latest" \
  -var "prometheus_image=<registry>/wingfoil/prometheus:latest" \
  -var "tempo_image=<registry>/wingfoil/tempo:latest" \
  -var "grafana_image=<registry>/wingfoil/grafana:latest" \
  wingfoil-latency.pkr.hcl
```

The AMI ID is printed at the end and recorded in `packer-manifest.json`.

## 2. Deploy the stack

```bash
cd wingfoil/examples/latency_e2e/pulumi/ec2-spot

python3 -m venv venv && source venv/bin/activate
pip install -r requirements.txt

pulumi stack init demo
pulumi config set aws:region eu-west-2
pulumi config set ami_id ami-0123456789abcdef0          # from step 1
pulumi config set --secret lmax_username <YOUR_LMAX_USERNAME>
pulumi config set --secret lmax_password <YOUR_LMAX_PASSWORD>

# Optional overrides
# pulumi config set availability_zone eu-west-2b        # default eu-west-2a
# pulumi config set instance_type     t3a.small         # AMD-flavoured Spot, slightly cheaper
# pulumi config set ingress_cidr      203.0.113.0/24    # lock to your office IP
# pulumi config set max_spot_price    0.0228            # cap (default = on-demand price)

pulumi up
```

After ~3–5 min the outputs print:

```
ws_server_url   http://<eip>:8080
grafana_url     http://<eip>:3000
prometheus_url  http://<eip>:9091/metrics
public_ip       <eip>
```

## 3. Verify

```bash
# WS server
curl -fsS http://<eip>:8080/healthz

# Grafana — load the dashboard, the "Spot interruption status" banner should
# read green ("Stable — no Spot interruption notice").
open http://<eip>:3000
```

## What happens during a Spot reclaim

1. **T-120s** — AWS sets the IMDS `spot/instance-action` flag.
2. **T-115s** — `spot_watcher.py` picks it up on its next 5 s poll. The
   Grafana banner switches red and counts down (`wingfoil_spot_termination_seconds_remaining`).
3. **T-0s** — instance terminates.
4. **T+~30s** — the ASG launches a replacement in the same AZ.
5. **T+~60–90s** — `user_data.sh` finishes: EIP reattached, secrets fetched,
   `docker compose up -d` started against cached images. Service is back.
6. **T+~90–120s** — fix_gw completes its FIX logon to LMAX, market data
   resumes flowing.

Expected total downtime per reclaim: **~60–90 s of public-URL outage** plus
**~30–60 s of LMAX session re-establishment**. AWS publishes interruption
rates per instance/region at the [Spot Instance Advisor][advisor]; t3.small in
eu-west-2 is typically <5%/mo, i.e. 0–3 reclaims/month.

[advisor]: https://aws.amazon.com/ec2/spot/instance-advisor/

## Cost estimate

| Component | Monthly |
|---|---|
| t3.small Spot (~$0.007/hr × 730) | ~$5 |
| Elastic IP | ~$3.65 |
| EBS gp3 20 GB | ~$1.86 |
| Secrets Manager (2 × $0.40) | ~$0.80 |
| Data transfer out (low traffic) | ~$0–2 |
| **Total** | **~$11–13** |

Assumes always-on, low traffic, no significant egress. Compare to
`fargate/`'s ~$45/mo (Fargate compute + ALB + ALB LCUs).

## Cleanup

```bash
pulumi destroy
```

The Packer-built AMI is **not** destroyed by Pulumi — delete it via the AWS
console or:

```bash
aws ec2 deregister-image --image-id <ami-id> --region eu-west-2
```

## Troubleshooting

**Spot capacity unavailable**: try a different AZ (`pulumi config set availability_zone eu-west-2c && pulumi up`) or a different instance type
(`t3a.small`).

**EIP didn't reattach**: check the IAM policy in `__main__.py` — the
`AssociateAddress` permission is conditioned on `Project` and `Stack` tags
matching this stack. The launch template's `tag_specifications` must apply
those tags at instance creation; verify with
`aws ec2 describe-instances --filters Name=tag:Stack,Values=demo`.

**Grafana banner stays "Stable" during a real reclaim**: check that
`spot_watcher.service` is running (`systemctl status wingfoil-spot-watcher`)
and that Prometheus is reaching `localhost:9092` (the
`latency_e2e_spot_watcher` job in `prometheus.yml`). On non-EC2-Spot stacks
the metric is unscraped — that's expected, and the banner stays green.

**`fix_gw` can't log in**: secrets fetch happens in `user_data.sh`. Inspect
`/var/log/cloud-init-output.log` on the instance for `aws secretsmanager`
errors, and confirm the IAM policy lets the instance read both secrets.

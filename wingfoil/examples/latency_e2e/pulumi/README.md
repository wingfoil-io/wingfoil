# Wingfoil Latency E2E — AWS ECS Fargate Deployment

Deploy the complete latency_e2e demo (5 containers: fix_gw, ws_server, prometheus, tempo, grafana) to AWS ECS Fargate using Pulumi.

## Prerequisites

1. **Pulumi CLI** — [Install Pulumi](https://www.pulumi.com/docs/install/)
2. **AWS credentials** — Configured via `aws configure` or environment variables
3. **Docker images** — Built and pushed to ECR or Docker Hub:
   - `wingfoil/ws-server` (from `Dockerfile.ws_server`)
   - `wingfoil/fix-gw` (from `Dockerfile.fix_gw`)
4. **LMAX credentials** — FIX username and password for the demo environment

## Setup

### 1. Install Python dependencies

```bash
cd wingfoil/examples/latency_e2e/pulumi
python3 -m venv venv
source venv/bin/activate
pip install -r requirements.txt
```

### 2. Configure the stack

```bash
# Set the stack to demo
pulumi stack select demo

# Set required configuration
pulumi config set aws:region us-east-1
pulumi config set environment demo
pulumi config set ws_server_image <ECR_URI_or_DOCKER_HUB_IMAGE>
pulumi config set fix_gw_image <ECR_URI_or_DOCKER_HUB_IMAGE>

# Set secrets (LMAX credentials)
pulumi config set --secret lmax_username <YOUR_LMAX_USERNAME>
pulumi config set --secret lmax_password <YOUR_LMAX_PASSWORD>
```

**Example with Docker Hub images:**
```bash
pulumi config set ws_server_image "yourusername/wingfoil-ws-server:latest"
pulumi config set fix_gw_image "yourusername/wingfoil-fix-gw:latest"
```

**Example with ECR images:**
```bash
pulumi config set ws_server_image "123456789012.dkr.ecr.us-east-1.amazonaws.com/wingfoil/ws-server:latest"
pulumi config set fix_gw_image "123456789012.dkr.ecr.us-east-1.amazonaws.com/wingfoil/fix-gw:latest"
```

### 3. Review the deployment plan

```bash
pulumi preview
```

### 4. Deploy

```bash
pulumi up
```

This will create:
- VPC with public/private subnets
- ALB with listeners on :80 (ws_server) and :3000 (grafana)
- ECS Cluster and Service
- S3 bucket for Tempo traces
- CloudWatch log group
- Secrets Manager secrets for LMAX credentials
- IAM roles and policies

## Accessing the Dashboard

After deployment, Pulumi exports the ALB DNS name. Use it to access the demo:

```bash
# Get the ALB DNS name
pulumi stack output alb_dns_name
```

Then open:
- **WS Server UI + WebSocket**: `http://<ALB_DNS>:80`
- **Grafana Dashboard**: `http://<ALB_DNS>:3000`

The dashboard should auto-load (see `latency.json` for the predefined dashboard).

## Configuration

Modify `Pulumi.demo.yaml` or use `pulumi config set` to customize:

| Variable | Default | Description |
|----------|---------|-------------|
| `aws:region` | `us-east-1` | AWS region |
| `environment` | `demo` | Environment tag |
| `cpu` | `1024` | ECS task CPU units (0.25, 0.5, 1, 2, 4 vCPU equivalent) |
| `memory` | `2048` | ECS task memory (MB) |
| `shm_size` | `256` | Shared memory for iceoryx2 (MB) |
| `ws_server_image` | — | ws_server container image (required) |
| `fix_gw_image` | — | fix_gw container image (required) |
| `lmax_username` | — | LMAX FIX username (secret, required) |
| `lmax_password` | — | LMAX FIX password (secret, required) |

## Monitoring

CloudWatch logs are available at:
```bash
/ecs/wingfoil-latency-demo
```

View logs:
```bash
# Real-time logs for ws_server
aws logs tail /ecs/wingfoil-latency-demo --follow --log-stream-name-pattern 'ws_server'

# Real-time logs for fix_gw
aws logs tail /ecs/wingfoil-latency-demo --follow --log-stream-name-pattern 'fix_gw'
```

## Cleanup

To destroy all resources:

```bash
pulumi destroy
```

## Troubleshooting

**Service stuck in `PROVISIONING`?**
- Check ECS task logs: `pulumi stack output cloudwatch_log_group`
- Verify image URIs are correct and accessible
- Ensure security groups allow traffic between containers (they should, by default)

**ALB returning 502?**
- Check target group health: AWS Console → Target Groups → wingfoil-latency-demo-{ws-tg,grafana-tg}
- Verify containers are healthy in ECS console

**Prometheus/Tempo can't reach ws_server?**
- Containers share the same task network namespace — use `localhost` (not service DNS)
- `prometheus.yml` already configured with `localhost:9091`
- `tempo.yaml` configured with `localhost:4318`

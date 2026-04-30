# Building and Pushing Docker Images

> **CI alternative:** the `Build & Push latency_e2e Images` workflow
> (`.github/workflows/build-latency-e2e-images.yml`) builds and pushes all
> five images to ECR on pushes to `main` (and via manual dispatch). It uses
> `AWS_ROLE_TO_ASSUME` via OIDC and creates the `wingfoil/<name>` ECR repos
> on first run, so the steps below are only needed for local builds.

## Prerequisites

1. Docker installed and running
2. AWS CLI configured
3. Access to ECR or Docker Hub

## Build Images Locally

The fargate stack ships five containers: the two wingfoil binaries plus three
observability containers (prometheus, tempo, grafana) with their configs baked
in. From the root of the wingfoil repo:

```bash
# Wingfoil binaries
docker build -f wingfoil/examples/latency_e2e/Dockerfile.ws_server -t wingfoil-ws-server:latest .
docker build -f wingfoil/examples/latency_e2e/Dockerfile.fix_gw    -t wingfoil-fix-gw:latest    .

# Observability — base image + COPY of the matching config dir
docker build -f wingfoil/examples/latency_e2e/Dockerfile.prometheus -t wingfoil-prometheus:latest .
docker build -f wingfoil/examples/latency_e2e/Dockerfile.tempo      -t wingfoil-tempo:latest      .
docker build -f wingfoil/examples/latency_e2e/Dockerfile.grafana    -t wingfoil-grafana:latest    .

# Verify images built successfully
docker images | grep wingfoil
```

## Push to AWS ECR

### Setup ECR repositories

```bash
aws ecr create-repository --repository-name wingfoil/ws-server  --region us-east-1
aws ecr create-repository --repository-name wingfoil/fix-gw     --region us-east-1
aws ecr create-repository --repository-name wingfoil/prometheus --region us-east-1
aws ecr create-repository --repository-name wingfoil/tempo      --region us-east-1
aws ecr create-repository --repository-name wingfoil/grafana    --region us-east-1
```

### Authenticate Docker with ECR

```bash
aws ecr get-login-password --region us-east-1 | docker login --username AWS --password-stdin <ACCOUNT_ID>.dkr.ecr.us-east-1.amazonaws.com
```

### Tag and push images

```bash
ECR=<ACCOUNT_ID>.dkr.ecr.us-east-1.amazonaws.com
for name in ws-server fix-gw prometheus tempo grafana; do
  docker tag  wingfoil-${name}:latest "${ECR}/wingfoil/${name}:latest"
  docker push "${ECR}/wingfoil/${name}:latest"
done
```

## Push to Docker Hub

### Login to Docker Hub

```bash
docker login
```

### Tag and push images

```bash
HUB=<YOUR_DOCKER_HUB_USERNAME>
for name in ws-server fix-gw prometheus tempo grafana; do
  docker tag  wingfoil-${name}:latest "${HUB}/wingfoil-${name}:latest"
  docker push "${HUB}/wingfoil-${name}:latest"
done
```

## Testing Locally with Docker Compose

To test the images locally before deploying to AWS:

```bash
cd wingfoil/examples/latency_e2e

# Start the original demo (for reference)
docker-compose up
```

## Deployment via Pulumi

Once images are pushed, use the image URIs in the Pulumi config:

```bash
cd wingfoil/examples/latency_e2e/pulumi/fargate

ECR=<ACCOUNT_ID>.dkr.ecr.us-east-1.amazonaws.com
pulumi config set ws_server_image  "${ECR}/wingfoil/ws-server:latest"
pulumi config set fix_gw_image     "${ECR}/wingfoil/fix-gw:latest"
pulumi config set prometheus_image "${ECR}/wingfoil/prometheus:latest"
pulumi config set tempo_image      "${ECR}/wingfoil/tempo:latest"
pulumi config set grafana_image    "${ECR}/wingfoil/grafana:latest"

pulumi up
```

## Image Build Notes

- **Builder stage**: Uses `rust:latest` (heavy, downloads full Rust toolchain + dependencies)
  - First build: ~5-10 minutes (depends on internet speed)
  - Subsequent builds: ~2-3 minutes (if Cargo cache is warm)
  
- **Runtime stage**: Uses `ubuntu:22.04` with only `libssl3` and `ca-certificates`
  - Final image size: ~200-300 MB (much smaller than builder stage)

- **Multi-stage benefit**: Only the final binary and static files are in the pushed image; the Rust toolchain is discarded

## Troubleshooting

**Build fails with permission denied?**
- Ensure you have the wingfoil repo checked out with correct ownership
- If using Docker Desktop, restart it

**Image push fails with 401 Unauthorized?**
- Verify credentials: `docker login` (for Docker Hub) or `aws ecr get-login-password` (for ECR)

**Build takes too long?**
- First build compiles all Rust dependencies — this is expected
- Subsequent builds use Docker layer caching and should be faster
- To speed up: ensure `cargo build` works locally first (`cargo build --release --example latency_e2e_ws_server`)

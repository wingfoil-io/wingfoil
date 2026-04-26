# Building and Pushing Docker Images

## Prerequisites

1. Docker installed and running
2. AWS CLI configured
3. Access to ECR or Docker Hub

## Build Images Locally

From the root of the wingfoil repo:

```bash
# Build ws_server image
docker build -f wingfoil/examples/latency_e2e/Dockerfile.ws_server -t wingfoil-ws-server:latest .

# Build fix_gw image
docker build -f wingfoil/examples/latency_e2e/Dockerfile.fix_gw -t wingfoil-fix-gw:latest .

# Verify images built successfully
docker images | grep wingfoil
```

## Push to AWS ECR

### Setup ECR repositories

```bash
aws ecr create-repository --repository-name wingfoil/ws-server --region us-east-1
aws ecr create-repository --repository-name wingfoil/fix-gw --region us-east-1
```

### Authenticate Docker with ECR

```bash
aws ecr get-login-password --region us-east-1 | docker login --username AWS --password-stdin <ACCOUNT_ID>.dkr.ecr.us-east-1.amazonaws.com
```

### Tag and push images

```bash
# ws_server
docker tag wingfoil-ws-server:latest <ACCOUNT_ID>.dkr.ecr.us-east-1.amazonaws.com/wingfoil/ws-server:latest
docker push <ACCOUNT_ID>.dkr.ecr.us-east-1.amazonaws.com/wingfoil/ws-server:latest

# fix_gw
docker tag wingfoil-fix-gw:latest <ACCOUNT_ID>.dkr.ecr.us-east-1.amazonaws.com/wingfoil/fix-gw:latest
docker push <ACCOUNT_ID>.dkr.ecr.us-east-1.amazonaws.com/wingfoil/fix-gw:latest
```

## Push to Docker Hub

### Login to Docker Hub

```bash
docker login
```

### Tag and push images

```bash
# ws_server
docker tag wingfoil-ws-server:latest <YOUR_DOCKER_HUB_USERNAME>/wingfoil-ws-server:latest
docker push <YOUR_DOCKER_HUB_USERNAME>/wingfoil-ws-server:latest

# fix_gw
docker tag wingfoil-fix-gw:latest <YOUR_DOCKER_HUB_USERNAME>/wingfoil-fix-gw:latest
docker push <YOUR_DOCKER_HUB_USERNAME>/wingfoil-fix-gw:latest
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
cd wingfoil/examples/latency_e2e/pulumi

pulumi config set ws_server_image "<ACCOUNT_ID>.dkr.ecr.us-east-1.amazonaws.com/wingfoil/ws-server:latest"
pulumi config set fix_gw_image "<ACCOUNT_ID>.dkr.ecr.us-east-1.amazonaws.com/wingfoil/fix-gw:latest"

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

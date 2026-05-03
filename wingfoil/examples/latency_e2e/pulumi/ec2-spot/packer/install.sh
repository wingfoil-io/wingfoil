#!/bin/bash
# Packer provisioner — runs as ec2-user on the AL2023 build instance.
# Installs Docker, pulls the five wingfoil images, and stages the
# docker-compose + spot_watcher files so first boot is just "compose up".

set -euxo pipefail

# Wait for cloud-init to finish so dnf isn't racing against base setup.
# AL2023's cloud-init returns exit 2 for "degraded done" — cloud-init
# completed but emitted non-fatal warnings (e.g. NetworkManager noise).
# Treat that as success; only exit 1 (genuine error) should fail the build.
cloud_init_rc=0
sudo cloud-init status --wait || cloud_init_rc=$?
if [ "${cloud_init_rc}" -ne 0 ] && [ "${cloud_init_rc}" -ne 2 ]; then
  echo "ERROR: cloud-init failed with exit code ${cloud_init_rc}" >&2
  exit "${cloud_init_rc}"
fi

sudo dnf update -y
# unzip — needed for the AWS CLI installer below.
# prometheus_client (used by spot_watcher.py) isn't packaged for AL2023, so
# install it into a dedicated venv at /opt/wingfoil/venv. A venv side-steps
# AL2023's externally-managed-environment marker without depending on the
# system pip being new enough to recognise --break-system-packages (the flag
# was only added in pip 23.0.1, and AL2023's bundled pip can lag behind).
# The systemd unit (see user_data.sh) invokes the venv's python3 directly.
sudo dnf install -y docker unzip python3-pip
sudo install -d -m 0755 /opt/wingfoil
sudo python3 -m venv /opt/wingfoil/venv
sudo /opt/wingfoil/venv/bin/pip install --no-cache-dir prometheus_client
sudo systemctl enable --now docker
sudo usermod -aG docker ec2-user

# docker-compose-plugin isn't in the AL2023 default repos — install the v2
# plugin binary directly.
sudo mkdir -p /usr/libexec/docker/cli-plugins
DOCKER_COMPOSE_VERSION="v2.29.7"
sudo curl -fsSL \
  "https://github.com/docker/compose/releases/download/${DOCKER_COMPOSE_VERSION}/docker-compose-linux-x86_64" \
  -o /usr/libexec/docker/cli-plugins/docker-compose
sudo chmod +x /usr/libexec/docker/cli-plugins/docker-compose

# AWS CLI v2 — used by user_data to fetch secrets and associate the EIP.
curl -fsSL "https://awscli.amazonaws.com/awscli-exe-linux-x86_64.zip" -o /tmp/awscliv2.zip
unzip -q /tmp/awscliv2.zip -d /tmp
sudo /tmp/aws/install
rm -rf /tmp/aws /tmp/awscliv2.zip

# Authenticate to ECR if we're pulling private images. ECR_REGISTRY is the
# host portion only (e.g. 123456789012.dkr.ecr.eu-west-2.amazonaws.com).
# ECR_PASSWORD is the auth token CI fetched via `aws ecr get-login-password`
# — the build instance has no IAM role, so it can't fetch the token itself.
if [ -n "${ECR_REGISTRY:-}" ]; then
  if [ -z "${ECR_PASSWORD:-}" ]; then
    echo "ERROR: ECR_REGISTRY is set but ECR_PASSWORD is empty; CI must forward PKR_VAR_ecr_password." >&2
    exit 1
  fi
  # Disable xtrace just for the login — `set -x` echoes commands after
  # parameter expansion, which would print the ECR token into the Packer log.
  # Packer's `sensitive = true` on ecr_password also redacts it, but belt-and-
  # braces here keeps the secret out of the trace stream entirely.
  set +x
  printf '%s' "${ECR_PASSWORD}" | sudo docker login --username AWS --password-stdin "${ECR_REGISTRY}"
  set -x
fi

# Pre-pull every image so reclaim recovery doesn't wait on the registry.
for img in "${WS_SERVER_IMAGE}" "${FIX_GW_IMAGE}" "${PROMETHEUS_IMAGE}" "${TEMPO_IMAGE}" "${GRAFANA_IMAGE}"; do
  sudo docker pull "${img}"
done

# Drop the docker login state so the AMI doesn't ship with an ECR token
# baked into /root/.docker/config.json. The token expires in ~12h anyway,
# but cleaning up keeps the AMI free of credential artifacts.
sudo rm -rf /root/.docker /home/ec2-user/.docker

# Stage the runtime files in /opt/wingfoil. user_data writes the .env file
# alongside compose.yml on first boot, then runs `docker compose up -d`.
# (/opt/wingfoil already exists from the venv step above.)
sudo install -m 0644 /tmp/docker-compose.yml /opt/wingfoil/docker-compose.yml
sudo install -m 0755 /tmp/spot_watcher.py     /opt/wingfoil/spot_watcher.py

# Bind-mount sources for the three containers. compose.yml uses paths relative
# to its own directory (./prometheus/prometheus.yml etc.), so these must live
# directly under /opt/wingfoil/. Without them docker silently creates each
# missing path as an empty directory and the container fails to start with
# "not a directory: ... Are you trying to mount a directory onto a file".
#
# Verify the bind-mount targets *before* copying — a wrong upload layout (e.g.
# Packer's file provisioner appending the source basename twice) would silently
# leave the expected paths missing, which only surfaces at first boot when
# docker auto-creates them as empty directories and the bind-mount fails.
for staged in \
    /tmp/prometheus/prometheus.yml \
    /tmp/tempo/tempo.yaml \
    /tmp/grafana/provisioning; do
  if [ ! -e "${staged}" ]; then
    echo "ERROR: expected staged path '${staged}' is missing — Packer file" >&2
    echo "       provisioner produced an unexpected layout. Listing /tmp:" >&2
    ls -lR /tmp/prometheus /tmp/tempo /tmp/grafana >&2 || true
    exit 1
  fi
done

sudo cp -r /tmp/prometheus /opt/wingfoil/prometheus
sudo cp -r /tmp/tempo      /opt/wingfoil/tempo
sudo cp -r /tmp/grafana    /opt/wingfoil/grafana
sudo chown -R root:root /opt/wingfoil/prometheus /opt/wingfoil/tempo /opt/wingfoil/grafana

# Sanity check the final layout on disk: docker compose mounts these exact
# paths, and a missing file at boot triggers the silent-empty-directory
# behaviour that this section exists to prevent.
for mount_src in \
    /opt/wingfoil/prometheus/prometheus.yml \
    /opt/wingfoil/tempo/tempo.yaml \
    /opt/wingfoil/grafana/provisioning; do
  if [ ! -e "${mount_src}" ]; then
    echo "ERROR: bind-mount source '${mount_src}' is missing after copy." >&2
    exit 1
  fi
done

# Bake the image references into compose.yml so `compose up` doesn't pull a
# floating tag on every boot. Fail the build if the upstream compose ever
# bumps a tag — a silent no-op here would mean booting on stale images.
replace_image() {
  local pattern="$1" replacement="$2" file="$3"
  if ! grep -qF "${pattern}" "${file}"; then
    echo "ERROR: pattern '${pattern}' not found in ${file}; bump install.sh to match." >&2
    exit 1
  fi
  sudo sed -i "s|${pattern}|${replacement}|g" "${file}"
}
replace_image "prom/prometheus:v2.55.1" "${PROMETHEUS_IMAGE}" /opt/wingfoil/docker-compose.yml
replace_image "grafana/tempo:2.6.1"     "${TEMPO_IMAGE}"      /opt/wingfoil/docker-compose.yml
replace_image "grafana/grafana:11.3.0"  "${GRAFANA_IMAGE}"    /opt/wingfoil/docker-compose.yml

# Append ws_server + fix_gw services — the source compose.yml is operator-side
# (assumes binaries on the host); on EC2 Spot we run everything in containers.
#
# ws_server mounts /etc/wingfoil/tls/ read-only; user_data generates a fresh
# self-signed cert there on every boot. The container's built-in `web-tls`
# feature picks the cert up via the env-var pair and serves HTTPS + WSS on
# port 8080. Grafana mounts the same cert for HTTPS on 3000.
# `ipc: host` is load-bearing: iceoryx2 stores its segments under
# /dev/shm/iox2_*, and Docker's default per-container IPC namespace gives each
# container a private /dev/shm tmpfs. `network_mode: host` only shares the
# network namespace — without `ipc: host` ws_server and fix_gw can't see each
# other's iceoryx2 segments, orders never reach fix_gw, and no fills come back.
sudo tee -a /opt/wingfoil/docker-compose.yml > /dev/null <<EOF

  ws_server:
    image: ${WS_SERVER_IMAGE}
    network_mode: host
    ipc: host
    volumes:
      - /etc/wingfoil/tls:/etc/wingfoil/tls:ro
    environment:
      WINGFOIL_WEB_ADDR: "0.0.0.0:8080"
      WINGFOIL_METRICS_ADDR: "0.0.0.0:9091"
      WINGFOIL_OTLP_ENDPOINT: "http://localhost:4318"
      WINGFOIL_TLS_CERT: "/etc/wingfoil/tls/cert.pem"
      WINGFOIL_TLS_KEY: "/etc/wingfoil/tls/key.pem"
      RUST_LOG: info
    restart: unless-stopped

  fix_gw:
    image: ${FIX_GW_IMAGE}
    network_mode: host
    ipc: host
    env_file: /etc/wingfoil/lmax.env
    environment:
      RUST_LOG: info
    restart: unless-stopped
EOF

# Drop a docker-compose.override.yml that switches Grafana to HTTPS using
# the same self-signed cert ws_server uses. `docker compose` auto-merges
# overrides; this keeps the upstream compose readable for local dev
# while letting the deployment opt into HTTPS.
sudo tee /opt/wingfoil/docker-compose.override.yml > /dev/null <<'EOF'
services:
  grafana:
    environment:
      GF_SERVER_PROTOCOL: "https"
      GF_SERVER_CERT_FILE: "/etc/wingfoil/tls/cert.pem"
      GF_SERVER_CERT_KEY: "/etc/wingfoil/tls/key.pem"
    volumes:
      - /etc/wingfoil/tls:/etc/wingfoil/tls:ro
EOF

# Verify prometheus_client is importable from the venv interpreter — the
# systemd unit invokes /opt/wingfoil/venv/bin/python3, and a silent missing
# package would only surface at first boot.
/opt/wingfoil/venv/bin/python3 -c "import prometheus_client"

# Sanity check — fail the build if any image is missing locally.
for img in "${WS_SERVER_IMAGE}" "${FIX_GW_IMAGE}" "${PROMETHEUS_IMAGE}" "${TEMPO_IMAGE}" "${GRAFANA_IMAGE}"; do
  sudo docker image inspect "${img}" >/dev/null
done

echo "Packer install complete."

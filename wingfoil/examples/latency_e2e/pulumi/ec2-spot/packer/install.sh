#!/bin/bash
# Packer provisioner — runs as ec2-user on the AL2023 build instance.
# Installs Docker, pulls the five wingfoil images, and stages the
# docker-compose + spot_watcher files so first boot is just "compose up".

set -euxo pipefail

# Wait for cloud-init to finish so dnf isn't racing against base setup.
sudo cloud-init status --wait

sudo dnf update -y
# unzip — needed for the AWS CLI installer below.
# python3-prometheus_client — used by spot_watcher.py; installing via dnf
# avoids AL2023's externally-managed-environment block on `pip3 install`.
sudo dnf install -y docker unzip python3-prometheus_client
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
# host portion only (e.g. 123456789012.dkr.ecr.us-east-1.amazonaws.com).
# ECR_PASSWORD is the auth token CI fetched via `aws ecr get-login-password`
# — the build instance has no IAM role, so it can't fetch the token itself.
if [ -n "${ECR_REGISTRY:-}" ]; then
  if [ -z "${ECR_PASSWORD:-}" ]; then
    echo "ERROR: ECR_REGISTRY is set but ECR_PASSWORD is empty; CI must forward PKR_VAR_ecr_password." >&2
    exit 1
  fi
  echo "${ECR_PASSWORD}" | sudo docker login --username AWS --password-stdin "${ECR_REGISTRY}"
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
sudo install -d -m 0755 /opt/wingfoil
sudo install -m 0644 /tmp/docker-compose.yml /opt/wingfoil/docker-compose.yml
sudo install -m 0755 /tmp/spot_watcher.py     /opt/wingfoil/spot_watcher.py

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
sudo tee -a /opt/wingfoil/docker-compose.yml > /dev/null <<EOF

  ws_server:
    image: ${WS_SERVER_IMAGE}
    network_mode: host
    environment:
      WINGFOIL_WEB_ADDR: "0.0.0.0:8080"
      WINGFOIL_METRICS_ADDR: "0.0.0.0:9091"
      WINGFOIL_OTLP_ENDPOINT: "http://localhost:4318"
      RUST_LOG: info
    restart: unless-stopped

  fix_gw:
    image: ${FIX_GW_IMAGE}
    network_mode: host
    env_file: /etc/wingfoil/lmax.env
    environment:
      RUST_LOG: info
    restart: unless-stopped
EOF

# Spot watcher dependency installed via dnf above (python3-prometheus_client).
# Verify it's importable from /usr/bin/python3 — the systemd unit invokes that
# interpreter, and a silent missing-package would only surface at first boot.
/usr/bin/python3 -c "import prometheus_client"

# Sanity check — fail the build if any image is missing locally.
for img in "${WS_SERVER_IMAGE}" "${FIX_GW_IMAGE}" "${PROMETHEUS_IMAGE}" "${TEMPO_IMAGE}" "${GRAFANA_IMAGE}"; do
  sudo docker image inspect "${img}" >/dev/null
done

echo "Packer install complete."

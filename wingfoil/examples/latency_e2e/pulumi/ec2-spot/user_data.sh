#!/bin/bash
# user_data.sh — first-boot bootstrap for the EC2 Spot ec2-spot stack.
#
# Pulumi templates the placeholders before passing this to EC2. On every boot
# (including replacements after a Spot reclaim), this script:
#   1. Associates the pre-allocated Elastic IP with this instance, so the
#      public URL stays stable across reclaims.
#   2. Fetches the LMAX FIX credentials from Secrets Manager.
#   3. Writes /etc/wingfoil/lmax.env (referenced by the fix_gw container).
#   4. Starts docker compose (the AMI already has Docker + pre-pulled images
#      + the compose file at /opt/wingfoil/docker-compose.yml).
#   5. Starts the spot watcher systemd unit so the Grafana banner has data.
#
# Idempotent — re-running starts already-running containers cleanly.

set -euo pipefail
set -x

EIP_ALLOCATION_ID="__EIP_ALLOCATION_ID__"
LMAX_USERNAME_SECRET="__LMAX_USERNAME_SECRET__"
LMAX_PASSWORD_SECRET="__LMAX_PASSWORD_SECRET__"
AWS_REGION="__AWS_REGION__"

# IMDSv2 — fetch our instance ID for the EIP association call.
IMDS_TOKEN=$(curl -fsSL -X PUT \
  -H "X-aws-ec2-metadata-token-ttl-seconds: 300" \
  http://169.254.169.254/latest/api/token)
INSTANCE_ID=$(curl -fsSL \
  -H "X-aws-ec2-metadata-token: ${IMDS_TOKEN}" \
  http://169.254.169.254/latest/meta-data/instance-id)

# Retry EIP association — fresh instances occasionally hit transient API
# errors / eventual-consistency failures, and cloud-init won't re-run
# user_data, so a one-shot failure leaves the box without a public address.
for attempt in 1 2 3 4 5; do
  if aws ec2 associate-address \
      --region "${AWS_REGION}" \
      --allocation-id "${EIP_ALLOCATION_ID}" \
      --instance-id "${INSTANCE_ID}" \
      --allow-reassociation; then
    break
  fi
  if [ "${attempt}" = "5" ]; then
    echo "ERROR: failed to associate EIP after 5 attempts" >&2
    exit 1
  fi
  sleep $((attempt * 3))
done

# LMAX FIX credentials — written to a root-only env file, then referenced by
# the fix_gw container via env_file:. Disable -x for this section so
# secret values do not appear in /var/log/cloud-init-output.log (which is
# also retrievable via `aws ec2 get-console-output`).
install -d -m 0700 /etc/wingfoil
set +x
LMAX_USERNAME=$(aws secretsmanager get-secret-value \
  --region "${AWS_REGION}" \
  --secret-id "${LMAX_USERNAME_SECRET}" \
  --query SecretString --output text)
LMAX_PASSWORD=$(aws secretsmanager get-secret-value \
  --region "${AWS_REGION}" \
  --secret-id "${LMAX_PASSWORD_SECRET}" \
  --query SecretString --output text)
( umask 077 && cat > /etc/wingfoil/lmax.env <<EOF
LMAX_USERNAME=${LMAX_USERNAME}
LMAX_PASSWORD=${LMAX_PASSWORD}
EOF
)
unset LMAX_USERNAME LMAX_PASSWORD
set -x
chmod 0600 /etc/wingfoil/lmax.env

# ── TLS material for ws_server + Grafana ──────────────────────────────────
# Self-signed cert regenerated on every boot. Browsers will warn (no public
# CA chain), but the WS / iframe traffic is then encrypted on the wire,
# which matters whenever the demo is reachable from a public network. The
# subjectAltName carries the EIP so `https://<eip>:8080` matches the cert
# enough for `openssl s_client`-style verification — browsers still
# require a manual click-through for the unknown root.
PUBLIC_IPV4=$(curl -fsSL \
  -H "X-aws-ec2-metadata-token: ${IMDS_TOKEN}" \
  http://169.254.169.254/latest/meta-data/public-ipv4)

install -d -m 0755 /etc/wingfoil/tls
openssl req -x509 -newkey rsa:2048 -nodes -days 365 \
  -keyout /etc/wingfoil/tls/key.pem \
  -out    /etc/wingfoil/tls/cert.pem \
  -subj "/CN=${PUBLIC_IPV4}" \
  -addext "subjectAltName=IP:${PUBLIC_IPV4}"
# Both files must be world-readable: the ws_server container runs as
# UID 10001 and the grafana container as UID 472. The key is regenerated
# on every boot, so file-system leakage of a stale key buys nothing.
chmod 0644 /etc/wingfoil/tls/cert.pem /etc/wingfoil/tls/key.pem

# Spot watcher — polls IMDS for reclaim notice, exposes a Prometheus gauge
# on :9092 that the Grafana banner panel reads.
cat > /etc/systemd/system/wingfoil-spot-watcher.service <<'EOF'
[Unit]
Description=wingfoil EC2 Spot interruption watcher (Prometheus gauge on :9092)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/opt/wingfoil/venv/bin/python3 /opt/wingfoil/spot_watcher.py
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable --now wingfoil-spot-watcher.service

# Generate a fresh Grafana admin password on every boot. Anonymous Viewer
# is on and the login form is disabled, so nobody logs in interactively
# — this just prevents the historical `admin/admin` default that the
# Grafana API otherwise accepts. Written to `/opt/wingfoil/.env` so
# `docker compose` substitutes `${GF_SECURITY_ADMIN_PASSWORD}`.
set +x
GRAFANA_ADMIN_PASSWORD=$(openssl rand -hex 16)
( umask 077 && cat > /opt/wingfoil/.env <<EOF
GF_SECURITY_ADMIN_PASSWORD=${GRAFANA_ADMIN_PASSWORD}
EOF
)
unset GRAFANA_ADMIN_PASSWORD
set -x
chmod 0600 /opt/wingfoil/.env

# Bind-mount sanity. The AMI's install.sh stages prometheus.yml / tempo.yaml
# / grafana/provisioning under /opt/wingfoil so docker-compose's relative
# bind sources resolve. A previous boot whose compose lacked
# create_host_path: false would have made docker auto-create any missing
# source as an empty directory and cached that mistake on disk; on the next
# boot the bind mount still fails with the misleading "not a directory: Are
# you trying to mount a directory onto a file" because the leaf is now a
# directory where compose expects a file. Detect and remove empty
# auto-created directories at the file paths so a single re-run recovers,
# then assert the final layout matches what compose expects — a stale or
# pre-#298 AMI fails fast here with a clear message instead of as an opaque
# OCI runtime error during `compose up`.
for stale in \
    /opt/wingfoil/prometheus/prometheus.yml \
    /opt/wingfoil/tempo/tempo.yaml; do
  if [ -d "${stale}" ] && [ -z "$(ls -A "${stale}")" ]; then
    rmdir "${stale}"
  fi
done
layout_ok=true
for f in \
    /opt/wingfoil/prometheus/prometheus.yml \
    /opt/wingfoil/tempo/tempo.yaml; do
  if [ ! -f "${f}" ]; then
    echo "ERROR: expected bind-mount source '${f}' is missing or not a regular file." >&2
    layout_ok=false
  fi
done
if [ ! -d /opt/wingfoil/grafana/provisioning ]; then
  echo "ERROR: expected bind-mount source '/opt/wingfoil/grafana/provisioning' is missing or not a directory." >&2
  layout_ok=false
fi
if ! ${layout_ok}; then
  echo "ERROR: AMI does not have the latency_e2e configs baked in correctly." >&2
  echo "       Rebuild the AMI from a commit at or after #298 and update ami_id." >&2
  exit 1
fi

# ── docker-compose override (boot-time, AMI-agnostic) ────────────────────
# Two things need to be true for the round trip to work:
#
#  1. ws_server and fix_gw must share an IPC namespace. iceoryx2 stores its
#     segments under /dev/shm/iox2_*; Docker's default per-container IPC
#     namespace gives each container a private /dev/shm tmpfs, so without
#     `ipc: host` the two containers can't see each other's iceoryx2
#     segments — orders never reach fix_gw, and no fills come back.
#     `network_mode: host` is *not* sufficient; that only shares the
#     network namespace.
#
#  2. Stale iox2_* segments from a previous boot (e.g. Spot reclaim killed
#     the containers before they cleaned up) must be removed first, or the
#     fresh containers fail with IncompatibleTypes when they try to open
#     services that already exist with mismatched generation IDs.
#
# Doing this in user_data.sh (rather than only in the Packer install.sh)
# means the fix applies on every boot, including instances launched from an
# AMI baked before #313 — the fix doesn't depend on which AMI is in use.
# install.sh also sets `ipc: host` directly in docker-compose.yml on newer
# AMIs; the override below is idempotent in that case.
#
# The override also re-declares Grafana's HTTPS config, which install.sh
# previously wrote to docker-compose.override.yml. Re-declaring it here
# keeps a single source of truth — the override on disk after this section
# is the one written below, regardless of AMI version.
rm -f /dev/shm/iox2_* || true

cat > /opt/wingfoil/docker-compose.override.yml <<'EOF'
services:
  ws_server:
    ipc: host
  fix_gw:
    ipc: host
  grafana:
    environment:
      GF_SERVER_PROTOCOL: "https"
      GF_SERVER_CERT_FILE: "/etc/wingfoil/tls/cert.pem"
      GF_SERVER_CERT_KEY: "/etc/wingfoil/tls/key.pem"
    volumes:
      - /etc/wingfoil/tls:/etc/wingfoil/tls:ro
EOF
chmod 0644 /opt/wingfoil/docker-compose.override.yml

# Bring up the demo stack — images already cached in the AMI, so this is a
# fast `docker run` per service rather than a registry pull.
( cd /opt/wingfoil && docker compose up -d )

echo "user_data complete: $(date -u +%FT%TZ)"

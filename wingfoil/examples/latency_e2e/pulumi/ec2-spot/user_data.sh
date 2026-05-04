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
# Let's Encrypt opt-in. Empty when the operator hasn't set `dns_hostname` in
# Pulumi config, in which case we keep the self-signed-cert path below.
DNS_HOSTNAME="__DNS_HOSTNAME__"
CERT_BUCKET="__CERT_BUCKET__"

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
# Two modes, selected by whether DNS_HOSTNAME was set in Pulumi config:
#
#  1. DNS_HOSTNAME unset → self-signed cert regenerated on every boot.
#     Browsers warn (no public CA chain), but the WS / iframe traffic is
#     still encrypted on the wire. The subjectAltName carries the EIP so
#     `https://<eip>` matches the cert enough for
#     `openssl s_client`-style verification.
#
#  2. DNS_HOSTNAME set → Let's Encrypt cert fetched via certbot in
#     --standalone mode (HTTP-01 on :80). Cert state is cached in S3 so a
#     Spot reclaim doesn't burn a fresh issuance against LE's 50/week/domain
#     limit. A daily systemd timer renews and bounces the containers if the
#     cert was actually rotated. If the LE flow fails for any reason
#     (DNS hasn't propagated yet, certbot pull error, etc.) we fall back
#     to the self-signed path so the demo still comes up.
# Read the EIP from the allocation we just associated rather than IMDS's
# `public-ipv4`. IMDS reflects the EIP only after the association
# propagates, and reading it too early returns the *ephemeral* public IP
# the instance launched with. That stale value then makes `wait_for_dns`
# loop forever (DNS correctly points at the EIP) and burns the boot
# into the self-signed fallback with the wrong IP in the SAN — the
# WSS handshake from the browser then fails silently and the demo
# looks "connected" but no frames flow.
PUBLIC_IPV4=$(aws ec2 describe-addresses \
  --region "${AWS_REGION}" \
  --allocation-ids "${EIP_ALLOCATION_ID}" \
  --query 'Addresses[0].PublicIp' --output text)

install -d -m 0755 /etc/wingfoil/tls

write_self_signed_cert() {
  # Include DNS_HOSTNAME in the SAN when set, so a self-signed fallback
  # still lets browsers complete the WSS handshake against the hostname.
  local san="IP:${PUBLIC_IPV4}"
  local subj="/CN=${PUBLIC_IPV4}"
  if [ -n "${DNS_HOSTNAME}" ]; then
    san="DNS:${DNS_HOSTNAME},${san}"
    subj="/CN=${DNS_HOSTNAME}"
  fi
  openssl req -x509 -newkey rsa:2048 -nodes -days 365 \
    -keyout /etc/wingfoil/tls/key.pem \
    -out    /etc/wingfoil/tls/cert.pem \
    -subj "${subj}" \
    -addext "subjectAltName=${san}"
  # Both files must be world-readable: the ws_server container runs as
  # UID 10001 and the grafana container as UID 472. The key is regenerated
  # on every boot, so file-system leakage of a stale key buys nothing.
  chmod 0644 /etc/wingfoil/tls/cert.pem /etc/wingfoil/tls/key.pem
}

publish_le_cert() {
  # Copy the LE-issued material out of /etc/letsencrypt (root-only) into
  # /etc/wingfoil/tls (world-readable, mounted by ws_server + grafana).
  cp -L "/etc/letsencrypt/live/${DNS_HOSTNAME}/fullchain.pem" /etc/wingfoil/tls/cert.pem
  cp -L "/etc/letsencrypt/live/${DNS_HOSTNAME}/privkey.pem"   /etc/wingfoil/tls/key.pem
  chmod 0644 /etc/wingfoil/tls/cert.pem /etc/wingfoil/tls/key.pem
}

push_le_cert_to_s3() {
  # Tar /etc/letsencrypt as the unit of state — certbot's `live/`
  # symlinks reference `archive/` and `renewal/` config, so all three
  # need to ride together.
  tar -C / -czf /tmp/letsencrypt.tar.gz etc/letsencrypt
  aws s3 cp /tmp/letsencrypt.tar.gz \
    "s3://${CERT_BUCKET}/${DNS_HOSTNAME}/letsencrypt.tar.gz" \
    --region "${AWS_REGION}"
  rm -f /tmp/letsencrypt.tar.gz
}

restore_le_cert_from_s3() {
  if aws s3 cp \
      "s3://${CERT_BUCKET}/${DNS_HOSTNAME}/letsencrypt.tar.gz" \
      /tmp/letsencrypt.tar.gz \
      --region "${AWS_REGION}" 2>/dev/null; then
    tar -C / -xzf /tmp/letsencrypt.tar.gz
    rm -f /tmp/letsencrypt.tar.gz
    return 0
  fi
  return 1
}

run_certbot() {
  # certonly --standalone --keep-until-expiring is a no-op when the cached
  # cert has >30d left, and a fresh issuance otherwise. --non-interactive +
  # --agree-tos is required for unattended runs.
  # --register-unsafely-without-email skips the LE expiry-warning email
  # prompt: this stack's daily renewal timer (wingfoil-le-renew.timer)
  # makes that warning a safety net we don't actually use, so we don't
  # hold an operator email in config for it.
  # Bind /etc/letsencrypt for state, publish :80 for the HTTP-01 challenge.
  docker run --rm \
    -v /etc/letsencrypt:/etc/letsencrypt \
    -v /var/lib/letsencrypt:/var/lib/letsencrypt \
    -p 80:80 \
    certbot/certbot:latest \
    certonly --standalone --non-interactive --agree-tos --keep-until-expiring \
    --register-unsafely-without-email \
    -d "${DNS_HOSTNAME}"
}

wait_for_dns() {
  # Up to ~5 min — Route53 propagates in seconds, third-party DNS providers
  # can take a couple of minutes after `pulumi up`. Uses the system resolver
  # (Amazon's VPC resolver on AL2023) which doesn't cache negative results
  # for long, so a freshly created record shows up promptly.
  local attempt resolved
  for attempt in $(seq 1 30); do
    resolved=$(getent hosts "${DNS_HOSTNAME}" 2>/dev/null | awk 'NR==1 {print $1}' || true)
    if [ "${resolved}" = "${PUBLIC_IPV4}" ]; then
      return 0
    fi
    echo "waiting for ${DNS_HOSTNAME} to resolve to ${PUBLIC_IPV4} (got '${resolved}', attempt ${attempt}/30)"
    sleep 10
  done
  return 1
}

if [ -n "${DNS_HOSTNAME}" ]; then
  install -d -m 0700 /etc/letsencrypt
  install -d -m 0700 /var/lib/letsencrypt
  # Best-effort restore from S3 — first deploy will 404, that's fine.
  restore_le_cert_from_s3 || echo "no cached cert in s3://${CERT_BUCKET}, will issue a fresh one"

  if wait_for_dns && run_certbot; then
    publish_le_cert
    push_le_cert_to_s3
    LE_OK=1
  else
    echo "WARNING: Let's Encrypt flow failed; falling back to self-signed cert" >&2
    write_self_signed_cert
    LE_OK=0
  fi
else
  write_self_signed_cert
  LE_OK=0
fi

# Daily renewal timer — only set up when LE is actually live. The renew
# subcommand is idempotent: it skips certs with >30d remaining, so the
# common-case run is a no-op. The deploy-hook fires only on actual
# rotation, where we re-publish to /etc/wingfoil/tls, push the new state
# to S3, and bounce the two containers that read the cert at startup.
if [ "${LE_OK}" = "1" ]; then
  cat > /opt/wingfoil/le-renew.sh <<EOF
#!/bin/bash
set -euo pipefail
docker run --rm \\
  -v /etc/letsencrypt:/etc/letsencrypt \\
  -v /var/lib/letsencrypt:/var/lib/letsencrypt \\
  -p 80:80 \\
  certbot/certbot:latest \\
  renew --non-interactive \\
  --deploy-hook "echo rotated" >/var/log/wingfoil-le-renew.log 2>&1

# certbot --deploy-hook only runs when at least one cert was actually
# renewed. Detect that by comparing the on-disk fingerprint before/after.
NEW_FP=\$(openssl x509 -in /etc/letsencrypt/live/${DNS_HOSTNAME}/cert.pem -noout -fingerprint -sha256)
OLD_FP=\$(openssl x509 -in /etc/wingfoil/tls/cert.pem -noout -fingerprint -sha256 2>/dev/null || echo "")
if [ "\${NEW_FP}" != "\${OLD_FP}" ]; then
  cp -L /etc/letsencrypt/live/${DNS_HOSTNAME}/fullchain.pem /etc/wingfoil/tls/cert.pem
  cp -L /etc/letsencrypt/live/${DNS_HOSTNAME}/privkey.pem   /etc/wingfoil/tls/key.pem
  chmod 0644 /etc/wingfoil/tls/cert.pem /etc/wingfoil/tls/key.pem
  tar -C / -czf /tmp/letsencrypt.tar.gz etc/letsencrypt
  aws s3 cp /tmp/letsencrypt.tar.gz \\
    s3://${CERT_BUCKET}/${DNS_HOSTNAME}/letsencrypt.tar.gz \\
    --region ${AWS_REGION}
  rm -f /tmp/letsencrypt.tar.gz
  ( cd /opt/wingfoil && docker compose restart ws_server grafana )
fi
EOF
  chmod 0755 /opt/wingfoil/le-renew.sh

  cat > /etc/systemd/system/wingfoil-le-renew.service <<'EOF'
[Unit]
Description=wingfoil Let's Encrypt cert renewal
After=docker.service

[Service]
Type=oneshot
ExecStart=/opt/wingfoil/le-renew.sh
EOF

  cat > /etc/systemd/system/wingfoil-le-renew.timer <<'EOF'
[Unit]
Description=wingfoil Let's Encrypt cert renewal (daily)

[Timer]
OnBootSec=1h
OnUnitActiveSec=24h
RandomizedDelaySec=30min

[Install]
WantedBy=timers.target
EOF

  systemctl daemon-reload
  systemctl enable --now wingfoil-le-renew.timer
fi

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
#
# The static/ bind mount was only added in #324, so its source path
# (/opt/wingfoil/static) is checked conditionally — gated on whether the
# baked compose.yml actually references it. An older AMI (pre-#324) whose
# compose.yml has no static bind mount must still boot cleanly and serve
# the in-image /app/static; failing fast on a missing source path that
# compose doesn't even use would brick the demo on stale AMIs (no listener
# on :443 -> ERR_CONNECTION_REFUSED on https://demo.wingfoil.io/).
for stale in \
    /opt/wingfoil/prometheus/prometheus.yml \
    /opt/wingfoil/tempo/tempo.yaml; do
  if [ -d "${stale}" ] && [ -z "$(ls -A "${stale}")" ]; then
    rmdir "${stale}"
  fi
done
required_files=(
  /opt/wingfoil/prometheus/prometheus.yml
  /opt/wingfoil/tempo/tempo.yaml
)
if grep -qF '/opt/wingfoil/static' /opt/wingfoil/docker-compose.yml; then
  required_files+=(/opt/wingfoil/static/index.html)
fi
layout_ok=true
for f in "${required_files[@]}"; do
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

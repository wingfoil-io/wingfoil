#!/bin/bash
# user_data.sh — first-boot bootstrap for the wingfoil latency_e2e bare-metal box.
#
# Pulumi templates the placeholders below before passing this to EC2. The
# script runs as root via cloud-init on first boot, configures grub for CPU
# isolation, reboots once to apply, then on the second boot installs the
# wingfoil binaries from S3 and starts them as systemd units pinned to the
# isolated cores. Observability (prometheus, tempo, grafana) runs on the
# housekeeping cores via docker `--cpuset-cpus`.
#
# Idempotent: gated on /var/lib/wingfoil-bootstrapped so a manual re-run is safe.

set -euxo pipefail

ISOLATED_CORES="__ISOLATED_CORES__"
WS_SERVER_CORE="__WS_SERVER_CORE__"
FIX_GW_CORE="__FIX_GW_CORE__"
BINARIES_BUCKET="__BINARIES_BUCKET__"
LMAX_USERNAME_SECRET="__LMAX_USERNAME_SECRET__"
LMAX_PASSWORD_SECRET="__LMAX_PASSWORD_SECRET__"
AWS_REGION="__AWS_REGION__"

STAMP_FILE=/var/lib/wingfoil-bootstrap-stage
mkdir -p /var/lib

stage="$(cat "$STAMP_FILE" 2>/dev/null || echo init)"

if [ "$stage" = "init" ]; then
  # ── Stage 1: configure grub, reboot ────────────────────────────────────
  # isolcpus removes these cores from the general scheduler; nohz_full + rcu_nocbs
  # silence the periodic timer tick + RCU callbacks on them so a busy graph
  # thread isn't preempted for kernel housekeeping.
  GRUB_ARGS="isolcpus=${ISOLATED_CORES} nohz_full=${ISOLATED_CORES} rcu_nocbs=${ISOLATED_CORES}"
  sed -i "s|^GRUB_CMDLINE_LINUX_DEFAULT=\"\(.*\)\"|GRUB_CMDLINE_LINUX_DEFAULT=\"\1 ${GRUB_ARGS}\"|" /etc/default/grub
  update-grub
  echo installed > "$STAMP_FILE"
  reboot
  exit 0
fi

if [ "$stage" = "installed" ]; then
  # ── Stage 2: install packages, fetch binaries + creds, write systemd units ─
  apt-get update
  apt-get install -y awscli docker.io docker-compose-v2

  # Verify isolation actually applied — if grub didn't pick up the change,
  # fail loudly rather than silently run unpinned.
  if ! grep -q "isolcpus=${ISOLATED_CORES}" /proc/cmdline; then
    echo "FATAL: isolcpus not applied — /proc/cmdline = $(cat /proc/cmdline)" >&2
    exit 1
  fi

  install -d -m 0755 /opt/wingfoil /opt/wingfoil/static /opt/wingfoil/observability
  aws s3 cp   "s3://${BINARIES_BUCKET}/latency_e2e_ws_server" /opt/wingfoil/latency_e2e_ws_server --region "${AWS_REGION}"
  aws s3 cp   "s3://${BINARIES_BUCKET}/latency_e2e_fix_gw"    /opt/wingfoil/latency_e2e_fix_gw    --region "${AWS_REGION}"
  aws s3 sync "s3://${BINARIES_BUCKET}/static/"               /opt/wingfoil/static/               --region "${AWS_REGION}"
  aws s3 sync "s3://${BINARIES_BUCKET}/observability/"        /opt/wingfoil/observability/        --region "${AWS_REGION}"
  chmod 0755 /opt/wingfoil/latency_e2e_ws_server /opt/wingfoil/latency_e2e_fix_gw

  LMAX_USERNAME=$(aws secretsmanager get-secret-value --secret-id "${LMAX_USERNAME_SECRET}" --region "${AWS_REGION}" --query SecretString --output text)
  LMAX_PASSWORD=$(aws secretsmanager get-secret-value --secret-id "${LMAX_PASSWORD_SECRET}" --region "${AWS_REGION}" --query SecretString --output text)

  # Write LMAX creds to a root-only env file rather than embedding in the
  # systemd unit (which would expose them via `systemctl show`).
  install -d -m 0700 /etc/wingfoil
  cat > /etc/wingfoil/lmax.env <<EOF
LMAX_USERNAME=${LMAX_USERNAME}
LMAX_PASSWORD=${LMAX_PASSWORD}
EOF
  chmod 0600 /etc/wingfoil/lmax.env

  cat > /etc/systemd/system/wingfoil-ws-server.service <<EOF
[Unit]
Description=wingfoil latency_e2e ws_server (graph thread pinned to core ${WS_SERVER_CORE})
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory=/opt/wingfoil
Environment=RUST_LOG=info
Environment=WINGFOIL_PRECISE_STAMPS=1
Environment=WINGFOIL_PIN_GRAPH=${WS_SERVER_CORE}
Environment=WINGFOIL_STATIC_DIR=/opt/wingfoil/static
ExecStart=/opt/wingfoil/latency_e2e_ws_server --addr 0.0.0.0:8080 --precise
Restart=on-failure
RestartSec=2
# Belt-and-braces alongside WINGFOIL_PIN_GRAPH: also restrict the whole
# process (including any future helper threads) to the hot core.
CPUAffinity=${WS_SERVER_CORE}
LimitMEMLOCK=infinity

[Install]
WantedBy=multi-user.target
EOF

  cat > /etc/systemd/system/wingfoil-fix-gw.service <<EOF
[Unit]
Description=wingfoil latency_e2e fix_gw (graph thread pinned to core ${FIX_GW_CORE})
After=network-online.target wingfoil-ws-server.service
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory=/opt/wingfoil
EnvironmentFile=/etc/wingfoil/lmax.env
Environment=RUST_LOG=info
Environment=WINGFOIL_PRECISE_STAMPS=1
Environment=WINGFOIL_PIN_GRAPH=${FIX_GW_CORE}
ExecStart=/opt/wingfoil/latency_e2e_fix_gw --precise
Restart=on-failure
RestartSec=2
CPUAffinity=${FIX_GW_CORE}
LimitMEMLOCK=infinity

[Install]
WantedBy=multi-user.target
EOF

  # Observability stack — kept on housekeeping cores 0-1 via cpuset so it
  # never competes with the hot-path threads. Configs were synced from S3
  # into /opt/wingfoil/observability/{prometheus,grafana,tempo} above.
  cat > /opt/wingfoil/observability/docker-compose.yml <<'EOF'
services:
  prometheus:
    image: prom/prometheus:v2.55.1
    cpuset: "0-1"
    network_mode: host
    volumes:
      - ./prometheus:/etc/prometheus:ro
    command: ["--config.file=/etc/prometheus/prometheus.yml"]
    restart: unless-stopped
  tempo:
    image: grafana/tempo:2.6.1
    cpuset: "0-1"
    network_mode: host
    volumes:
      - ./tempo:/etc/tempo:ro
    command: ["-config.file=/etc/tempo/tempo.yaml"]
    restart: unless-stopped
  grafana:
    image: grafana/grafana:11.3.0
    cpuset: "0-1"
    network_mode: host
    volumes:
      - ./grafana/provisioning:/etc/grafana/provisioning:ro
    environment:
      GF_AUTH_ANONYMOUS_ENABLED: "true"
      GF_AUTH_ANONYMOUS_ORG_ROLE: Viewer
      GF_AUTH_DISABLE_LOGIN_FORM: "true"
      GF_SECURITY_ALLOW_EMBEDDING: "true"
      GF_FEATURE_TOGGLES_ENABLE: traceqlEditor
    restart: unless-stopped
EOF

  systemctl daemon-reload
  systemctl enable --now wingfoil-ws-server.service wingfoil-fix-gw.service
  ( cd /opt/wingfoil/observability && docker compose up -d )

  # ── Idle-stop watcher ──────────────────────────────────────────────────
  # Self-stops the instance after 10 min uptime + 10 min of zero active
  # sessions. Prevents a forgotten visitor from leaving the $4.28/hr box
  # awake for hours.
  aws s3 cp "s3://${BINARIES_BUCKET}/idle_watch.sh" /opt/wingfoil/idle_watch.sh --region "${AWS_REGION}"
  chmod 0755 /opt/wingfoil/idle_watch.sh

  cat > /etc/systemd/system/wingfoil-idle-watch.service <<'EOF'
[Unit]
Description=wingfoil idle-stop check
After=wingfoil-ws-server.service

[Service]
Type=oneshot
ExecStart=/opt/wingfoil/idle_watch.sh
EOF

  cat > /etc/systemd/system/wingfoil-idle-watch.timer <<'EOF'
[Unit]
Description=wingfoil idle-stop watcher (runs every minute)

[Timer]
OnBootSec=2min
OnUnitActiveSec=1min
AccuracySec=10s

[Install]
WantedBy=timers.target
EOF

  systemctl daemon-reload
  systemctl enable --now wingfoil-idle-watch.timer

  echo running > "$STAMP_FILE"
fi

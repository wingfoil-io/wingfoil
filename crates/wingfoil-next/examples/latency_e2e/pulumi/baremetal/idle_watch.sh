#!/bin/bash
# idle_watch.sh — self-stop the bare-metal demo box when nobody's using it.
#
# Stops the instance when:
#   * uptime ≥ BOOT_GRACE_MINUTES, AND
#   * the local prometheus exporter has reported zero `latency_e2e_active_sessions`
#     continuously for ≥ IDLE_MINUTES.
#
# Designed to be invoked from a 1-min systemd timer. Exits 0 in all
# non-terminal cases so the timer never red-flags. The `aws ec2 stop-instances`
# call works because the instance role grants `ec2:StopInstances` on itself
# (see __main__.py).

set -euo pipefail

BOOT_GRACE_MINUTES=10
IDLE_MINUTES=10
PROMETHEUS_URL="http://localhost:9091/metrics"
STAMP_FILE=/var/lib/wingfoil-last-active
TAG="wingfoil-idle-watch"

mkdir -p /var/lib

log() { logger -t "$TAG" -- "$*"; }

# 1. Boot grace — don't even look at activity for the first BOOT_GRACE_MINUTES.
#    Pin the idle stamp to "now" during grace so the idle countdown starts
#    fresh once we exit it.
uptime_min=$(awk '{print int($1/60)}' /proc/uptime)
if [ "$uptime_min" -lt "$BOOT_GRACE_MINUTES" ]; then
  date +%s > "$STAMP_FILE"
  exit 0
fi

# 2. Read the active-sessions gauge. If the exporter isn't responding (binary
#    crashed, restarting, …) we treat that as idle — keeping a broken box
#    awake serves no one.
active=$(curl -fsS --max-time 2 "$PROMETHEUS_URL" 2>/dev/null \
         | awk '/^latency_e2e_active_sessions / {print $2; exit}' || true)
# Trim decimals / scientific notation that prom may emit for floats.
active=${active%%.*}
active=${active%%e*}
active=${active:-0}

if [ "$active" -gt 0 ] 2>/dev/null; then
  date +%s > "$STAMP_FILE"
  exit 0
fi

# 3. Idle. Has it been long enough?
last_active=$(cat "$STAMP_FILE" 2>/dev/null || true)
if [ -z "$last_active" ]; then
  date +%s > "$STAMP_FILE"
  exit 0
fi

now=$(date +%s)
idle_for_min=$(( (now - last_active) / 60 ))

if [ "$idle_for_min" -lt "$IDLE_MINUTES" ]; then
  exit 0
fi

# 4. Stop ourselves. IMDSv2 token first, then instance-id + region.
TOKEN=$(curl -fsS --max-time 2 -X PUT "http://169.254.169.254/latest/api/token" \
        -H "X-aws-ec2-metadata-token-ttl-seconds: 60")
IID=$(curl -fsS --max-time 2 -H "X-aws-ec2-metadata-token: $TOKEN" \
      http://169.254.169.254/latest/meta-data/instance-id)
REGION=$(curl -fsS --max-time 2 -H "X-aws-ec2-metadata-token: $TOKEN" \
         http://169.254.169.254/latest/meta-data/placement/region)

log "stopping $IID after ${idle_for_min} min idle (uptime ${uptime_min} min)"
aws ec2 stop-instances --instance-ids "$IID" --region "$REGION"

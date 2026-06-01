#!/usr/bin/env bash
# usage: scripts/ci-logs.sh <job_id>   (or pipe failed-only run logs)
set -euo pipefail
job="$1"; repo="wingfoil-io/wingfoil"
url=$(curl -s -o /dev/null -w '%{redirect_url}' \
  -H "Authorization: Bearer $GH_TOKEN" \
  "https://api.github.com/repos/$repo/actions/jobs/$job/logs")
curl -s "$url"   # signed URL carries its own auth; no header forwarded

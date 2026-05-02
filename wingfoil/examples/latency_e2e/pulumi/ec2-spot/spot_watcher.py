#!/usr/bin/env python3
"""Polls IMDSv2 for an EC2 Spot interruption notice and exposes a Prometheus
gauge so the Grafana dashboard can show a "restarting in N seconds" banner.

  - GET /latest/meta-data/spot/instance-action returns 404 normally and a JSON
    {"action": "...", "time": "<ISO8601>"} ~2 minutes before reclaim.
  - The gauge `wingfoil_spot_termination_seconds_remaining` is -1 when no
    notice is active, otherwise the seconds remaining until termination.

Runs as a systemd unit on port 9092. Prometheus (running as a host-network
container on the same instance) scrapes localhost:9092.
"""

from __future__ import annotations

import json
import logging
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone

from prometheus_client import Gauge, start_http_server

logger = logging.getLogger("spot_watcher")

IMDS_BASE = "http://169.254.169.254/latest"
TOKEN_TTL_SECONDS = 300
POLL_INTERVAL_SECONDS = 5
METRICS_PORT = 9092

seconds_remaining = Gauge(
    "wingfoil_spot_termination_seconds_remaining",
    "Seconds until AWS terminates this Spot instance; -1 when no notice is active.",
)


def _imds_token() -> str:
    req = urllib.request.Request(
        f"{IMDS_BASE}/api/token",
        method="PUT",
        headers={"X-aws-ec2-metadata-token-ttl-seconds": str(TOKEN_TTL_SECONDS)},
    )
    with urllib.request.urlopen(req, timeout=2) as resp:
        return resp.read().decode("utf-8")


def _imds_get(path: str, token: str) -> str | None:
    req = urllib.request.Request(
        f"{IMDS_BASE}/{path}",
        headers={"X-aws-ec2-metadata-token": token},
    )
    try:
        with urllib.request.urlopen(req, timeout=2) as resp:
            return resp.read().decode("utf-8")
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return None
        raise


def _parse_remaining(payload: str) -> float:
    data = json.loads(payload)
    terminate_at = datetime.fromisoformat(data["time"].replace("Z", "+00:00"))
    return (terminate_at - datetime.now(tz=timezone.utc)).total_seconds()


def main() -> None:
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    start_http_server(METRICS_PORT)
    seconds_remaining.set(-1)

    while True:
        # Catch broadly: a malformed payload or unexpected IMDS schema must not
        # kill the loop — we'd lose the metric until systemd restarts us.
        try:
            token = _imds_token()
            payload = _imds_get("meta-data/spot/instance-action", token)
            if payload is None:
                seconds_remaining.set(-1)
            else:
                seconds_remaining.set(max(0.0, _parse_remaining(payload)))
        except (urllib.error.URLError, TimeoutError) as e:
            logger.warning("IMDS unreachable: %s", e)
            seconds_remaining.set(-1)
        except Exception:  # noqa: BLE001 — keep the loop alive on parse errors
            logger.exception("failed to parse spot interruption notice")
            seconds_remaining.set(-1)

        time.sleep(POLL_INTERVAL_SECONDS)


if __name__ == "__main__":
    main()

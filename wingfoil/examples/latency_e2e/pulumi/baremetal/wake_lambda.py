"""
Wake-on-demand Lambda for the wingfoil bare-metal perf demo.

Three routes on a single function URL:

    GET  /         → static HTML page with a "wake" button
    GET  /status   → current instance state + public_ip as JSON
    POST /wake     → start the instance if stopped; return state as JSON

The page polls /status every 15s after a wake click and switches to a
"box is up — open the demo" link once the EC2 instance reports
`running`. Note that EC2 reporting `running` only means the hypervisor
has booted; ws_server itself takes another ~30s after that to come up,
so the link will 502 briefly. The page handles that by retrying.
"""

import hmac
import json
import os

import boto3

ec2 = boto3.client("ec2")
INSTANCE_ID = os.environ["INSTANCE_ID"]
WS_SERVER_PORT = int(os.environ.get("WS_SERVER_PORT", "8080"))
# Optional shared secret. When set, every HTTP request must carry
# `?token=<value>` (or matching `X-Wake-Token` header) — including the GET
# that serves the HTML page. Empty disables the check (back-compat with
# stacks that haven't set it yet).
WAKE_TOKEN = os.environ.get("WAKE_TOKEN", "")


HTML = """<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>wingfoil — perf demo</title>
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <style>
    body { font-family: system-ui, sans-serif; max-width: 640px; margin: 4em auto; padding: 0 1em; line-height: 1.5; color: #222; }
    h1 { margin-bottom: 0.2em; }
    .sub { color: #666; margin-top: 0; }
    button { font-size: 1em; padding: 0.6em 1.2em; cursor: pointer; border: 1px solid #333; background: #fff; border-radius: 4px; }
    button:disabled { cursor: not-allowed; opacity: 0.6; }
    #status { margin-top: 1.5em; padding: 1em; background: #f4f4f4; border-radius: 4px; min-height: 2em; font-family: ui-monospace, monospace; font-size: 0.9em; }
    a { color: #0366d6; }
    .warn { color: #b08800; }
    .err { color: #b22; }
  </style>
</head>
<body>
  <h1>Wingfoil bare-metal perf demo</h1>
  <p class="sub">Sub-microsecond intra-process hops on dedicated, isolated CPU cores.</p>
  <p>The demo runs on a bare-metal EC2 instance that auto-stops after 10 min idle to keep costs bounded. Click below to wake it; takes about <b>5–8 minutes</b> to be ready.</p>
  <button id="wake">Wake perf box</button>
  <div id="status">…</div>
  <script>
    const btn = document.getElementById("wake");
    const out = document.getElementById("status");
    let pollHandle = null;

    // Forward the token (if any) from the page URL onto every API call so
    // /status and /wake see the same shared secret the user used to load
    // this page.
    const pageToken = new URLSearchParams(window.location.search).get("token") || "";
    async function call(path, method = "GET") {
      const url = pageToken ? path + "?token=" + encodeURIComponent(pageToken) : path;
      const r = await fetch(url, { method });
      return await r.json();
    }

    function render(s) {
      const state = s.state || "unknown";
      if (state === "running" && s.ws_server_url) {
        out.innerHTML =
          `Box is up — <a href="${s.ws_server_url}">open the demo →</a>` +
          `<br><span class="warn">If the link 502s, the binaries are still starting; retry after ~30s.</span>`;
        btn.disabled = true;
        if (pollHandle) { clearInterval(pollHandle); pollHandle = null; }
        return true;
      }
      const map = {
        stopped:  "Stopped. Click \\"Wake perf box\\" to start.",
        pending:  "Starting up… (5–8 minutes)",
        running:  "Running, waiting on public IP…",
        stopping: "Stopping (idle-stop in progress). Wait until \\"stopped\\" before waking.",
      };
      out.textContent = map[state] || `State: ${state}`;
      btn.disabled = state !== "stopped";
      return false;
    }

    async function check() {
      try { render(await call("/status")); }
      catch (e) { out.innerHTML = `<span class="err">status check failed: ${e}</span>`; }
    }

    btn.addEventListener("click", async () => {
      btn.disabled = true;
      out.textContent = "Sending wake request…";
      try {
        render(await call("/wake", "POST"));
        if (!pollHandle) pollHandle = setInterval(check, 15000);
      } catch (e) {
        out.innerHTML = `<span class="err">wake failed: ${e}</span>`;
        btn.disabled = false;
      }
    });

    check();
  </script>
</body>
</html>
"""


def _describe():
    r = ec2.describe_instances(InstanceIds=[INSTANCE_ID])
    inst = r["Reservations"][0]["Instances"][0]
    state = inst["State"]["Name"]
    public_ip = inst.get("PublicIpAddress")
    return {
        "state": state,
        "public_ip": public_ip,
        "ws_server_url": f"http://{public_ip}:{WS_SERVER_PORT}" if public_ip and state == "running" else None,
    }


def _json(status, body):
    return {
        "statusCode": status,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps(body),
    }


def _token_ok(event) -> bool:
    if not WAKE_TOKEN:
        return True
    qs = event.get("queryStringParameters") or {}
    headers = {k.lower(): v for k, v in (event.get("headers") or {}).items()}
    presented = qs.get("token") or headers.get("x-wake-token") or ""
    # Constant-time compare so the function URL doesn't leak the secret one
    # byte at a time via response timing.
    return hmac.compare_digest(presented, WAKE_TOKEN)


def handler(event, _context):
    http = event.get("requestContext", {}).get("http", {})
    method = http.get("method", "GET")
    path = event.get("rawPath", "/")

    if not _token_ok(event):
        return _json(401, {"error": "missing or invalid token"})

    if method == "GET" and path in ("/", "/index.html"):
        return {
            "statusCode": 200,
            "headers": {"Content-Type": "text/html; charset=utf-8"},
            "body": HTML,
        }

    if method == "GET" and path == "/status":
        return _json(200, _describe())

    if method == "POST" and path == "/wake":
        info = _describe()
        if info["state"] == "stopped":
            ec2.start_instances(InstanceIds=[INSTANCE_ID])
            return _json(202, {**info, "state": "pending", "started": True})
        # Already running / pending / stopping — return current state.
        return _json(200, {**info, "started": False})

    return _json(404, {"error": "not found", "path": path, "method": method})

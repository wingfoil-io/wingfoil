#!/usr/bin/env bash
#
# Assemble the browser-side dependencies of the `trading_e2e` example into
# `crates/wingfoil/examples/showcase/trading_e2e/static/vendor/`, so the demo
# page serves them from its own origin instead of fetching them from a CDN at
# page load.
#
# Why this exists
# ---------------
# The demo page used to resolve `@wingfoil/client` from esm.sh via an
# importmap, which made a *deployed* demo depend on an npm publish having
# already happened — you could not deploy a commit until its version was on
# npm, which is backwards when the deploy is what gates the release. It also
# put a third-party CDN in the critical path of "does the page load at all",
# and let the served client drift from the server it talks to even though the
# two share a wire format (`wingfoil-wire-types`).
#
# Building the client from the same commit as the server makes that agreement
# a build-time property rather than a deploy-time coincidence.
#
# What it produces
# ----------------
#   static/vendor/client/      <- js/ (this repo) built via wasm-pack + tsc
#   static/vendor/telemetry/   <- @wingfoil/telemetry, pinned (separate repo)
#   static/vendor/uplot/       <- uplot ESM build + stylesheet, pinned
#
# The directory is generated, not committed — see static/.gitignore. Both
# consumers of `static/` build it first:
#
#   * `build-trading-e2e-images.yml`, before `docker build` (the tree is the
#     build context, and Dockerfile.ws_server COPYs static/ into the image).
#   * `build-trading-e2e-ami.yml`, before Packer uploads `static/` to the AMI
#     — the AMI's copy bind-mounts *over* the image's at
#     `/opt/wingfoil/static`, so building only the image would leave the
#     overlay shadowing it with a vendor-less page.
#
# Run it by hand before `cargo run --example trading_e2e_ws_server` too; the
# example serves this same directory in local dev.
#
# Requires: node, pnpm, wasm-pack, and the wasm32-unknown-unknown Rust target.

set -euo pipefail

# Pinned third-party versions. `@wingfoil/telemetry` lives in its own
# repository (wingfoil-io/wingfoil-telemetry-js) and is consumed from npm;
# uplot is its (optional) peer dependency, which we must supply ourselves —
# see the importmap note below.
TELEMETRY_VERSION="0.1.0"
UPLOT_VERSION="1.6.31"

REPO_ROOT="$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)"
STATIC_DIR="${REPO_ROOT}/crates/wingfoil/examples/showcase/trading_e2e/static"
VENDOR_DIR="${STATIC_DIR}/vendor"

if [ ! -d "${STATIC_DIR}" ]; then
  echo "ERROR: static dir not found at ${STATIC_DIR}" >&2
  exit 1
fi

for tool in node pnpm wasm-pack; do
  if ! command -v "${tool}" >/dev/null 2>&1; then
    echo "ERROR: '${tool}' is required but not installed." >&2
    echo "       node/pnpm: https://pnpm.io/installation" >&2
    echo "       wasm-pack: https://rustwasm.github.io/wasm-pack/installer/" >&2
    exit 1
  fi
done

echo "==> Building @wingfoil/client from js/"
pnpm --dir "${REPO_ROOT}/js" install --frozen-lockfile
pnpm --dir "${REPO_ROOT}/js" run build

CLIENT_DIST="${REPO_ROOT}/js/dist"
if [ ! -f "${CLIENT_DIST}/index.js" ]; then
  echo "ERROR: js build did not produce dist/index.js" >&2
  exit 1
fi
# The wasm payload is copied into dist/ by the package's own `build:ts` step.
# Verify rather than assume: a dist without it serves a client that throws on
# init, which looks like a server fault from the browser.
if [ ! -d "${CLIENT_DIST}/wasm" ]; then
  echo "ERROR: js build did not produce dist/wasm/ — the client cannot init" >&2
  exit 1
fi

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "${WORK_DIR}"' EXIT

fetch_package() {
  # npm pack writes <name>-<version>.tgz into the cwd; both packages here are
  # leaves (uplot has no dependencies, telemetry's only one is the uplot peer
  # we supply explicitly), so there is no dependency graph to resolve.
  local spec="$1" dest="$2"
  echo "==> Fetching ${spec}"
  local tarball
  tarball="$(cd "${WORK_DIR}" && npm pack "${spec}" --silent)"
  mkdir -p "${dest}"
  tar -xzf "${WORK_DIR}/${tarball}" -C "${dest}" --strip-components=1
}

rm -rf "${VENDOR_DIR}"
mkdir -p "${VENDOR_DIR}"

echo "==> Assembling ${VENDOR_DIR}"
mkdir -p "${VENDOR_DIR}/client"
# Only the entry points this page reaches, plus their relative dependencies.
# The package's other entry points (solid/svelte/vue) are framework adapters
# whose bare imports — "solid-js", "svelte/store", "vue" — resolve against
# peer dependencies that this page has no importmap entries for and does not
# ship. They could never load here, so serving them would only leave modules
# on the origin that throw when imported. index.js and tracing.js between
# them pull in ./utils.js and ./wasm/, all by relative specifier.
for module in index tracing utils; do
  cp "${CLIENT_DIST}/${module}.js" "${VENDOR_DIR}/client/"
  # Source maps and typings are optional; copy when tsc emitted them.
  for sidecar in "${module}.js.map" "${module}.d.ts" "${module}.d.ts.map"; do
    [ -f "${CLIENT_DIST}/${sidecar}" ] && cp "${CLIENT_DIST}/${sidecar}" "${VENDOR_DIR}/client/"
  done
done
cp -R "${CLIENT_DIST}/wasm" "${VENDOR_DIR}/client/wasm"

fetch_package "@wingfoil/telemetry@${TELEMETRY_VERSION}" "${WORK_DIR}/telemetry"
mkdir -p "${VENDOR_DIR}/telemetry"
cp -R "${WORK_DIR}/telemetry/dist/." "${VENDOR_DIR}/telemetry/"

fetch_package "uplot@${UPLOT_VERSION}" "${WORK_DIR}/uplot"
mkdir -p "${VENDOR_DIR}/uplot"
cp "${WORK_DIR}/uplot/dist/uPlot.esm.js" "${VENDOR_DIR}/uplot/"
cp "${WORK_DIR}/uplot/dist/uPlot.min.css" "${VENDOR_DIR}/uplot/"

# Every bare specifier the page can reach must have an importmap entry, and
# the set is larger than the imports in app.js: @wingfoil/telemetry's
# chart.js does `import uPlot from "uplot"`. esm.sh used to rewrite that
# transitive specifier to a URL for us, so it never appeared in the importmap
# — serving the raw package makes it the page's problem. Assert the files
# those entries point at exist, so a missing one fails here rather than as a
# blank page in a browser.
for required in \
  "${VENDOR_DIR}/client/index.js" \
  "${VENDOR_DIR}/client/tracing.js" \
  "${VENDOR_DIR}/telemetry/index.js" \
  "${VENDOR_DIR}/uplot/uPlot.esm.js" \
  "${VENDOR_DIR}/uplot/uPlot.min.css"; do
  if [ ! -f "${required}" ]; then
    echo "ERROR: expected vendored file is missing: ${required}" >&2
    exit 1
  fi
done

echo "==> Vendored bundle ready:"
du -sh "${VENDOR_DIR}"/* | sed 's/^/    /'

#!/usr/bin/env bash
# Fetch the LibriTTS-R piper voice (~80MB) into assets/voice/.
#
# The upstream piper voices on huggingface are blocked by some egress proxies,
# so this pulls the same model from the sherpa-onnx GitHub release, which
# packages the identical onnx + config.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
dest="$(cd "$here/.." && pwd)/assets/voice"
url="https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/vits-piper-en_US-libritts_r-medium.tar.bz2"

mkdir -p "$dest"
if [ -f "$dest/en_US-libritts_r-medium.onnx" ]; then
  echo "voice already present in $dest" >&2
  exit 0
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "fetching $url" >&2
curl -sSL -o "$tmp/voice.tar.bz2" "$url"
tar xjf "$tmp/voice.tar.bz2" -C "$tmp"

src="$tmp/vits-piper-en_US-libritts_r-medium"
mv "$src/en_US-libritts_r-medium.onnx" "$src/en_US-libritts_r-medium.onnx.json" "$dest/"
echo "voice installed in $dest" >&2

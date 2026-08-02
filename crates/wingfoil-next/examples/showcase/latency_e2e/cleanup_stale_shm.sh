#!/bin/bash
# Cleanup stale iceoryx2 shared memory files from /dev/shm
#
# This script removes orphaned iceoryx2 service static configs that can persist
# after ungraceful process termination (e.g., SIGKILL). These stale configs cause
# IncompatibleTypes errors on restart.
#
# Safe to run before launching any latency_e2e examples.

set -e

CLEANUP_PATTERN="/dev/shm/iox2_*"

echo "Cleaning stale iceoryx2 shared memory files..."

# Count files before cleanup
BEFORE=$(ls -1 $CLEANUP_PATTERN 2>/dev/null | wc -l || echo 0)

# Remove stale files (ignore errors if none exist)
rm -f $CLEANUP_PATTERN 2>/dev/null || true

AFTER=$(ls -1 $CLEANUP_PATTERN 2>/dev/null | wc -l || echo 0)

if [ "$BEFORE" -gt 0 ]; then
    echo "✓ Cleaned $BEFORE stale iceoryx2 shared memory file(s)"
else
    echo "✓ No stale files found"
fi

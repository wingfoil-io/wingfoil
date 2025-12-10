#!/usr/bin/env python3
"""
Deduplication Example

Demonstrates how to use the `.distinct()` operator to filter out consecutive duplicate values from a stream.
"""

from wingfoil import ticker

period = 0.1

# Create a stream that emits repeating values
# We simulate this by creating a base ticker and mapping it to integer division
# This should produce sequences like 0, 0, 0, 1, 1, 1, 2, 2, 2...
stream = (
    ticker(period)
    .count()
    .map(lambda x: int(x / 3))  # repeat each number 3 times (0,0,0, 1,1,1...)
    .logged("Raw")              # Log the raw repeating stream
    .distinct()                 # Filter out consecutive duplicates
    .logged("Distinct")         # Log the clean stream (0, 1, 2...)
)

print("Running deduplication example...")
stream.run(
    realtime=True,
    cycles=10  # Should suffice to show 0, 1, 2, 3
)

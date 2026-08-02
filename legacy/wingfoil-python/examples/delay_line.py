#!/usr/bin/env python3
"""
Delay Line Example

Demonstrates how to use the `.delay()` operator to introduce temporal spacing in a stream.
"""

from wingfoil import ticker

period = 0.5

# Create a source ticker
source = (
    ticker(period)
    .count()
    .logged("Source")
)

# Create a delayed version of the source
# Delay by 0.25 seconds
delayed = (
    source
    .delay(0.25)
    .logged("Delayed")
)

# Run the delayed stream (which pulls from source)
print("Running delay line example...")
print("Expect 'Source' output followed by 'Delayed' output ~0.25s later.")
delayed.run(
    realtime=True,
    cycles=5
)

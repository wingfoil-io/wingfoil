
This crate contains the python bindings for the wingfoil crate.   These bindings are still 
work in progress / incomplete.

Wingfoil is a blazingly fast, highly scalable stream processing framework designed 
for latency-critical use cases such as electronic trading and real-time AI systems.


## quick start

This python code
```
#!/usr/bin/env python3

from wingfoil import ticker

period = 1.0 # seconds
src = ticker(period).count().logged("hello, world")
src.run()
print(src.peek_value())
```
..produces this output:
```text
[2025-11-02T18:42:18Z INFO  wingfoil] 0.000_092 hello, world 1
[2025-11-02T18:42:19Z INFO  wingfoil] 1.008_038 hello, world 2
[2025-11-02T18:42:20Z INFO  wingfoil] 2.012_219 hello, world 3
[2025-11-02T18:42:21Z INFO  wingfoil] 3.002_859 hello, world 4
```

## Installation

The wingfoil python module will available for installation using pip.

## Building from source

```bash
# install deps

sudo apt-get install python3
sudo apt-get install python3-pip
sudo apt-get install python3-venv

# set up virtual env

python3 -m venv ./.venv

# activate the virutal env

source ./.venv/bin/activate

# under virtual env install these modules

pip install patchelf
pip install maturin

# under virtual env, build the fluvial wheel

maturin develop --release

# You can run example with

python3 ./examples/quickstart.py 

This crate contains the python bindings for the wingfoil crate.

Wingfoil is a blazingly fast, highly scalable 
stream processing framework designed for latency-critical use cases such as electronic trading 
and real-time AI systems.

```
#!/usr/bin/env python3

from wingfoil import ticker

period = 1.0 # secondssrc = ticker(period).count().logged("hello, world")
src.run()
print(src.peek_value())
```

The wingfoil python module will available for installation using pip.

To build from source:

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
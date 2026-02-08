## Building from source

```bash
# install deps

sudo apt-get install python3
sudo apt-get install python3-pip
sudo apt-get install python3-venv

# set up virtual env

python3 -m venv ~/.venv

# activate the virtual env

source ~/.venv/bin/activate

# under virtual env install these modules

pip install patchelf
pip install maturin

# under virtual env, build the wingfoil wheel

maturin develop --release

# run tests

pip install pytest
pytest

# You can run example with

python3 ./examples/quick_start.py
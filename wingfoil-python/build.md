## Building from source

```bash
# install deps

sudo apt-get install python3
sudo apt-get install python3-pip
sudo apt-get install python3-venv

# set up virtual env

python3 -m venv ~/.venv

# activate the virutal env

source ~/.venv/bin/activate

# under virtual env install these modules

pip install patchelf
pip install maturin

# under virtual env, build the fluvial wheel

maturin develop --release

# You can run example with

python3 ./examples/quick_start.py 
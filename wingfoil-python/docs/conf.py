# docs/conf.py

import os
import sys
sys.path.insert(0, os.path.abspath('..'))  # Adds wingfoil-python to Python path

project = 'Wingfoil'
author = 'Jake Mitchell'
release = '0.1.10'

extensions = [
    'sphinx.ext.autodoc',  # Pull docstrings from Python modules
    'sphinx.ext.napoleon', # Support for Google/NumPy style docstrings
    'myst_parser',  # enable Markdown support
]

templates_path = ['_templates']
exclude_patterns = []

html_theme = 'sphinx_rtd_theme'
html_static_path = ['_static']
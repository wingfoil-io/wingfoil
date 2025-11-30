# docs/conf.py

import os
import sys
sys.path.insert(0, os.path.abspath('../python/wingfoil'))  # Adds wingfoil-python to Python path
sys.path.insert(0, os.path.abspath('../python'))

project = 'Wingfoil'
author = 'Jake Mitchell'
release = '0.1.14'

extensions = [
    'sphinx.ext.autodoc',
    'sphinx.ext.napoleon',
    'myst_parser',
]

autodoc_default_options = {
    "members": True,
    "undoc-members": True,
    "show-inheritance": True,
}

autosummary_generate = True
autosummary_generate_overwrite = True

templates_path = ['_templates']
exclude_patterns = []

html_theme = 'sphinx_rtd_theme'
html_static_path = ['_static']

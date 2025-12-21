

import sys, os
sys.path.insert(0, os.path.abspath('..')) 

import wingfoil
sys.modules['wingfoil.wingfoil'] = wingfoil # some workaround for sphinx nonsense!?

copyright = '2025, Jake Mitchell'
author = 'Jake Mitchell'
release = '0.1.16'

# -- General configuration ---------------------------------------------------
extensions = [
    'sphinx.ext.autodoc',     # REQUIRED: To extract docstrings
    'sphinx.ext.autosummary', # RECOMMENDED: To generate API tables
    'sphinx.ext.napoleon',    # RECOMMENDED: For Google/NumPy style docstrings
    'myst_parser',            # If you use Markdown in docs
]

templates_path = ['_templates']
exclude_patterns = ['_build', 'Thumbs.db', '.DS_Store']

# --- AUTODOC/AUTOSUMMARY CONFIGURATION ---
autodoc_default_options = {
    "members": True,
    "undoc-members": True,
    "show-inheritance": True,
}
autosummary_generate = True
add_module_names = False

# -- Options for HTML output -------------------------------------------------
html_theme = 'sphinx_rtd_theme' # Use a standard documentation theme
html_static_path = ['_static']


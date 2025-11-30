

import sys, os
sys.path.insert(0, os.path.abspath('..')) 

# CRITICAL FIX: Alias the top-level module to the nested name Autodoc expects
try:
    import wingfoil
    # If wingfoil imports, assign its contents to the nested path Autodoc is failing on.
    sys.modules['wingfoil.wingfoil'] = wingfoil
    print("SUCCESS: Aliased 'wingfoil' to 'wingfoil.wingfoil' for Sphinx.")
except ImportError:
    # This keeps the build from crashing if the module isn't built yet
    print("WARNING: Could not import 'wingfoil' for aliasing.")
    pass

copyright = '2025, Jake Mitchell'
author = 'Jake Mitchell'
release = '0.1.15'

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

# -- Options for HTML output -------------------------------------------------
html_theme = 'sphinx_rtd_theme' # Use a standard documentation theme
html_static_path = ['_static']


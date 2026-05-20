"""Kāra Jupyter kernel — Python launcher + kernelspec installer.

The actual kernel process is the Rust ``karac-kernel`` binary built
from the sibling Cargo crate (``kernel/`` at the workspace root).
This package is the thin distribution surface: a launcher that finds
the binary and forwards Jupyter's connection file to it, plus an
installer that registers the kernelspec with JupyterLab / ``jupyter
console``.

See ``kernel/python/pyproject.toml`` for the rationale on why this
package has zero runtime dependencies beyond the standard library.
"""

__version__ = "0.1.0"

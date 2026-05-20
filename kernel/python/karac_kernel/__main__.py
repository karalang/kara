"""``python -m karac_kernel`` entry point.

Dispatches between two roles:

- ``python -m karac_kernel install [args]`` → run the kernelspec
  installer (see ``install.py``).
- ``python -m karac_kernel [other args]`` → forward args to the
  Rust kernel binary (see ``launcher.py``). This is the form
  Jupyter invokes when starting the kernel, with
  ``--connection-file=<path>`` as the only argument.
"""

from __future__ import annotations

import sys


def main() -> int:
    argv = sys.argv[1:]
    if argv and argv[0] == "install":
        from karac_kernel import install

        return install.main(argv[1:])

    from karac_kernel import launcher

    return launcher.main(argv)


if __name__ == "__main__":
    sys.exit(main())

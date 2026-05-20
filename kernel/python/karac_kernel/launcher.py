"""Locate the Kāra kernel binary and exec it with Jupyter's argv.

Lookup order (first hit wins):

1. ``KARAC_KERNEL_BIN`` env var — pointing directly at the binary.
   Used in development to point at ``target/debug/karac-kernel``
   without ``cargo install``-ing.
2. ``shutil.which("karac-kernel")`` — standard PATH lookup. The
   typical install path is ``~/.cargo/bin`` (cargo install) or
   ``/usr/local/bin`` (system package).
3. ``KARAC_KERNEL_FALLBACK_PATHS`` — colon-separated list of
   directories to scan as a last resort. Documented for advanced
   setups (multi-version installs, distro-managed Cargo bin dirs);
   empty by default.

If none of those resolve, exit with a clear error pointing the user
at the install instructions. The Rust binary itself was built with
``cargo build -p karac-kernel --features real-zmq`` — slice 1-5 land
the binary, slice 6 (this code) hands Jupyter the front door.
"""

from __future__ import annotations

import os
import shutil
import sys
from pathlib import Path

BINARY_NAME = "karac-kernel"
ENV_BIN = "KARAC_KERNEL_BIN"
ENV_FALLBACK = "KARAC_KERNEL_FALLBACK_PATHS"


def find_binary() -> Path:
    """Return the absolute path of the Kāra kernel binary.

    Raises ``FileNotFoundError`` with a help-text message when no
    candidate resolves. Callers should let the exception propagate
    so Jupyter surfaces the message in its kernel-failure UI.
    """
    explicit = os.environ.get(ENV_BIN)
    if explicit:
        path = Path(explicit).expanduser()
        if path.is_file() and os.access(path, os.X_OK):
            return path.resolve()
        raise FileNotFoundError(
            f"${ENV_BIN}={explicit!r} is set but does not point at an "
            f"executable file. Unset the variable or fix the path."
        )

    found = shutil.which(BINARY_NAME)
    if found is not None:
        return Path(found).resolve()

    fallback = os.environ.get(ENV_FALLBACK, "")
    for d in fallback.split(os.pathsep):
        if not d:
            continue
        candidate = Path(d).expanduser() / BINARY_NAME
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return candidate.resolve()

    raise FileNotFoundError(
        f"Could not locate the {BINARY_NAME!r} binary. Install it with:\n"
        f"    cargo install karac-kernel --features real-zmq\n"
        f"or set ${ENV_BIN} to its absolute path."
    )


def exec_kernel(argv: list[str]) -> int:
    """Exec the Rust kernel binary with ``argv`` and never return.

    ``argv`` should be the full argv Jupyter passed to us *minus*
    the program name (i.e. typically just
    ``["--connection-file=<path>"]``). On platforms that support
    ``os.execvp`` (POSIX) we replace the current process so signals
    + exit codes flow through naturally; on Windows we ``subprocess
    .run`` and propagate the return code.
    """
    binary = find_binary()
    full_argv = [str(binary), *argv]
    if os.name == "posix":
        os.execvp(full_argv[0], full_argv)
        # Unreachable — execvp replaces the process image.
        return 0  # pragma: no cover
    else:
        # Windows: no execvp semantics that propagate Ctrl+C
        # correctly to the child. subprocess.run + sys.exit is the
        # idiomatic fallback.
        import subprocess

        result = subprocess.run(full_argv, check=False)
        return result.returncode


def main(argv: list[str] | None = None) -> int:
    """Entry point invoked by ``python -m karac_kernel <argv>`` when
    Jupyter starts the kernel. ``argv`` defaults to
    ``sys.argv[1:]``. Returns a process exit code (only on Windows
    — POSIX exec replaces this process).
    """
    if argv is None:
        argv = sys.argv[1:]
    try:
        return exec_kernel(argv)
    except FileNotFoundError as e:
        print(str(e), file=sys.stderr)
        return 127  # POSIX convention: "command not found".

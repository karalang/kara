"""Register the Kāra kernelspec with Jupyter.

Writes a ``kernel.json`` file into one of Jupyter's data
directories, in the standard layout:

    <data-dir>/kernels/kara/
        kernel.json

``kernel.json`` points ``argv`` at ``python -m karac_kernel`` so
JupyterLab / ``jupyter console`` route through this package's
launcher (see ``launcher.py``), which in turn execs the Rust
``karac-kernel`` binary.

Jupyter data directories follow the platform conventions Jupyter
itself documents:

- User scope:   ``$XDG_DATA_HOME/jupyter`` (Linux), ``~/Library/Jupyter``
                (macOS), ``%APPDATA%/jupyter`` (Windows).
- System scope: ``/usr/local/share/jupyter`` (POSIX), ``%PROGRAMDATA%
                /jupyter`` (Windows).
- Custom:       any directory passed via ``--prefix``.

The lookup is delegated to ``jupyter --paths --json`` when available
(authoritative — Jupyter knows its own conda / venv / global state)
with a hardcoded platform fallback that matches the upstream
``jupyter_core.paths`` defaults verbatim. This keeps the installer
dependency-free (no ``jupyter_core`` import) while still respecting
non-default install layouts.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

KERNEL_NAME = "kara"
DISPLAY_NAME = "Kāra"
LANGUAGE_NAME = "kara"
PYGMENTS_LEXER = "rust"


def kernel_json_payload() -> dict:
    """Build the kernelspec ``kernel.json`` body.

    ``argv`` uses ``sys.executable`` so the user's kernel runs under
    the same Python interpreter the install command ran under —
    avoids surprises when ``karac-kernel`` is installed into a venv
    and the system Python tries to import the package and fails.
    The ``{connection_file}`` template is substituted by Jupyter at
    kernel launch time.

    ``interrupt_mode: "message"`` tells Jupyter to send an
    ``interrupt_request`` on the control channel rather than
    SIGINT'ing the process — matches what slice 5's
    ``handle_interrupt_request`` expects.
    """
    return {
        "argv": [
            sys.executable,
            "-m",
            "karac_kernel",
            "--connection-file={connection_file}",
        ],
        "display_name": DISPLAY_NAME,
        "language": LANGUAGE_NAME,
        "interrupt_mode": "message",
        "metadata": {
            "kernel": "karac-kernel",
            "language_info": {
                "name": LANGUAGE_NAME,
                "file_extension": ".kara",
                "mimetype": "text/x-kara",
                "pygments_lexer": PYGMENTS_LEXER,
            },
        },
    }


@dataclass
class InstallTarget:
    """Resolved target directory for the kernelspec write."""

    data_dir: Path
    """The Jupyter data dir (e.g. ``~/Library/Jupyter``); contains
    the ``kernels/`` subdirectory we write into."""

    kernel_dir: Path
    """The actual ``<data-dir>/kernels/<KERNEL_NAME>`` directory we
    create + write ``kernel.json`` to."""

    scope: str
    """Human-readable scope name (``"user"``, ``"system"``,
    ``"prefix"``) for the install-completed message."""


def resolve_install_target(
    scope: str = "user",
    prefix: str | None = None,
) -> InstallTarget:
    """Pick the data dir + kernel dir for the requested scope."""
    if prefix is not None:
        data_dir = Path(prefix).expanduser() / "share" / "jupyter"
        return InstallTarget(
            data_dir=data_dir,
            kernel_dir=data_dir / "kernels" / KERNEL_NAME,
            scope="prefix",
        )
    data_dir = jupyter_data_dir(scope=scope)
    return InstallTarget(
        data_dir=data_dir,
        kernel_dir=data_dir / "kernels" / KERNEL_NAME,
        scope=scope,
    )


def jupyter_data_dir(scope: str = "user") -> Path:
    """Return Jupyter's user / system data directory.

    Delegates to ``jupyter --paths --json`` when the CLI is on PATH
    so we honor venv / conda / custom-prefix installs; falls back to
    the hardcoded platform defaults otherwise. The fallback values
    match ``jupyter_core.paths.jupyter_data_dir()`` /
    ``jupyter_core.paths.jupyter_path()`` so the kernelspec lands in
    the same directory either way.
    """
    if scope not in ("user", "system"):
        raise ValueError(f"unknown scope {scope!r}; expected 'user' or 'system'")

    jupyter = shutil.which("jupyter")
    if jupyter is not None:
        try:
            result = subprocess.run(
                [jupyter, "--paths", "--json"],
                check=True,
                capture_output=True,
                text=True,
                timeout=10,
            )
            payload = json.loads(result.stdout)
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired, json.JSONDecodeError):
            payload = None
        if payload is not None:
            data_paths = payload.get("data") or []
            if scope == "user" and data_paths:
                # `jupyter --paths --json` lists user dir first.
                return Path(data_paths[0])
            if scope == "system" and len(data_paths) > 1:
                return Path(data_paths[1])

    return _fallback_data_dir(scope)


def _fallback_data_dir(scope: str) -> Path:
    """Hardcoded data-dir paths matching upstream ``jupyter_core``.

    Used when ``jupyter --paths`` isn't on PATH (rare; only
    happens if `jupyter` isn't installed). The kernel can't run
    against a Jupyter that doesn't exist, but ``jupyter`` is in a
    different package than the kernel binary so the user may
    install them out of order — falling back keeps the install
    side honest.
    """
    if scope == "system":
        if sys.platform == "win32":
            programdata = os.environ.get("PROGRAMDATA")
            if programdata:
                return Path(programdata) / "jupyter"
            return Path("C:/ProgramData/jupyter")
        return Path("/usr/local/share/jupyter")

    # scope == "user"
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Jupyter"
    if sys.platform == "win32":
        appdata = os.environ.get("APPDATA")
        if appdata:
            return Path(appdata) / "jupyter"
        return Path.home() / "AppData" / "Roaming" / "jupyter"
    # Linux / other POSIX: XDG.
    xdg = os.environ.get("XDG_DATA_HOME")
    if xdg:
        return Path(xdg) / "jupyter"
    return Path.home() / ".local" / "share" / "jupyter"


def install(target: InstallTarget) -> Path:
    """Write the kernelspec into ``target.kernel_dir``.

    Returns the path of the written ``kernel.json``. Idempotent —
    re-running overwrites the file, matching ``jupyter kernelspec
    install``'s replace-by-default behavior.
    """
    target.kernel_dir.mkdir(parents=True, exist_ok=True)
    kernel_json = target.kernel_dir / "kernel.json"
    payload = kernel_json_payload()
    kernel_json.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    return kernel_json


def main(argv: list[str] | None = None) -> int:
    """``python -m karac_kernel install`` entry point.

    Also registered as a console script (``karac-kernel-install``)
    in ``pyproject.toml`` so users who don't want to type
    ``python -m`` can use the shorter form.
    """
    parser = argparse.ArgumentParser(
        prog="karac-kernel-install",
        description="Register the Kāra Jupyter kernelspec with JupyterLab.",
    )
    scope = parser.add_mutually_exclusive_group()
    scope.add_argument(
        "--user",
        action="store_true",
        help="Install for the current user (default).",
    )
    scope.add_argument(
        "--system",
        action="store_true",
        help="Install system-wide (may require root / Administrator).",
    )
    scope.add_argument(
        "--prefix",
        metavar="PATH",
        help="Install under PATH/share/jupyter (venv / conda layout).",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print the target path + kernelspec payload without writing.",
    )
    args = parser.parse_args(argv)

    if args.system:
        scope_name = "system"
    else:
        scope_name = "user"

    target = resolve_install_target(scope=scope_name, prefix=args.prefix)

    if args.dry_run:
        print(f"Would write kernelspec to: {target.kernel_dir / 'kernel.json'}")
        print(f"Scope: {target.scope}")
        print("Payload:")
        print(json.dumps(kernel_json_payload(), indent=2))
        return 0

    written = install(target)
    print(f"Installed Kāra kernelspec ({target.scope} scope) at: {written}")
    print("Launch JupyterLab or `jupyter console --kernel=kara` to use it.")
    return 0

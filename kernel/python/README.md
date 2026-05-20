# karac-kernel (Python distribution)

Thin Python launcher + kernelspec installer for the Kāra Jupyter
kernel. The actual kernel process is the Rust `karac-kernel` binary
from the sibling Cargo crate (`kernel/` at the workspace root); this
package only registers the kernelspec with Jupyter and forwards
invocations to the binary.

## Install

```bash
# 1. Build + install the Rust kernel binary (one-time):
cargo install karac-kernel --features real-zmq

# 2. Register the kernelspec with Jupyter:
pip install karac-kernel
python -m karac_kernel install --user
```

`--user` (default) writes to your user data dir
(`~/Library/Jupyter/kernels/kara/` on macOS,
`$XDG_DATA_HOME/jupyter/kernels/kara/` on Linux,
`%APPDATA%/jupyter/kernels/kara/` on Windows).
`--system` installs to the system-wide path (may need root).
`--prefix PATH` installs under a venv / conda prefix.

Use `--dry-run` to preview the target path + payload without
writing.

## Launching

After installing the kernelspec, JupyterLab and `jupyter console
--kernel=kara` will offer "Kāra" in the kernel picker. Internally,
Jupyter invokes:

```
python -m karac_kernel --connection-file=<connection-file>
```

which calls `launcher.find_binary()` to locate `karac-kernel` and
execs it.

## Binary discovery

`launcher.find_binary()` looks in (first hit wins):

1. `KARAC_KERNEL_BIN` env var — absolute path to the binary.
   Useful in development to point at `target/debug/karac-kernel`
   without `cargo install`-ing.
2. `shutil.which("karac-kernel")` — PATH lookup.
3. `KARAC_KERNEL_FALLBACK_PATHS` — colon-separated directories.

If nothing matches, the launcher prints the install command and
exits 127 (POSIX "command not found").

## Tests

```bash
cd kernel/python
python -m unittest discover tests
```

Stdlib `unittest` only — no extra deps. The tests cover binary
discovery, payload schema pinning, and idempotent install into a
temp prefix.

## v1.1.x follow-up

The Python shim is the v1.1-launch surface, not the long-term home.
Once Kāra's stable stdlib ships with `std.fs`, `std.env`, and
`std.process`, the installer gets rewritten in Kāra itself
(`install-kernel.kara` shipped alongside the Rust binary) and this
Python package retires. See the kernel-MVP tracker entry for the
follow-up commitment.

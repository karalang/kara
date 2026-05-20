"""Unit tests for ``karac_kernel.launcher`` — binary lookup logic.

These tests don't run the Rust binary; they verify the discovery
path (env override > PATH > fallback dirs > error) using temp
directories so the test surface stays self-contained on a fresh
checkout. Run with ``python -m unittest`` from
``kernel/python/``.
"""

from __future__ import annotations

import os
import stat
import sys
import tempfile
import unittest
from pathlib import Path

# Make `karac_kernel` importable without installing the package.
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from karac_kernel import launcher  # noqa: E402


class FindBinaryTests(unittest.TestCase):
    """Lookup-order coverage for `find_binary`."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        # Stash the real env so each test gets a clean slate.
        self._saved_env = {
            k: os.environ.get(k)
            for k in (launcher.ENV_BIN, launcher.ENV_FALLBACK, "PATH")
        }
        # Empty PATH so `shutil.which` can't pick up a system binary
        # by accident.
        os.environ["PATH"] = ""
        os.environ.pop(launcher.ENV_BIN, None)
        os.environ.pop(launcher.ENV_FALLBACK, None)

    def tearDown(self) -> None:
        for k, v in self._saved_env.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v

    def _make_executable(self, dir_path: Path, name: str) -> Path:
        path = dir_path / name
        path.write_text("#!/bin/sh\nexit 0\n")
        path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
        return path

    def test_env_override_takes_precedence(self) -> None:
        tmp = Path(self.tmp.name)
        explicit = self._make_executable(tmp, "any-name-works")
        os.environ[launcher.ENV_BIN] = str(explicit)
        # Also put a different "karac-kernel" on PATH to confirm the
        # env var wins.
        path_dir = tmp / "path"
        path_dir.mkdir()
        self._make_executable(path_dir, launcher.BINARY_NAME)
        os.environ["PATH"] = str(path_dir)

        found = launcher.find_binary()
        self.assertEqual(found, explicit.resolve())

    def test_env_override_with_missing_file_errors(self) -> None:
        os.environ[launcher.ENV_BIN] = str(Path(self.tmp.name) / "does-not-exist")
        with self.assertRaisesRegex(FileNotFoundError, "does not point at"):
            launcher.find_binary()

    def test_path_lookup(self) -> None:
        tmp = Path(self.tmp.name)
        path_dir = tmp / "path"
        path_dir.mkdir()
        binary = self._make_executable(path_dir, launcher.BINARY_NAME)
        os.environ["PATH"] = str(path_dir)

        found = launcher.find_binary()
        self.assertEqual(found, binary.resolve())

    def test_fallback_paths_are_scanned_last(self) -> None:
        tmp = Path(self.tmp.name)
        fallback_dir = tmp / "fallback"
        fallback_dir.mkdir()
        binary = self._make_executable(fallback_dir, launcher.BINARY_NAME)
        os.environ[launcher.ENV_FALLBACK] = str(fallback_dir)

        found = launcher.find_binary()
        self.assertEqual(found, binary.resolve())

    def test_unfound_binary_raises_helpful_error(self) -> None:
        with self.assertRaises(FileNotFoundError) as ctx:
            launcher.find_binary()
        self.assertIn("cargo install karac-kernel", str(ctx.exception))
        self.assertIn(launcher.ENV_BIN, str(ctx.exception))


class MainEntryPointTests(unittest.TestCase):
    """`launcher.main` exit codes on the not-found path."""

    def setUp(self) -> None:
        self._saved_env = {
            k: os.environ.get(k) for k in (launcher.ENV_BIN, "PATH")
        }
        os.environ["PATH"] = ""
        os.environ.pop(launcher.ENV_BIN, None)

    def tearDown(self) -> None:
        for k, v in self._saved_env.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v

    def test_main_returns_127_when_binary_missing(self) -> None:
        # POSIX convention: 127 = command not found. The launcher
        # surfaces this so Jupyter's kernel-failure UI shows the
        # correct exit-status semantics.
        rc = launcher.main(["--connection-file=/tmp/nonexistent.json"])
        self.assertEqual(rc, 127)


if __name__ == "__main__":
    unittest.main()

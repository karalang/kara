"""Unit tests for ``karac_kernel.install`` — kernelspec writer.

Tests use temp directories + ``--prefix`` to redirect the write
target so they never touch the user's real Jupyter data dir. Run
with ``python -m unittest`` from ``kernel/python/``.
"""

from __future__ import annotations

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from karac_kernel import install  # noqa: E402


class PayloadShapeTests(unittest.TestCase):
    """Schema pinning for ``kernel.json`` body."""

    def test_payload_has_required_keys(self) -> None:
        payload = install.kernel_json_payload()
        # Required by the Jupyter kernelspec schema:
        for key in ("argv", "display_name", "language"):
            self.assertIn(key, payload, f"missing {key}")

    def test_argv_uses_current_python(self) -> None:
        payload = install.kernel_json_payload()
        self.assertEqual(payload["argv"][0], sys.executable)
        self.assertEqual(payload["argv"][1:3], ["-m", "karac_kernel"])

    def test_argv_contains_connection_file_template(self) -> None:
        payload = install.kernel_json_payload()
        joined = " ".join(payload["argv"])
        self.assertIn("{connection_file}", joined)

    def test_interrupt_mode_is_message(self) -> None:
        # Critical contract — slice 5's interrupt_request handler
        # only fires when Jupyter sends a control-channel message
        # rather than SIGINT. If a future edit drops this field,
        # interrupt stops working on JupyterLab.
        payload = install.kernel_json_payload()
        self.assertEqual(payload["interrupt_mode"], "message")

    def test_language_metadata_matches_rust_kernel(self) -> None:
        payload = install.kernel_json_payload()
        self.assertEqual(payload["language"], "kara")
        lang_info = payload["metadata"]["language_info"]
        self.assertEqual(lang_info["name"], "kara")
        self.assertEqual(lang_info["file_extension"], ".kara")
        self.assertEqual(lang_info["mimetype"], "text/x-kara")


class InstallTargetTests(unittest.TestCase):
    """Path-resolution coverage for ``resolve_install_target``."""

    def test_prefix_overrides_scope(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            target = install.resolve_install_target(prefix=tmp)
            self.assertEqual(target.scope, "prefix")
            self.assertTrue(str(target.kernel_dir).startswith(tmp))
            self.assertTrue(
                str(target.kernel_dir).endswith(
                    os.path.join("share", "jupyter", "kernels", "kara")
                )
            )

    def test_user_scope_defaults_under_home(self) -> None:
        # The exact path is platform-specific; we just confirm it's
        # rooted under the user's home directory so a typo can't
        # accidentally point at `/usr/...`.
        target = install.resolve_install_target(scope="user")
        # The data dir on macOS/Linux always lives under HOME unless
        # the user explicitly redirected via XDG; in CI both cases
        # should resolve under HOME.
        home = str(Path.home())
        xdg = os.environ.get("XDG_DATA_HOME")
        ok = str(target.data_dir).startswith(home) or (
            xdg and str(target.data_dir).startswith(xdg)
        )
        self.assertTrue(ok, f"user data dir {target.data_dir} not under home {home}")


class InstallWriteTests(unittest.TestCase):
    """End-to-end install into a temp prefix."""

    def test_install_writes_kernel_json_and_returns_path(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            target = install.resolve_install_target(prefix=tmp)
            written = install.install(target)

            self.assertTrue(written.exists())
            self.assertEqual(written.name, "kernel.json")
            self.assertEqual(written.parent, target.kernel_dir)
            self.assertEqual(written.parent.name, "kara")

            with written.open() as f:
                payload = json.load(f)
            self.assertEqual(payload["language"], "kara")
            self.assertEqual(payload["display_name"], "Kāra")

    def test_install_is_idempotent(self) -> None:
        # Re-running the installer must overwrite cleanly (matches
        # `jupyter kernelspec install --replace` default).
        with tempfile.TemporaryDirectory() as tmp:
            target = install.resolve_install_target(prefix=tmp)
            first = install.install(target)
            second = install.install(target)
            self.assertEqual(first, second)
            self.assertTrue(second.exists())


class MainEntryPointTests(unittest.TestCase):
    """`install.main` CLI flag coverage."""

    def test_dry_run_does_not_write(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            rc = install.main(["--prefix", tmp, "--dry-run"])
            self.assertEqual(rc, 0)
            target_path = Path(tmp) / "share" / "jupyter" / "kernels" / "kara"
            self.assertFalse(target_path.exists())

    def test_prefix_install_writes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            rc = install.main(["--prefix", tmp])
            self.assertEqual(rc, 0)
            expected = Path(tmp) / "share" / "jupyter" / "kernels" / "kara" / "kernel.json"
            self.assertTrue(expected.exists())

    def test_conflicting_scope_flags_rejected(self) -> None:
        # argparse exits with code 2 on a mutually-exclusive-group
        # violation; SystemExit carries the code.
        with self.assertRaises(SystemExit) as ctx:
            install.main(["--user", "--system"])
        self.assertEqual(ctx.exception.code, 2)


class FallbackDirShapeTests(unittest.TestCase):
    """`_fallback_data_dir` returns plausible paths per platform."""

    def test_user_scope_returns_an_absolute_path(self) -> None:
        path = install._fallback_data_dir("user")
        self.assertTrue(path.is_absolute())

    def test_system_scope_returns_an_absolute_path(self) -> None:
        path = install._fallback_data_dir("system")
        self.assertTrue(path.is_absolute())

    def test_unknown_scope_via_resolve_install_target_rejected(self) -> None:
        with self.assertRaises(ValueError):
            install.jupyter_data_dir(scope="bogus")


if __name__ == "__main__":
    unittest.main()

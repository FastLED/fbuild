#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Unit tests for forbidden_commands.py.

FastLED/fbuild#694 — pin the false-positive-resistance promises from
the issue's acceptance criterion ("benign mentions in commit bodies /
docs are not flagged").

Run directly: `python ci/hooks/test_forbidden_commands.py`.
"""

import importlib.util
import os
import sys
import unittest
from pathlib import Path


HOOK_PATH = Path(__file__).resolve().parent / "forbidden_commands.py"
spec = importlib.util.spec_from_file_location("forbidden_commands", HOOK_PATH)
assert spec is not None and spec.loader is not None
forbidden = importlib.util.module_from_spec(spec)
spec.loader.exec_module(forbidden)


class FindForbidden(unittest.TestCase):
    def test_bare_pyocd_invocation_is_blocked(self) -> None:
        hit = forbidden.find_forbidden("pyocd flash --target lpc845 firmware.elf")
        self.assertIsNotNone(hit)
        self.assertEqual(hit[0], "pyocd")

    def test_bare_esptool_blocked(self) -> None:
        hit = forbidden.find_forbidden(
            "esptool --chip esp32s3 --port COM12 write_flash 0x0 firmware.bin"
        )
        self.assertIsNotNone(hit)
        self.assertEqual(hit[0], "esptool")

    def test_esptool_py_dotted_form_blocked(self) -> None:
        hit = forbidden.find_forbidden("esptool.py --chip esp32 write_flash 0x0 fw.bin")
        self.assertIsNotNone(hit)
        self.assertEqual(hit[0], "esptool.py")

    def test_dfu_util_blocked(self) -> None:
        hit = forbidden.find_forbidden("dfu-util -a 0 -D firmware.dfu")
        self.assertIsNotNone(hit)
        self.assertEqual(hit[0], "dfu-util")

    def test_picotool_blocked(self) -> None:
        hit = forbidden.find_forbidden("picotool load firmware.uf2 -f")
        self.assertIsNotNone(hit)
        self.assertEqual(hit[0], "picotool")

    def test_probe_rs_cli_blocked(self) -> None:
        hit = forbidden.find_forbidden("probe-rs run --chip ESP32S3 firmware.elf")
        self.assertIsNotNone(hit)
        self.assertEqual(hit[0], "probe-rs")

    def test_env_prefix_does_not_bypass(self) -> None:
        # `env VAR=val pyocd …` is still invoking pyocd.
        hit = forbidden.find_forbidden("env RUST_LOG=debug pyocd reset --target lpc845")
        self.assertIsNotNone(hit)
        self.assertEqual(hit[0], "pyocd")

    def test_sudo_prefix_does_not_bypass(self) -> None:
        hit = forbidden.find_forbidden("sudo dfu-util -a 0 -D firmware.dfu")
        self.assertIsNotNone(hit)
        self.assertEqual(hit[0], "dfu-util")

    def test_fbuild_deploy_is_allowed(self) -> None:
        self.assertIsNone(
            forbidden.find_forbidden("fbuild deploy -e lpc845brk")
        )

    def test_unrelated_command_is_allowed(self) -> None:
        self.assertIsNone(forbidden.find_forbidden("ls -la"))
        self.assertIsNone(forbidden.find_forbidden("git status"))
        self.assertIsNone(forbidden.find_forbidden("cargo build"))


class BenignMentionFiltering(unittest.TestCase):
    """The hook MUST NOT block commit bodies, doc grep, echo'd help text.

    Lesson from FastLED's first revision of the hook (see #3339 review
    thread): it over-fired on commit messages. Pin the false-positives
    so a future tightening doesn't regress.
    """

    def test_git_commit_body_mentioning_pyocd_is_allowed(self) -> None:
        self.assertIsNone(
            forbidden.find_forbidden(
                'git commit -m "fix(lpc): avoid pyocd race after reset"'
            )
        )

    def test_grep_for_string_pyocd_is_allowed(self) -> None:
        self.assertIsNone(
            forbidden.find_forbidden("grep -r 'pyocd' docs/")
        )

    def test_echo_mentioning_esptool_is_allowed(self) -> None:
        self.assertIsNone(
            forbidden.find_forbidden('echo "see esptool docs for chip detection"')
        )

    def test_cat_through_grep_for_dfu_util_is_allowed(self) -> None:
        self.assertIsNone(
            forbidden.find_forbidden("cat README.md | grep dfu-util")
        )

    def test_quoted_picotool_mention_in_pr_body_is_allowed(self) -> None:
        self.assertIsNone(
            forbidden.find_forbidden(
                "gh pr create --title 'rp2040 fix' --body 'avoid picotool deadlock'"
            )
        )

    def test_double_quoted_probe_rs_mention_is_allowed(self) -> None:
        self.assertIsNone(
            forbidden.find_forbidden(
                'echo "probe-rs is the alternative for non-CMSIS-DAP probes"'
            )
        )


class OverrideEnvVar(unittest.TestCase):
    """`FL_AGENT_ALLOW_ALL_CMDS=1` must bypass the hook entirely.

    This is the escape hatch the issue's acceptance criterion calls
    out by name — needs to match FastLED's exactly for muscle memory.
    """

    def test_override_constant_matches_fastled(self) -> None:
        self.assertEqual(forbidden.OVERRIDE_ENV, "FL_AGENT_ALLOW_ALL_CMDS")

    def test_main_returns_0_with_override_set(self) -> None:
        # Simulate the harness handing the hook a forbidden command
        # via stdin. With the override env var set, the hook must
        # return exit 0 (allow).
        import io

        old_env = os.environ.get(forbidden.OVERRIDE_ENV)
        old_stdin = sys.stdin
        try:
            os.environ[forbidden.OVERRIDE_ENV] = "1"
            sys.stdin = io.StringIO(
                '{"tool_input": {"command": "pyocd flash firmware.elf"}}'
            )
            rc = forbidden.main()
            self.assertEqual(rc, 0)
        finally:
            sys.stdin = old_stdin
            if old_env is None:
                os.environ.pop(forbidden.OVERRIDE_ENV, None)
            else:
                os.environ[forbidden.OVERRIDE_ENV] = old_env


if __name__ == "__main__":
    unittest.main(verbosity=2)

"""The local Dylint lane runs on the host through soldr; lint never runs under bosn."""

import tomllib
import unittest
from pathlib import Path
from unittest import mock

from ci import local_dylint_gate
from ci.local_dylint_gate import VALIDATORS, host_commands, lint_command, lint_libraries

ROOT = Path(__file__).resolve().parent.parent
DYLINT_WORKFLOW = ROOT / ".github" / "workflows" / "dylint.yml"


def gate_config() -> dict:
    return tomllib.loads((ROOT / "local-gate.toml").read_text(encoding="utf-8"))["gate"]


class DylintLaneShapeTests(unittest.TestCase):
    def test_lint_lanes_never_use_bosn(self) -> None:
        lanes = gate_config()["lanes"]
        for lane in ("dylint", "lint"):
            with self.subTest(lane=lane):
                self.assertNotIn("bosn", lanes[lane]["tools"])
                self.assertIn("soldr", lanes[lane]["tools"])

    def test_no_replay_job_or_selection_maps_to_the_dylint_lane(self) -> None:
        """A replay binding is an act2/bosn run; the host lane produces none."""
        replay = gate_config()["replay"]
        for job in replay["jobs"]:
            self.assertNotIn("dylint", job["lanes"], job["source-job"])
        for selection in replay["selections"]:
            self.assertNotEqual(selection["lane"], "dylint")
            self.assertNotEqual(selection["workflow"], "dylint.yml")

    def test_workspace_lint_is_soldr_lint_for_every_target(self) -> None:
        self.assertEqual(lint_command(), ["soldr", "lint", "rust", "--check", "--cross-dylint"])
        self.assertEqual(host_commands()[-1], (lint_command(), ROOT))

    def test_every_lint_library_is_formatted_and_tested(self) -> None:
        libraries = lint_libraries()
        self.assertGreater(len(libraries), 0)
        commands = host_commands()
        for library in libraries:
            with self.subTest(library=library.name):
                self.assertIn((["soldr", "cargo", "fmt", "--all", "--", "--check"], library), commands)
                self.assertIn((["soldr", "cargo", "test"], library), commands)

    def test_every_command_goes_through_soldr_or_uv(self) -> None:
        for argv, _cwd in host_commands():
            self.assertIn(argv[0], {"soldr", "uv"}, argv)

    def test_validators_match_the_hosted_dylint_workflow(self) -> None:
        workflow = DYLINT_WORKFLOW.read_text(encoding="utf-8")
        for validator in VALIDATORS:
            with self.subTest(validator=validator[0]):
                self.assertIn(validator[0], workflow)
                self.assertTrue((ROOT / validator[0]).is_file())

    def test_library_commands_drop_the_stable_toolchain_override(self) -> None:
        with mock.patch.dict(local_dylint_gate.os.environ, {"RUSTUP_TOOLCHAIN": "1.95.0"}):
            env = local_dylint_gate.lane_env()
        self.assertNotIn("RUSTUP_TOOLCHAIN", env)

    def test_ui_tests_get_a_lane_owned_prebuilt_driver_root(self) -> None:
        """Without DYLINT_DRIVER_PATH, dylint_testing builds the driver from git."""
        env = local_dylint_gate.lane_env()
        self.assertEqual(env["DYLINT_DRIVER_PATH"], str(ROOT / "target" / "dylint-tests" / "drivers"))

    def test_prepare_tool_bin_dirs_are_parsed(self) -> None:
        output = (
            "soldr: dylint prepare: resolving cargo-dylint + dylint-link prebuilts\n"
            "soldr: dylint prepare: tool bin dir /h/.soldr/bin/cargo-dylint-6.2.0\n"
            "soldr: dylint prepare: tool bin dir /h/.soldr/bin/dylint-link-6.2.0\n"
        )
        self.assertEqual(
            local_dylint_gate.tool_bin_dirs(output),
            ["/h/.soldr/bin/cargo-dylint-6.2.0", "/h/.soldr/bin/dylint-link-6.2.0"],
        )

    def test_non_linux_host_is_a_hard_error(self) -> None:
        with mock.patch.object(local_dylint_gate.platform, "system", return_value="Darwin"):
            with mock.patch.object(local_dylint_gate.subprocess, "run") as run:
                with self.assertRaises(SystemExit):
                    local_dylint_gate.run_dylint()
        run.assert_not_called()

    def test_first_failure_stops_the_lane(self) -> None:
        failed = mock.Mock(returncode=3)
        with mock.patch.object(local_dylint_gate.platform, "system", return_value="Linux"):
            with mock.patch.object(local_dylint_gate.subprocess, "run", return_value=failed) as run:
                with self.assertRaises(SystemExit) as raised:
                    local_dylint_gate.run_dylint()
        self.assertEqual(raised.exception.code, 3)
        self.assertEqual(run.call_count, 1)


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""Unit tests for the shell command tool guard hook."""

import unittest

from tool_guard import check_command, extract_command


class ToolGuardTests(unittest.TestCase):
    def test_blocks_bare_rust_tool(self):
        result = check_command("cargo test")
        self.assertIsNotNone(result)
        self.assertEqual(result[0], "cargo")

    def test_blocks_uv_run_rust_tool_shim(self):
        commands = (
            "uv run cargo test",
            "uv run -- cargo test",
            "uv run --offline cargo build",
            "uv run --with foo cargo test",
            "uv run --with=foo cargo test",
            "uv run --isolated cargo build",
            "uv run --project . cargo check",
            "uv run -q cargo test",
            "uv run -- --offline cargo test",
        )
        for command in commands:
            with self.subTest(command=command):
                result = check_command(command)
                self.assertIsNotNone(result)
                self.assertEqual(result[0], "cargo")

    def test_allows_soldr_wrapped_rust_tool(self):
        self.assertIsNone(check_command("soldr cargo test"))
        self.assertIsNone(check_command("soldr --no-cache cargo build"))
        self.assertIsNone(check_command("soldr rustc --version"))
        self.assertIsNone(check_command("soldr rustfmt --check src/lib.rs"))
        self.assertIsNone(check_command("uv run soldr cargo test"))
        self.assertIsNone(check_command("uv run soldr rustfmt --check src/lib.rs"))

    def test_blocks_bare_python(self):
        result = check_command("python ci/script.py")
        self.assertIsNotNone(result)
        self.assertEqual(result[0], "python")

    def test_extracts_powershell_command_field(self):
        command = extract_command({
            "tool_name": "PowerShell",
            "tool_input": {"command": "cargo test"},
        })
        self.assertEqual(command, "cargo test")

    def test_extracts_shell_script_field(self):
        command = extract_command({
            "tool_name": "Shell",
            "tool_input": {"script": "cargo test"},
        })
        self.assertEqual(command, "cargo test")


if __name__ == "__main__":
    unittest.main()

from __future__ import annotations

import unittest

from ci import check_rust_toolchain_pins


class RustToolchainPinTests(unittest.TestCase):
    def test_all_fbuild_owned_pins_match(self) -> None:
        self.assertFalse(check_rust_toolchain_pins.validate())

    def test_selected_release_is_cfg_select_msrv(self) -> None:
        self.assertEqual(check_rust_toolchain_pins.PIN, "1.95.0")

    def test_actual_workflow_field_cannot_hide_drift_behind_comment(self) -> None:
        text = "# keep 1.95.0 in this comment\n      toolchain: 1.96.0\n"

        failures = check_rust_toolchain_pins.validate_owned_text(".github/workflows/dylint.yml", text)

        self.assertTrue(any("field drifted" in failure for failure in failures))

    def test_additional_divergent_field_cannot_hide_behind_canonical_field(self) -> None:
        text = "      toolchain: 1.95.0\n      toolchain: 1.96.0\n"

        failures = check_rust_toolchain_pins.validate_owned_text(".github/workflows/dylint.yml", text)

        self.assertTrue(any("noncanonical Rust pin 1.96.0" in failure for failure in failures))

    def test_new_file_pin_is_discovered_without_an_allowlist_entry(self) -> None:
        for selector in ("1.96.0", "1.96", "stable", "nightly"):
            with self.subTest(selector=selector):
                failures = check_rust_toolchain_pins.validate_discovered_pins(
                    ".github/workflows/new-rust-job.yml",
                    f"      toolchain: {selector}\n",
                )

                self.assertEqual(
                    failures,
                    [f"noncanonical Rust pin {selector} in .github/workflows/new-rust-job.yml:1"],
                )

    def test_dylint_nightly_is_an_exact_scoped_exception(self) -> None:
        self.assertFalse(
            check_rust_toolchain_pins.validate_discovered_pins(
                "dylints/example/rust-toolchain.toml",
                'channel = "nightly-2026-04-16"\n',
            )
        )
        self.assertTrue(
            check_rust_toolchain_pins.validate_discovered_pins(
                ".github/workflows/new-rust-job.yml",
                "toolchain: nightly-2026-04-16\n",
            )
        )

    def test_unrelated_toolchain_package_version_is_not_a_rust_pin(self) -> None:
        failures = check_rust_toolchain_pins.validate_discovered_pins(
            "crates/example/src/toolchain.rs",
            'const EMBEDDED_TOOLCHAIN_VERSION: &str = "1.110301.0";\n',
        )

        self.assertEqual(failures, [])


if __name__ == "__main__":
    unittest.main()

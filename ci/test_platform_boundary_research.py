from __future__ import annotations

import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

from ci import platform_boundary_research


class PlatformBoundaryResearchTests(unittest.TestCase):
    def test_red_fixture_contains_every_representative_violation(self) -> None:
        fixture = Path(__file__).parent / "fixtures/platform_boundary/research_red_pass.rs"
        findings = platform_boundary_research.scan_rust(fixture, platform_boundary_research.ROOT)
        kinds = {finding.kind for finding in findings}

        self.assertIn("attr_cfg", kinds)
        self.assertIn("cfg_macro", kinds)
        self.assertIn("native_path", kinds)
        self.assertIn("compile_host_fact", kinds)

    def test_comments_and_strings_are_not_findings(self) -> None:
        source = (
            '// cfg!(windows)\n'
            'const TEXT: &str = "std::os::unix";\n'
            'const RAW: &str = r#"cfg!(windows); libc::kill(1, 1)"#;\n'
        )
        code = platform_boundary_research.code_only(source)

        self.assertNotRegex(code, r"cfg\s*!|std\s*::\s*os")

    def test_compile_host_macros_are_found_but_quoted_text_is_not(self) -> None:
        with TemporaryDirectory(dir=platform_boundary_research.ROOT) as directory:
            path = Path(directory) / "compile_facts.rs"
            path.write_text(
                'const A: Option<&str> = option_env!("CARGO_CFG_TARGET_OS");\n'
                'const B: &str = env!("CARGO_CFG_TARGET_ARCH");\n'
                'const TEXT: &str = r#"env!(\\"CARGO_CFG_TARGET_ENV\\")"#;\n',
                encoding="utf-8",
            )
            findings = platform_boundary_research.scan_rust(
                path, platform_boundary_research.ROOT
            )

        macros = [
            finding
            for finding in findings
            if finding.kind == "compile_host_fact"
        ]
        self.assertEqual(len(macros), 2)

    def test_all_target_dependency_table_forms_are_recognized(self) -> None:
        for suffix in ("dependencies", "dev-dependencies", "build-dependencies"):
            with self.subTest(suffix=suffix):
                self.assertIsNotNone(
                    platform_boundary_research.TARGET_TABLE.match(
                        f"[target.'cfg(unix)'.{suffix}]"
                    )
                )

    def test_mixed_qemu_file_classifies_permissions_as_filesystem_mechanics(self) -> None:
        path = (
            platform_boundary_research.ROOT
            / "crates/fbuild-toolchain/src/toolchain/esp_qemu.rs"
        )
        findings = platform_boundary_research.scan_rust(
            path, platform_boundary_research.ROOT
        )
        permission_findings = [
            finding
            for finding in findings
            if finding.capability == "fs"
            and finding.kind in {"attr_cfg", "cfg_macro", "native_path"}
        ]

        self.assertTrue(permission_findings)
        self.assertTrue(
            all(
                finding.capability == "fs"
                and finding.classification == "host_mechanic"
                for finding in permission_findings
            )
        )

        for context in platform_boundary_research.ESP_QEMU_FS_CONTEXTS:
            with self.subTest(context=context):
                self.assertEqual(
                    platform_boundary_research.classify(
                        "crates/fbuild-toolchain/src/toolchain/esp_qemu.rs",
                        "attr_cfg",
                        "#[cfg(unix)]",
                        context,
                    ),
                    ("fs", "host_mechanic"),
                )

    def test_function_context_handles_inner_and_attached_cfg_attributes(self) -> None:
        source = """
fn outer() {
    #[cfg(unix)]
    let enabled = true;
}

#[cfg(target_os = "linux")]
fn attached() {}
"""
        code = platform_boundary_research.code_only(source)

        self.assertEqual(
            platform_boundary_research.enclosing_function(
                code, code.index("#[cfg(unix)]")
            ),
            "outer",
        )
        self.assertEqual(
            platform_boundary_research.enclosing_function(
                code, code.index("#[cfg(target_os")
            ),
            "attached",
        )

    def test_inventory_is_sorted_and_matches_committed_file(self) -> None:
        findings = platform_boundary_research.inventory()

        self.assertEqual(findings, sorted(findings))
        self.assertEqual(
            platform_boundary_research.INVENTORY.read_text(encoding="utf-8"),
            platform_boundary_research.render(findings),
        )


if __name__ == "__main__":
    unittest.main()

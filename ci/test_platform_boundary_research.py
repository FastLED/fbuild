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
        source = '// cfg!(windows)\nconst TEXT: &str = "std::os::unix";\nconst RAW: &str = r#"cfg!(windows); libc::kill(1, 1)"#;\n'
        code = platform_boundary_research.code_only(source)

        self.assertNotRegex(code, r"cfg\s*!|std\s*::\s*os")

    def test_character_literal_does_not_hide_later_cfg_macro(self) -> None:
        source = "let quote = '\"';\nlet host = cfg!(windows);\n"
        code = platform_boundary_research.code_only(source)

        self.assertRegex(code, r"cfg\s*!\s*\(windows\)")

    def test_local_concrete_host_module_path_is_inventoried(self) -> None:
        with TemporaryDirectory(dir=platform_boundary_research.ROOT) as directory:
            path = Path(directory) / "concrete.rs"
            path.write_text(
                "fn recover() { let _ = windows::Backend; }\n",
                encoding="utf-8",
            )
            findings = platform_boundary_research.scan_rust(path, platform_boundary_research.ROOT)

        self.assertEqual(
            [(finding.kind, finding.normalized) for finding in findings],
            [("native_path", "windows::")],
        )

    def test_local_linux_and_macos_module_names_are_not_native_crates(self) -> None:
        with TemporaryDirectory(dir=platform_boundary_research.ROOT) as directory:
            path = Path(directory) / "local_modules.rs"
            path.write_text(
                "fn detect() { linux::detect(); macos::detect(); unix::detect(); }\n",
                encoding="utf-8",
            )
            findings = platform_boundary_research.scan_rust(path, platform_boundary_research.ROOT)

        self.assertEqual(findings, [])

    def test_single_segment_native_use_is_inventoried(self) -> None:
        with TemporaryDirectory(dir=platform_boundary_research.ROOT) as directory:
            path = Path(directory) / "native_use.rs"
            path.write_text("use libc;\n", encoding="utf-8")
            findings = platform_boundary_research.scan_rust(path, platform_boundary_research.ROOT)

        self.assertEqual(
            [(finding.kind, finding.normalized) for finding in findings],
            [("native_path", "libc")],
        )

    def test_compile_host_macros_are_found_but_quoted_text_is_not(self) -> None:
        with TemporaryDirectory(dir=platform_boundary_research.ROOT) as directory:
            path = Path(directory) / "compile_facts.rs"
            path.write_text(
                'const A: Option<&str> = option_env!("CARGO_CFG_TARGET_OS");\nconst B: &str = env!("CARGO_CFG_TARGET_ARCH");\nconst TEXT: &str = r#"env!(\\"CARGO_CFG_TARGET_ENV\\")"#;\n',
                encoding="utf-8",
            )
            findings = platform_boundary_research.scan_rust(path, platform_boundary_research.ROOT)

        macros = [finding for finding in findings if finding.kind == "compile_host_fact"]
        self.assertEqual(len(macros), 2)

    def test_all_target_dependency_table_forms_are_recognized(self) -> None:
        for suffix in ("dependencies", "dev-dependencies", "build-dependencies"):
            with self.subTest(suffix=suffix):
                self.assertIsNotNone(platform_boundary_research.TARGET_TABLE.match(f"[target.'cfg(unix)'.{suffix}]"))

    def test_core_filesystem_manifest_occurrences_have_exact_ownership(self) -> None:
        path = "crates/fbuild-core/\x43argo.toml"
        occurrences = (
            ("target_dependency_table", "[target.'cfg(unix)'.dependencies]", ""),
            ("target_dependency_table", "[target.'cfg(windows)'.dependencies]", ""),
            ("native_dependency", "libc", "[target.'cfg(unix)'.dependencies]"),
            ("native_dependency", "windows-sys", "[target.'cfg(windows)'.dependencies]"),
        )
        for kind, normalized, context in occurrences:
            with self.subTest(kind=kind, normalized=normalized):
                self.assertEqual(
                    platform_boundary_research.classify(path, kind, normalized, context),
                    ("fs", "host_mechanic"),
                )
        for normalized, context in (
            ("libc", ""),
            ("libc", "[dependencies]"),
            ("windows-sys", "[target.'cfg(unix)'.dependencies]"),
        ):
            with self.subTest(normalized=normalized, context=context):
                self.assertEqual(
                    platform_boundary_research.classify(
                        path, "native_dependency", normalized, context
                    ),
                    ("process", "host_mechanic"),
                )

    def test_core_native_dependency_ownership_requires_matching_target_table(self) -> None:
        with TemporaryDirectory(dir=platform_boundary_research.ROOT) as directory:
            root = Path(directory)
            manifest = root / "crates" / "fbuild-core" / "\x43argo.toml"
            manifest.parent.mkdir(parents=True)
            manifest.write_text(
                "[dependencies]\nlibc = \"0.2\"\n"
                "[target.'cfg(unix)'.dependencies]\nlibc = \"0.2\"\n",
                encoding="utf-8",
            )
            dependencies = [
                finding
                for finding in platform_boundary_research.scan_manifests(root)
                if finding.kind == "native_dependency"
            ]

        self.assertEqual(
            [(finding.capability, finding.classification) for finding in dependencies],
            [("process", "host_mechanic"), ("fs", "host_mechanic")],
        )

    def test_core_ipc_dependencies_have_exact_ownership(self) -> None:
        path = "crates/fbuild-core/\x43argo.toml"
        for dependency in ("interprocess", "socket2"):
            with self.subTest(dependency=dependency):
                self.assertEqual(
                    platform_boundary_research.classify(
                        path, "native_dependency", dependency, ""
                    ),
                    ("ipc", "host_mechanic"),
                )

    def test_selected_ipc_implementation_has_exact_ownership(self) -> None:
        self.assertEqual(
            platform_boundary_research.classify(
                "crates/fbuild-core/src/platform/windows/ipc.rs",
                "native_path",
                "socket2::",
                "",
            ),
            ("ipc", "host_mechanic"),
        )

    def test_mixed_qemu_permissions_are_migrated_but_context_stays_classified(self) -> None:
        path = platform_boundary_research.ROOT / "crates/fbuild-toolchain/src/toolchain/esp_qemu.rs"
        findings = platform_boundary_research.scan_rust(path, platform_boundary_research.ROOT)
        permission_findings = [
            finding
            for finding in findings
            if finding.capability == "fs"
            and finding.kind in {"attr_cfg", "cfg_macro", "native_path"}
        ]

        self.assertFalse(permission_findings)

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
            platform_boundary_research.enclosing_function(code, code.index("#[cfg(unix)]")),
            "outer",
        )
        self.assertEqual(
            platform_boundary_research.enclosing_function(code, code.index("#[cfg(target_os")),
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

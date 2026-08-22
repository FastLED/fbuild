from __future__ import annotations

import dataclasses
import re
import unittest

from ci import enforce_platform_boundary as boundary


class EnforcePlatformBoundaryTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.expected = boundary.parse_ledger()
        cls.observed = boundary.rows_from_findings(boundary.research.inventory())

    def test_committed_exact_occurrence_ledger_matches_whole_tree(self) -> None:
        # Phase 8a (FastLED/fbuild#1314): the 24 `device`-namespace rows
        # migrated behind the platform facade are gone; what remains is
        # fs/ipc/host/process/host_executable work for later phases.
        self.assertEqual(len(self.expected), 33)
        self.assertFalse(boundary.validate_ledger(self.expected))
        self.assertFalse(boundary.compare(self.expected, self.observed))

    def test_private_platform_implementation_findings_are_not_baselined(self) -> None:
        finding = boundary.research.Finding(
            "crates/fbuild-core/src/platform/windows/example.rs",
            1,
            "compile_host_fact",
            "std::env::consts::ARCH",
            "host",
            "host_mechanic",
        )

        self.assertEqual(boundary.rows_from_findings([finding]), [])

        image_finding = boundary.research.Finding(
            "crates/fbuild-core/src/platform/executable.rs",
            1,
            "native_path",
            "std::env::current_exe",
            "host_executable",
            "host_mechanic",
        )
        self.assertEqual(boundary.rows_from_findings([image_finding]), [])

        manifest_finding = boundary.research.Finding(
            "crates/fbuild-core/Cargo.toml",
            1,
            "native_dependency",
            "windows-sys",
            "fs",
            "host_mechanic",
        )
        self.assertEqual(boundary.rows_from_findings([manifest_finding]), [])
        unix_manifest_finding = dataclasses.replace(
            manifest_finding,
            normalized="libc",
        )
        self.assertEqual(boundary.rows_from_findings([unix_manifest_finding]), [])
        unscoped_manifest_finding = dataclasses.replace(
            unix_manifest_finding,
            capability="process",
        )
        self.assertEqual(
            len(boundary.rows_from_findings([unscoped_manifest_finding])), 1
        )
        unauthorized_manifest_finding = dataclasses.replace(
            manifest_finding,
            normalized="winapi",
        )
        self.assertEqual(len(boundary.rows_from_findings([unauthorized_manifest_finding])), 1)

        unauthorized_facade_finding = dataclasses.replace(
            image_finding,
            kind="cfg_macro",
            normalized='cfg!(windows)',
        )
        self.assertEqual(
            boundary.rows_from_findings([unauthorized_facade_finding]),
            [
                boundary.LedgerRow(
                    unauthorized_facade_finding.path,
                    unauthorized_facade_finding.kind,
                    unauthorized_facade_finding.normalized,
                    0,
                    unauthorized_facade_finding.capability,
                    unauthorized_facade_finding.classification,
                )
            ],
        )

    def test_no_raw_host_fact_reads_remain_outside_the_boundary(self) -> None:
        self.assertFalse(
            [
                row
                for row in self.expected
                if row.kind in {"cfg_macro", "compile_host_fact"}
            ]
        )

    def test_no_filesystem_mechanics_remain_outside_the_boundary(self) -> None:
        self.assertFalse([row for row in self.expected if row.capability == "fs"])

    def test_rp2040_filesystem_mechanics_use_the_neutral_facade(self) -> None:
        source = (
            boundary.ROOT / "crates/fbuild-deploy/src/rp2040.rs"
        ).read_text(encoding="utf-8")
        for forbidden in ("AsRawHandle", "CancelSynchronousIo", ".raw_os_error()"):
            self.assertNotIn(forbidden, source)

    def test_daemon_ipc_and_shutdown_use_neutral_facades(self) -> None:
        backend = (
            boundary.ROOT / "crates/fbuild-daemon/src/broker/backend.rs"
        ).read_text(encoding="utf-8")
        main = (boundary.ROOT / "crates/fbuild-daemon/src/main.rs").read_text(
            encoding="utf-8"
        )

        self.assertNotIn("interprocess", backend)
        for forbidden in (
            "socket2",
            "AsRawSocket",
            "SetConsoleCtrlHandler",
            "windows_console",
        ):
            self.assertNotIn(forbidden, main)

    def test_executable_spelling_does_not_bypass_the_executable_facade(self) -> None:
        host_selected_exe = re.compile(
            r"if\s+(?:fbuild_core|crate)::platform::host::is_windows\(\)"
            r"\s*\{.{0,240}?\.exe",
            re.DOTALL,
        )
        bypasses = []
        for source in boundary.research.source_files():
            text = source.read_text(encoding="utf-8")
            for match in host_selected_exe.finditer(text):
                if "platform::executable" not in match.group(0):
                    bypasses.append(
                        f"{source.relative_to(boundary.ROOT).as_posix()}:{text.count(chr(10), 0, match.start()) + 1}"
                    )
        self.assertFalse(bypasses, bypasses)

    def test_duplicate_and_non_contiguous_ordinal_are_rejected(self) -> None:
        malformed = [*self.expected, self.expected[0]]
        failures = boundary.validate_ledger(malformed)

        self.assertTrue(any("duplicate" in failure for failure in failures))
        self.assertTrue(any("non-contiguous" in failure for failure in failures))

    def test_second_identical_occurrence_in_grandfathered_file_is_new(self) -> None:
        first = self.expected[0]
        same_group = [row for row in self.expected if (row.path, row.kind, row.normalized) == (first.path, first.kind, first.normalized)]
        extra = dataclasses.replace(first, ordinal=len(same_group))

        failures = boundary.compare(self.expected, [*self.observed, extra])

        self.assertTrue(any("new occurrence" in failure for failure in failures))

    def test_deleted_source_requires_deleting_ledger_row(self) -> None:
        failures = boundary.compare(self.expected, self.observed[1:])

        self.assertTrue(any("stale occurrence" in failure for failure in failures))

    def test_dylint_and_scanner_baselines_agree(self) -> None:
        self.assertEqual(
            boundary.parse_dylint_baseline(),
            boundary.scanner_dylint_counts(self.expected),
        )

    def test_actual_dylint_undercount_is_rejected(self) -> None:
        expected = boundary.scanner_dylint_counts(self.expected)
        source, kind, normalized = next(iter(expected))
        process = "123"
        sources = {(process, source)}
        findings = boundary.collections.Counter({(process, source, kind, normalized): expected[(source, kind, normalized)] - 1})

        failures = boundary.compare_dylint_observations(expected, sources, findings)

        self.assertTrue(any("observations disagree" in failure for failure in failures))

    def test_one_selector_and_six_neutral_namespaces(self) -> None:
        platform = boundary.ROOT / "crates/fbuild-core/src/platform"
        all_source = "\n".join(path.read_text(encoding="utf-8") for path in (boundary.ROOT / "crates").rglob("*.rs"))
        selector = (platform / "mod.rs").read_text(encoding="utf-8")

        self.assertEqual(all_source.count("std::cfg_select!"), 1)
        self.assertNotIn("_ =>", selector)
        for namespace in ("process", "fs", "ipc", "executable", "host", "device"):
            self.assertTrue((platform / f"{namespace}.rs").is_file())


if __name__ == "__main__":
    unittest.main()

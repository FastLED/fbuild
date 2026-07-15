"""Unit tests for the full-tree production USB VID/PID guard."""

import importlib.util
import sys
import unittest


spec = importlib.util.spec_from_file_location("guard", "ci/check_usb_vidpid_literals.py")
guard = importlib.util.module_from_spec(spec)
assert spec.loader is not None
sys.modules[spec.name] = guard
spec.loader.exec_module(guard)


class GuardTests(unittest.TestCase):
    def reasons(self, path: str, source: str) -> list[str]:
        return [finding.reason for finding in guard.scan_text(path, source)]

    def test_production_pair_is_rejected(self):
        reasons = self.reasons("crates/demo/src/boards.rs", "const ID: (u16, u16) = (0x1234, 0x5678);")
        self.assertIn("USB-shaped numeric pair", reasons)

    def test_named_single_literal_is_rejected(self):
        reasons = self.reasons("crates/demo/src/device.rs", "if vid == 0x1234 { return true; }")
        self.assertIn("named VID/PID literal", reasons)

    def test_named_decimal_literal_is_rejected(self):
        reasons = self.reasons("crates/demo/src/device.rs", "const DEVICE_VID: u16 = 4660;")
        self.assertIn("named VID/PID literal", reasons)

    def test_board_json_separate_fields_are_rejected(self):
        findings = guard.scan_text(
            "crates/fbuild-config/assets/boards/json/demo.json",
            '{\n  "vid": "0x1234",\n  "pid": "5678"\n}',
        )
        self.assertEqual(
            [finding.reason for finding in findings],
            ["VID/PID field", "VID/PID field"],
        )

    def test_embedded_catalogue_is_rejected(self):
        reasons = self.reasons(
            "crates/demo/src/data.rs",
            'const IDS: &[u8] = include_bytes!("data/usb-ids.bin");',
        )
        self.assertIn("embedded USB identity asset", reasons)

    def test_cfg_test_module_is_allowed_and_following_code_is_scanned(self):
        source = """
#[cfg(test)]
mod tests {
    const ID: (u16, u16) = (0x1234, 0x5678);
}
const VID: u16 = 0x9999;
"""
        findings = guard.scan_text("crates/demo/src/lib.rs", source)
        self.assertEqual(len(findings), 1)
        self.assertEqual(findings[0].line, 6)

    def test_cfg_test_single_item_is_allowed(self):
        source = """
#[cfg(test)]
const ID: (u16, u16) = (0x1234, 0x5678);
"""
        self.assertEqual(guard.scan_text("crates/demo/src/lib.rs", source), [])

    def test_cfg_test_raw_string_braces_do_not_end_module_early(self):
        source = '''
#[cfg(test)]
mod tests {
    const JSON: &str = r#"{"vid":"1234"}"#;
    const ID: (u16, u16) = (0x1234, 0x5678);
}
'''
        self.assertEqual(guard.scan_text("crates/demo/src/lib.rs", source), [])

    def test_test_path_and_frozen_fixture_are_allowed(self):
        source = "const ID: (u16, u16) = (0x1234, 0x5678);"
        self.assertEqual(guard.scan_text("crates/demo/tests/fixture.rs", source), [])
        self.assertEqual(guard.scan_text("crates/fbuild-core/data/fixture.txt", source), [])


if __name__ == "__main__":
    unittest.main()

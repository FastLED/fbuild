"""Unit tests for the production USB VID/PID diff guard."""

import importlib.util
import unittest


spec = importlib.util.spec_from_file_location("guard", "ci/check_usb_vidpid_literals.py")
guard = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(guard)


class GuardTests(unittest.TestCase):
    def test_catalogue_hex_pair_is_rejected(self):
        diff = "+++ b/crates/fbuild-serial/src/boards.rs\n+    (0x1234, 0x5678),"
        self.assertEqual(
            guard.added_production_pairs(diff),
            [("crates/fbuild-serial/src/boards.rs", "1234", "5678")],
        )

    def test_board_json_separate_fields_are_rejected(self):
        diff = (
            "+++ b/crates/fbuild-config/assets/boards/json/demo.json\n"
            '+  "vid": "0x1234",\n'
            '+  "pid": "5678",\n'
        )
        self.assertEqual(
            guard.added_production_pairs(diff),
            [
                ("crates/fbuild-config/assets/boards/json/demo.json", "VID", "0x1234"),
                ("crates/fbuild-config/assets/boards/json/demo.json", "PID", "5678"),
            ],
        )

    def test_fixture_marker_cannot_bypass_production_guard(self):
        diff = (
            "+++ b/crates/fbuild-serial/src/boards.rs\n"
            "+ // USB_VIDPID_ALLOW (0x1234, 0x5678)\n"
        )
        self.assertEqual(
            guard.added_production_pairs(diff),
            [("crates/fbuild-serial/src/boards.rs", "1234", "5678")],
        )

    def test_test_fixture_is_out_of_production_scope(self):
        diff = "+++ b/crates/fbuild-serial/tests/fixture.rs\n+ (0x1234, 0x5678)"
        self.assertEqual(guard.added_production_pairs(diff), [])


if __name__ == "__main__":
    unittest.main()

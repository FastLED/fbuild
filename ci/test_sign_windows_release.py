"""Tests for ci/sign_windows_release.py and its release-workflow wiring."""

from __future__ import annotations

import struct
import sys
import tempfile
import unittest
from pathlib import Path

import yaml

sys.path.insert(0, str(Path(__file__).resolve().parent))

import sign_windows_release as sign  # noqa: E402

REPO = Path(__file__).resolve().parents[1]
WORKFLOWS = REPO / ".github" / "workflows"
PUBLISHER = "Zachary Vorhies"
SIGNED_PAYLOAD = b"\x30\x82" + PUBLISHER.encode() + sign.RFC3161_TIMESTAMP_OID


def _pe(cert: bytes | None = None) -> bytes:
    """Minimal PE32+ image, optionally with a WIN_CERTIFICATE table."""
    pe_offset = 0x40
    optional = pe_offset + 24
    entry = optional + 112 + sign.SECURITY_DIRECTORY_INDEX * 8
    image = bytearray(entry + 8)
    image[0:2] = b"MZ"
    struct.pack_into("<I", image, 0x3C, pe_offset)
    image[pe_offset : pe_offset + 4] = b"PE\0\0"
    struct.pack_into("<H", image, optional, 0x20B)
    if cert is not None:
        blob = struct.pack(
            "<IHH",
            8 + len(cert),
            sign.WIN_CERT_REVISION_2,
            sign.WIN_CERT_TYPE_PKCS_SIGNED_DATA,
        )
        struct.pack_into("<II", image, entry, len(image), len(blob) + len(cert))
        image += blob + cert
    return bytes(image)


def _workflow(name: str) -> dict:
    return yaml.safe_load((WORKFLOWS / name).read_text(encoding="utf-8"))


class SignatureCheckTests(unittest.TestCase):
    def test_requires_timestamped_signature_by_publisher(self) -> None:
        self.assertIsNone(sign.signature_problem(_pe(SIGNED_PAYLOAD), PUBLISHER))
        cases = [
            (_pe(), "no Authenticode signature"),
            (_pe(b"CN=Someone Else" + sign.RFC3161_TIMESTAMP_OID), "publisher"),
            (_pe(b"\x30\x82" + PUBLISHER.encode()), "RFC 3161"),
        ]
        for image, problem in cases:
            self.assertIn(problem, sign.signature_problem(image, PUBLISHER) or "")

    def test_verify_covers_exe_and_pyd_and_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            staging = Path(tmp) / "staging"
            staging.mkdir()
            (staging / "fbuild.exe").write_bytes(_pe(SIGNED_PAYLOAD))
            (staging / "fbuild-daemon.exe").write_bytes(_pe(SIGNED_PAYLOAD))
            (staging / "_native.pyd").write_bytes(_pe())
            (staging / "SHA256SUMS.txt").write_text("not a PE")
            with self.assertRaisesRegex(sign.SigningError, "_native.pyd"):
                sign.verify_dirs([staging], PUBLISHER)
            (staging / "_native.pyd").write_bytes(_pe(SIGNED_PAYLOAD))
            sign.verify_dirs([staging], PUBLISHER)

            empty = Path(tmp) / "empty"
            empty.mkdir()
            with self.assertRaisesRegex(sign.SigningError, "no Windows PE"):
                sign.verify_dirs([empty], PUBLISHER)

    def test_jsign_command_pins_artifact_signing_and_keeps_token_off_argv(self) -> None:
        config = sign.SigningConfig(
            sign.endpoint_host("https://eus.codesigning.azure.net/"),
            "zvorhies-signing",
            "zachvorhies-public-trust",
            PUBLISHER,
        )
        command = sign.jsign_command(Path("jsign.jar"), config, [Path("fbuild.exe")])
        flags = dict(zip(command[3::2], command[4::2]))
        self.assertEqual(flags["--storetype"], "TRUSTEDSIGNING")
        self.assertEqual(flags["--keystore"], "eus.codesigning.azure.net")
        self.assertEqual(flags["--storepass"], f"env:{sign.TOKEN_ENV}")
        self.assertEqual(flags["--alias"], "zvorhies-signing/zachvorhies-public-trust")
        self.assertEqual(flags["--alg"], "SHA-256")
        self.assertEqual(flags["--tsaurl"], "http://timestamp.acs.microsoft.com")
        self.assertEqual(flags["--tsmode"], "RFC3161")
        self.assertEqual(
            sign.publisher_common_name("CN=Zachary Vorhies, O=Zachary Vorhies, C=US"),
            PUBLISHER,
        )
        with self.assertRaises(sign.SigningError):
            sign.endpoint_host("https://example.com/")

    def test_config_requires_every_secret(self) -> None:
        env = {
            "AZURE_SIGNING_ENDPOINT": "https://eus.codesigning.azure.net",
            "AZURE_SIGNING_ACCOUNT": "a",
            "AZURE_SIGNING_PROFILE": "p",
        }
        with self.assertRaisesRegex(sign.SigningError, "AZURE_SIGN_PUBLISHER"):
            sign.config_from_env(env)


class WorkflowWiringTests(unittest.TestCase):
    def test_template_signs_before_checksums_attestation_and_upload(self) -> None:
        steps = _workflow("template_native_build.yml")["jobs"]["build"]["steps"]
        names = [step.get("name", "") for step in steps]
        order = [
            names.index("Stage artifacts"),
            names.index("Azure login for Authenticode signing"),
            names.index("Authenticode-sign Windows binaries"),
            names.index("Generate checksums"),
            names.index("Attest native artifacts"),
            names.index("Upload artifacts"),
        ]
        self.assertEqual(order, sorted(order))
        login = steps[order[1]]
        self.assertEqual(
            login["uses"], "azure/login@a457da9ea143d694b1b9c7c869ebb04ebe844ef5"
        )
        self.assertNotIn("client-secret", login["with"])
        for step in (login, steps[order[2]]):
            self.assertEqual(step["if"], "inputs.sign_windows")

    def test_release_signs_only_windows_lanes_and_gates_publication(self) -> None:
        jobs = _workflow("release-auto.yml")["jobs"]
        build = jobs["build"]
        self.assertEqual(
            build["with"]["sign_windows"], "${{ contains(matrix.target, 'windows') }}"
        )
        self.assertEqual(build["permissions"]["id-token"], "write")
        self.assertEqual(build["permissions"]["contents"], "read")
        self.assertNotIn("AZURE_CLIENT_SECRET", build["secrets"])
        for legacy in (
            "AZURE_SIGN_ENDPOINT",
            "AZURE_SIGN_ACCOUNT",
            "AZURE_SIGN_PROFILE",
        ):
            self.assertNotIn(legacy, build["secrets"])
        self.assertIn("windows-release-smoke", jobs["publish"]["needs"])
        self.assertIn("windows-release-smoke", jobs["build-pypi"]["needs"])
        smoke = jobs["windows-release-smoke"]["steps"][-1]["run"]
        self.assertEqual(smoke, "python ci/sign_windows_release.py smoke --dir native")
        self.assertIn("Get-AuthenticodeSignature", sign.AUTHENTICODE_PS)

    def test_unsigned_build_workflow_does_not_sign(self) -> None:
        build = _workflow("build.yml")["jobs"]["build"]
        self.assertNotIn("sign_windows", build.get("with", {}))


if __name__ == "__main__":
    unittest.main()

"""Stream Cargo tests and identify completed assertion failures for cache saves."""

import json
import os
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

JsonValue = str | int | float | bool | None | list["JsonValue"] | dict[str, "JsonValue"]

@dataclass(frozen=True)
class TestEvidence:
    build_finished: bool = False
    compiler_failed: bool = False
    tests_failed: bool = False
    last_error: str = ""

    def observe(self, line: str) -> "TestEvidence":
        clean = re.sub(r"\x1b\[[0-9;]*m", "", line).strip()
        clean = re.sub(r"^\d+\.\d+\s+", "", clean)
        if clean.startswith("soldr[cache] "):
            return self
        finished = self.build_finished
        failed = self.compiler_failed or "Couldn't compile the test." in clean
        try:
            record: JsonValue = json.loads(clean)
        except json.JSONDecodeError:
            record = None
        if isinstance(record, dict):
            if record.get("reason") == "build-finished":
                finished = record.get("success") is True
                failed = failed or not finished
            if record.get("reason") == "compiler-message":
                message = record.get("message")
                if isinstance(message, dict) and message.get("level") == "error":
                    failed = True
        return TestEvidence(
            finished,
            failed,
            self.tests_failed or clean.startswith("test result: FAILED."),
            clean if clean.startswith("error:") else self.last_error,
        )

    def assertion_failure_only(self, returncode: int) -> bool:
        return (
            returncode == 101
            and self.build_finished
            and not self.compiler_failed
            and self.tests_failed
            and re.fullmatch(r"error: (?:test|doctest) failed(?:,.*)?", self.last_error) is not None
        )


def main() -> int:
    argv = sys.argv[1:]
    if argv[:1] == ["--"]:
        argv = argv[1:]
    if argv[:3] != ["soldr", "cargo", "test"]:
        raise SystemExit("expected soldr cargo test command")
    boundary = argv.index("--") if "--" in argv else len(argv)
    argv.insert(boundary, "--message-format=json")
    evidence = TestEvidence()
    with subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True) as child:
        assert child.stdout is not None
        for line in child.stdout:
            print(line, end="", flush=True)
            evidence = evidence.observe(line)
        returncode = child.wait()
    output = os.environ.get("GITHUB_OUTPUT")
    if output:
        with Path(output).open("a") as stream:
            allowed = str(evidence.assertion_failure_only(returncode)).lower()
            stream.write(f"assertion_failure_only={allowed}\n")
    return returncode


if __name__ == "__main__":
    raise SystemExit(main())

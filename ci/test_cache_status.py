"""Stream Cargo tests, identify completed assertion failures, and name daemon deaths.

The assertions decide cache saves. The daemon classification exists because the
Dell forge pool's *shared* soldr daemon kills cold compiles mid-flight
(FastLED/fbuild#2178): without it a red trunk is indistinguishable from a red
diff, and the reflex fix -- ``gh run rerun --failed`` -- is not the repair.

Neither classification changes the exit status.
"""

import json
import os
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

JsonValue = str | int | float | bool | None | list["JsonValue"] | dict[str, "JsonValue"]

RELAY_RESET = "soldr#3377"
LOST_DAEMON = "lost the connection to the soldr daemon"
ROUTE = re.compile(r"route (soldr-daemon-[0-9A-Za-z]+)")


def normalized(line: str) -> str:
    """Strip ANSI, the soldr elapsed-time prefix and cc-rs warning forwarding."""

    clean = re.sub(r"\x1b\[[0-9;]*m", "", line).strip()
    clean = re.sub(r"^\d+\.\d+\s+", "", clean)
    return clean.removeprefix("cargo:warning=").strip()


FORWARDED_PREFIX = re.compile(r"^(?:warning|note|error): [\w@./+-]+: ")


def forwarded_body(message: str) -> str:
    """The event itself, without cc-rs's ``warning: <package>: `` decoration.

    One kill is reported twice by cargo -- once as the build script's own
    ``warning: <package>: ...`` line and once as the forwarded
    ``cargo:warning=...`` copy -- so the decoration must not inflate the count.
    """

    return FORWARDED_PREFIX.sub("", message)


def _append(seen: tuple[str, ...], message: str) -> tuple[str, ...]:
    body = forwarded_body(message)
    if any(forwarded_body(existing) == body for existing in seen):
        return seen
    return (*seen, message)


@dataclass(frozen=True)
class TestEvidence:
    """What the runner itself proved about the build it just ran.

    ``build_failed`` is NOT a code signal: a daemon kill reaches cargo as a
    build-script exit 101, so ``{"reason":"build-finished","success":false}``
    is the *consequence* of the very death being classified and cannot veto
    that classification. Only a rustc error diagnostic or a failed test does.
    """

    build_finished: bool = False
    build_failed: bool = False
    compiler_failed: bool = False
    tests_failed: bool = False
    last_error: str = ""

    def observe(self, line: str) -> "TestEvidence":
        clean = normalized(line)
        if clean.startswith("soldr[cache] "):
            return self
        finished = self.build_finished
        build_failed = self.build_failed
        failed = self.compiler_failed or "Couldn't compile the test." in clean
        try:
            record: JsonValue = json.loads(clean)
        except json.JSONDecodeError:
            record = None
        if isinstance(record, dict):
            if record.get("reason") == "build-finished":
                finished = record.get("success") is True
                build_failed = build_failed or not finished
            if record.get("reason") == "compiler-message":
                message = record.get("message")
                if isinstance(message, dict) and message.get("level") == "error":
                    failed = True
        return TestEvidence(
            finished,
            build_failed,
            failed,
            self.tests_failed or clean.startswith("test result: FAILED."),
            clean if clean.startswith("error:") else self.last_error,
        )

    def assertion_failure_only(self, returncode: int) -> bool:
        return (
            returncode == 101
            and self.build_finished
            and not self.build_failed
            and not self.compiler_failed
            and self.tests_failed
            and re.fullmatch(r"error: (?:test|doctest) failed(?:,.*)?", self.last_error)
            is not None
        )


@dataclass(frozen=True)
class DaemonDeath:
    """Compile sessions the shared soldr daemon dropped, as the log reported them."""

    resets: tuple[str, ...] = ()
    lost: tuple[str, ...] = ()

    def observe(self, line: str) -> "DaemonDeath":
        clean = normalized(line)
        if RELAY_RESET in clean:
            return DaemonDeath(_append(self.resets, clean), self.lost)
        if LOST_DAEMON in clean:
            return DaemonDeath(self.resets, _append(self.lost, clean))
        return self

    def route(self) -> str:
        for message in (*self.resets, *self.lost):
            match = ROUTE.search(message)
            if match:
                return match.group(1)
        return ""

    def verdict(self) -> str:
        """One sentence, or '' when the daemon was never blamed."""

        signs = []
        if self.resets:
            signs.append(f"{len(self.resets)} relay reset(s) ({RELAY_RESET})")
        if self.lost:
            signs.append(f"{len(self.lost)} lost-connection report(s)")
        if not signs:
            return ""
        route = self.route()
        where = f" on route {route}" if route else ""
        return f"the shared soldr daemon killed the compile{where}: " + ", ".join(signs)


def emit_daemon_verdict(
    daemon: DaemonDeath, returncode: int, evidence: TestEvidence
) -> None:
    """Annotate the run so a red trunk is not read as a code failure.

    A daemon death explains a *compile* that never produced output. It never
    excuses a test the suite itself reported as failed: when the runner also
    saw ``test result: FAILED`` or a rustc error diagnostic, the daemon hit is
    context, not a verdict, and claiming "not a code failure" would hide a real
    break. A bare failed ``build-finished`` record is deliberately NOT evidence:
    the kill itself ends the build script with exit 101.
    """

    verdict = daemon.verdict()
    if not verdict:
        return
    if evidence.tests_failed or evidence.compiler_failed:
        print(
            f"::error::{verdict}; the suite also reported a failure of its own, "
            "so this is NOT proven infra -- read the failing test",
            flush=True,
        )
        return
    print(
        f"::{'error' if returncode else 'warning'}::INFRA - {verdict}; not a code failure",
        flush=True,
    )
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with Path(summary).open("a") as stream:
            stream.write(
                f"### soldr daemon died mid-compile (infra)\n\n{verdict}\n\n"
                "Not a code failure, and `gh run rerun --failed` is not the repair while the\n"
                "host's shared daemon is wedged (FastLED/fbuild#2178). The lane needs the daemon\n"
                "restarted or the pool routed elsewhere.\n"
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
    daemon = DaemonDeath()
    with subprocess.Popen(
        argv, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True
    ) as child:
        assert child.stdout is not None
        for line in child.stdout:
            print(line, end="", flush=True)
            evidence = evidence.observe(line)
            daemon = daemon.observe(line)
        returncode = child.wait()
    emit_daemon_verdict(daemon, returncode, evidence)
    output = os.environ.get("GITHUB_OUTPUT")
    if output:
        with Path(output).open("a") as stream:
            allowed = str(evidence.assertion_failure_only(returncode)).lower()
            stream.write(f"assertion_failure_only={allowed}\n")
    return returncode


if __name__ == "__main__":
    raise SystemExit(main())

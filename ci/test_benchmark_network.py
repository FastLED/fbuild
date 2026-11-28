"""The benchmark namespace must survive one dead DNS resolver (fbuild#1959 class).

Run 46130090245 (2026-11-28) failed the nightly comparison after 45 minutes of
healthy downloads: `arduino-cli core install esp32:esp32@3.3.12` died three
times with

    dial tcp: lookup github.com on 1.1.1.1:53: read udp 10.200.0.2:...: i/o timeout

The TCP path was fine -- the preceding `esp32-libs`, `esp32c3-libs` and
`esp32c5-libs` archives all downloaded over the same shaped veth -- so the only
broken hop was the namespace's resolver, which the workflow pinned to a SINGLE
`nameserver 1.1.1.1`. One resolver refusing UDP loses the whole night of
measurements, and the Rust retry loop cannot help because every attempt fails
at the same dead address.

The namespace therefore declares more than one resolver and bounds the per
server wait, so a dead first address fails over to a working one inside the
retry budget instead of consuming it.
"""

from __future__ import annotations

import ipaddress
import re
import unittest
from pathlib import Path

import yaml

WORKFLOW = (
    Path(__file__).resolve().parents[1]
    / ".github"
    / "workflows"
    / "benchmark-build-comparison.yml"
)

NETNS_STEP = "Create the 10 Mbit/s benchmark network namespace"
RESOLV_PATH = "/etc/netns/bench/resolv.conf"


def netns_script() -> str:
    document = yaml.safe_load(WORKFLOW.read_text(encoding="utf-8"))
    for job in document["jobs"].values():
        for step in job.get("steps", []):
            if step.get("name") == NETNS_STEP:
                return step["run"]
    raise AssertionError(f"{WORKFLOW.name} has no {NETNS_STEP!r} step")


def logical_lines(script: str) -> list[str]:
    """Shell commands with backslash continuations folded into one line each."""
    lines: list[str] = []
    buffer = ""
    for raw in script.splitlines():
        stripped = raw.strip()
        if stripped.endswith("\\"):
            buffer += stripped[:-1] + " "
            continue
        lines.append(buffer + stripped)
        buffer = ""
    if buffer:
        lines.append(buffer)
    return lines


def resolv_conf_lines(script: str) -> list[str]:
    """The lines the netns step writes into the namespace's resolv.conf."""
    for command in logical_lines(script):
        if RESOLV_PATH not in command:
            continue
        if "printf" not in command and "echo" not in command:
            continue
        # `sudo -S -p ''` contributes an empty quoted token; the payload does not.
        payloads = [q for q in re.findall(r"'([^']*)'", command) if q]
        if payloads:
            # `printf 'a\nb'` carries literal escapes; `echo 'a'` does not.
            return "\n".join(payloads).replace("\\n", "\n").splitlines()
    raise AssertionError(f"no `tee {RESOLV_PATH}` payload in {NETNS_STEP!r}")


def nameservers(lines: list[str]) -> list[str]:
    return [line.split()[1] for line in lines if line.split()[:1] == ["nameserver"]]


class BenchmarkNamespaceResolverTests(unittest.TestCase):
    def test_the_namespace_still_overrides_resolv_conf(self) -> None:
        self.assertIn(RESOLV_PATH, netns_script())

    def test_more_than_one_resolver_is_declared(self) -> None:
        addresses = nameservers(resolv_conf_lines(netns_script()))
        distinct = {str(ipaddress.ip_address(a)) for a in addresses}
        self.assertGreaterEqual(
            len(distinct), 2, f"one dead resolver loses the nightly: {addresses}"
        )

    def test_the_resolvers_are_distinct_operators(self) -> None:
        # Same-operator replicas share an outage; ask two independent resolvers.
        addresses = nameservers(resolv_conf_lines(netns_script()))
        distinct = sorted(set(addresses))
        self.assertEqual(len(distinct), len(addresses), addresses)
        self.assertNotEqual(
            {a.rsplit(".", 1)[0] for a in distinct},
            {distinct[0].rsplit(".", 1)[0]},
            f"single-operator resolvers cannot fail over: {addresses}",
        )

    def test_failover_is_bounded(self) -> None:
        lines = resolv_conf_lines(netns_script())
        options = [line for line in lines if line.split()[:1] == ["options"]]
        self.assertTrue(options, "no `options` line to bound the resolver wait")
        settings = dict(token.split(":", 1) for token in options[0].split()[1:])
        self.assertLessEqual(
            int(settings.get("timeout", "5")),
            2,
            f"a 5s wait per server stalls the retry budget: {options[0]}",
        )
        self.assertGreaterEqual(int(settings.get("attempts", "2")), 2)


if __name__ == "__main__":
    unittest.main()

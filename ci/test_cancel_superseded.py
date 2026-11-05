"""Tests for ci/cancel_superseded.py -- the merge-queue janitor.

The script cancels in-flight `merge_group` runs that a newer queue run for the
same PR replaced. Two ways it used to get that wrong are regression cases here:

* timestamps were compared as STRINGS.  GitHub sometimes emits fractional
  seconds (`...00.500Z`), and `.` sorts before `Z`, so a run created 500 ms
  AFTER a whole-second one read as older -- the janitor cancelled the LIVE
  queue run and left the stale one.  Order is now (datetime, id).
* a same-second tie matched neither run, so the redundant run survived and
  held a runner for the rest of the queue; the id breaks the tie.

It also used to print `PR #None: cancelling 0` and exit 0 when the triggering
run's ref was not a merge-queue ref: a broken event wiring reported success for
a cleanup with no target.  That is now a hard error.

Everything drives the real module; the API is stubbed so no network or token is
needed.  The script must stay standalone: its workflow job sparse-checks out
only `ci/cancel_superseded.py`, so it cannot import sibling `ci` modules.
"""

from __future__ import annotations

import contextlib
import io
import unittest
import warnings
from urllib.error import HTTPError, URLError

from ci import cancel_superseded as cancel

BASE = "gh-readonly-queue/main"
CURRENT = "2026-11-05T20:00:00Z"


def run(run_id, pr, status="in_progress", created_at=CURRENT, base="main"):
    return {
        "id": run_id,
        "workflow_id": 9,
        "head_branch": f"{BASE}/pr-{pr}-abc123" if pr is not None else base,
        "status": status,
        "created_at": created_at,
    }


class QueueRefTest(unittest.TestCase):
    def test_parses_a_queue_ref(self):
        self.assertEqual(1919, cancel.queue_pr(f"{BASE}/pr-1919-deadbeef"))

    def test_parses_a_non_main_base(self):
        self.assertEqual(
            7, cancel.queue_pr("gh-readonly-queue/release/2.x/pr-7-abc123")
        )

    def test_rejects_a_branch_ref(self):
        self.assertIsNone(cancel.queue_pr("main"))

    def test_rejects_a_sha_suffix_that_is_not_hex(self):
        self.assertIsNone(cancel.queue_pr(f"{BASE}/pr-1-nothex"))


class SupersededTest(unittest.TestCase):
    def test_cancels_an_older_in_flight_run_of_the_same_pr(self):
        old = run(900, 1919, created_at="2026-11-05T19:00:00Z")
        current = run(901, 1919)
        self.assertEqual([900], cancel.superseded([old, current], current))

    def test_ignores_a_run_for_a_different_pr(self):
        other = run(902, 1918, created_at="2026-11-05T19:00:00Z")
        current = run(901, 1919)
        self.assertEqual([], cancel.superseded([other, current], current))

    def test_ignores_a_finished_run(self):
        done = run(903, 1919, status="completed", created_at="2026-11-05T19:00:00Z")
        current = run(901, 1919)
        self.assertEqual([], cancel.superseded([done, current], current))

    def test_ignores_the_current_run(self):
        current = run(901, 1919)
        self.assertEqual([], cancel.superseded([current], current))

    def test_does_not_cancel_a_fractional_second_run_that_is_later(self):
        """REGRESSION: `.500Z` is LATER than a whole second; string compare flipped it."""
        newer = run(906, 77, created_at="2026-11-05T20:00:00.500Z")
        current = run(905, 77, created_at="2026-11-05T20:00:00Z")
        self.assertEqual([], cancel.superseded([newer, current], current))

    def test_cancels_a_fractional_second_run_that_is_earlier(self):
        older = run(907, 77, created_at="2026-11-05T19:59:59.900Z")
        current = run(905, 77, created_at="2026-11-05T20:00:00Z")
        self.assertEqual([907], cancel.superseded([older, current], current))

    def test_same_second_tie_cancels_the_lower_id(self):
        """REGRESSION: a strict `<` left BOTH runs alive; ids increase with time."""
        first = run(900, 1919)
        second = run(901, 1919)
        self.assertEqual([900], cancel.superseded([first, second], second))
        self.assertEqual([], cancel.superseded([first, second], first))

    def test_reports_every_older_run_of_the_pr(self):
        runs = [
            run(910, 1919, "queued", "2026-11-05T19:00:00Z"),
            run(911, 1919, created_at="2026-11-05T19:30:00Z"),
        ]
        current = run(912, 1919)
        self.assertEqual([910, 911], cancel.superseded([*runs, current], current))


class OrderTest(unittest.TestCase):
    def test_fractional_seconds_order_chronologically(self):
        early = run(1, 1919, created_at="2026-11-05T20:00:00Z")
        later = run(2, 1919, created_at="2026-11-05T20:00:00.500Z")
        self.assertLess(cancel.order(early), cancel.order(later))

    def test_missing_timestamp_is_an_error(self):
        with self.assertRaises(ValueError):
            cancel.order({"id": 1, "head_branch": "main"})

    def test_unparseable_timestamp_is_an_error(self):
        with self.assertRaises(ValueError):
            cancel.order(run(1, 1919, created_at="yesterday"))


class FakeApi:
    """Serves the three endpoints the script uses, and records cancels."""

    def __init__(self, current, runs, cancel_error=None):
        self.current = current
        self.runs = runs
        self.cancel_error = cancel_error
        self.cancelled = []
        self.paths = []

    def __call__(self, path, method="GET"):
        self.paths.append((method, path))
        if path.endswith("/cancel"):
            run_id = int(path.rsplit("/", 2)[-2])
            if self.cancel_error is not None:
                raise self.cancel_error
            self.cancelled.append(run_id)
            return {}
        if "/actions/workflows/" in path:
            return {"workflow_runs": list(self.runs)}
        return self.current


class MainTest(unittest.TestCase):
    def setUp(self):
        # HTTPError subclasses file-like; a fabricated one warns on cleanup.
        warnings.simplefilter("ignore", ResourceWarning)
        self.addCleanup(warnings.resetwarnings)
        self.addCleanup(setattr, cancel, "api", cancel.api)
        self.addCleanup(self._clear_env)
        cancel.os.environ["GITHUB_REPOSITORY"] = "FastLED/fbuild"
        cancel.os.environ["GITHUB_RUN_ID"] = "901"

    @staticmethod
    def _clear_env():
        for name in ("GITHUB_REPOSITORY", "GITHUB_RUN_ID"):
            cancel.os.environ.pop(name, None)

    def drive(self, api, *argv):
        cancel.api = api
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = cancel.main(list(argv))
        return code, out.getvalue(), err.getvalue()

    def test_cancels_the_stale_run(self):
        current = run(901, 1919)
        api = FakeApi(
            current, [run(900, 1919, created_at="2026-11-05T19:00:00Z"), current]
        )
        code, out, _ = self.drive(api)
        self.assertEqual(0, code)
        self.assertEqual([900], api.cancelled)
        self.assertIn("cancelling 1", out)

    def test_dry_run_does_not_cancel(self):
        current = run(901, 1919)
        api = FakeApi(
            current, [run(900, 1919, created_at="2026-11-05T19:00:00Z"), current]
        )
        code, out, _ = self.drive(api, "--dry-run")
        self.assertEqual(0, code)
        self.assertEqual([], api.cancelled)
        self.assertIn("[900]", out)

    def test_a_non_queue_ref_is_a_hard_error(self):
        """REGRESSION: this used to print `PR #None: cancelling 0` and exit 0."""
        current = run(901, None, base="main")
        api = FakeApi(current, [current])
        code, out, err = self.drive(api)
        self.assertEqual(1, code)
        self.assertIn("not a merge-queue run", err)
        self.assertIn("'main'", err)
        self.assertEqual([], api.cancelled)
        self.assertNotIn("PR #None", out)

    def test_a_finished_run_conflict_is_tolerated(self):
        current = run(901, 1919)
        error = HTTPError("https://api.github.com/x", 409, "Conflict", {}, None)
        api = FakeApi(
            current,
            [run(900, 1919, created_at="2026-11-05T19:00:00Z"), current],
            error,
        )
        code, out, _ = self.drive(api)
        self.assertEqual(0, code)
        self.assertIn("409", out)

    def test_a_network_error_does_not_block_the_queue(self):
        current = run(901, 1919)
        error = URLError("connection reset")
        api = FakeApi(
            current,
            [run(900, 1919, created_at="2026-11-05T19:00:00Z"), current],
            error,
        )
        code, out, _ = self.drive(api)
        self.assertEqual(0, code)
        self.assertIn("leaving it", out)

    def test_cancels_every_stale_run_of_the_pr_only(self):
        current = run(912, 1919)
        stale = [
            run(910, 1919, "queued", "2026-11-05T19:00:00Z"),
            run(911, 1919, created_at="2026-11-05T19:30:00Z"),
        ]
        keep = [
            run(913, 1918, created_at="2026-11-05T19:40:00Z"),
            run(914, 1919, "completed", "2026-11-05T19:45:00Z"),
        ]
        api = FakeApi(current, [*stale, *keep, current])
        code, _, _ = self.drive(api)
        self.assertEqual(0, code)
        self.assertEqual([910, 911], sorted(api.cancelled))


if __name__ == "__main__":
    unittest.main()

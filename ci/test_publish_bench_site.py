"""Two publishers must never destroy each other's row (fbuild#2005 follow-up).

`ci/merge_bench_history.py` fixed the *sequential* case: a later publisher reads
the branch after the earlier one pushed, so unioning keeps both rows. It cannot
fix the *overlapping* case, because the branch is read before the rival pushes
and `git push --force` never fails -- the rival's row is reverted silently and
`history.jsonl` is append-only, so that measurement is gone for good.

These tests drive real git repositories (a bare `origin` plus two independent
publish dirs) instead of mocking `subprocess`, because the defect lives in git's
ref semantics: the lease is what turns "lost row" into "rejected push". The
interleaved test is RED against the old single-shot force push -- the seed rows
plus B's row survive, A's row is missing.
"""

from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path

from ci.publish_bench_site import PublishError, PublishSpec, publish

SEED_ROWS = 3


def row(ts_offset: int, sha: str, ratio: float = 0.5) -> str:
    base = int(1e9)
    return f'{{"ts": "unix:{base + ts_offset}", "sha": "{sha}", "ratio": {ratio}}}'


def run(*args: str, cwd: Path) -> str:
    result = subprocess.run(
        ["git", "-C", str(cwd), *args], capture_output=True, text=True, check=True
    )
    return result.stdout


class BenchSitePublishTests(unittest.TestCase):
    """One bare remote, real branches, two publishers racing on it."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.origin = self.root / "origin.git"
        self.origin.mkdir()
        run("init", "--bare", "--quiet", str(self.origin), cwd=self.root)
        self._seed_remote()

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def _seed_remote(self) -> None:
        seed = self.root / "seed"
        seed.mkdir()
        run("init", "--quiet", cwd=seed)
        run("config", "user.name", "seed", cwd=seed)
        run("config", "user.email", "seed@example.com", cwd=seed)
        history = "".join(
            f"{row(index, f'seed{index:04d}')}\n" for index in range(SEED_ROWS)
        )
        (seed / "history.jsonl").write_text(history, encoding="utf-8")
        (seed / "index.html").write_text("<html>published</html>", encoding="utf-8")
        run("add", "-A", cwd=seed)
        run("commit", "--quiet", "-m", "seed", cwd=seed)
        run(
            "push",
            "--quiet",
            str(self.origin),
            "HEAD:refs/heads/benchmark-stats",
            cwd=seed,
        )

    def _mirror(self) -> Path:
        mirror = self.root / "mirror-src"
        (mirror / "nested").mkdir(parents=True, exist_ok=True)
        (mirror / "manifest.json").write_text('{"schema": 3}', encoding="utf-8")
        (mirror / "nested" / "owner.json").write_text("{}", encoding="utf-8")
        return mirror

    def _measurements(self, name: str, sha: str, offset: int) -> Path:
        measurements = self.root / name
        measurements.mkdir(parents=True, exist_ok=True)
        (measurements / "history.jsonl").write_text(
            f"{row(offset, sha)}\n", encoding="utf-8"
        )
        (measurements / "index.html").write_text(
            f"<html>{sha}</html>", encoding="utf-8"
        )
        return measurements

    def _spec(
        self, name: str, measurements: Path | None, attempts: int = 5
    ) -> PublishSpec:
        return PublishSpec(
            repo_url=str(self.origin),
            publish_dir=self.root / name,
            measurements=measurements,
            mirror_dir=self._mirror(),
            message=f"publish {name}",
            max_attempts=attempts,
        )

    def _published_rows(self) -> list[str]:
        clone = self.root / "inspect"
        if clone.exists():
            run(
                "fetch",
                "--quiet",
                str(self.origin),
                "refs/heads/benchmark-stats",
                cwd=clone,
            )
            run("reset", "--hard", "--quiet", "FETCH_HEAD", cwd=clone)
        else:
            clone.mkdir()
            run("init", "--quiet", cwd=clone)
            run(
                "fetch",
                "--quiet",
                str(self.origin),
                "refs/heads/benchmark-stats",
                cwd=clone,
            )
            run("reset", "--hard", "--quiet", "FETCH_HEAD", cwd=clone)
        text = (clone / "history.jsonl").read_text(encoding="utf-8")
        return [line for line in text.splitlines() if line.strip()]

    def _published_file(self, name: str) -> str:
        self._published_rows()
        return (self.root / "inspect" / name).read_text(encoding="utf-8")

    def test_sequential_publishers_keep_both_rows(self):
        first = self._spec("publish-a", self._measurements("measure-a", "runAAAA", 100))
        second = self._spec(
            "publish-b", self._measurements("measure-b", "runBBBB", 200)
        )
        self.assertEqual(0, publish(first))
        self.assertEqual(0, publish(second))
        shas = [
            row_text.split('"sha": "')[1].split('"')[0]
            for row_text in self._published_rows()
        ]
        self.assertIn("runAAAA", shas)
        self.assertIn("runBBBB", shas)
        self.assertEqual(SEED_ROWS + 2, len(shas))

    def test_overlapping_publishers_keep_both_rows(self):
        # A assembles, B publishes in that window, then A pushes: a plain
        # `--force` push here reverts B's row and A's row replaces it.
        first = self._spec("publish-a", self._measurements("measure-a", "runAAAA", 100))
        second = self._spec(
            "publish-b", self._measurements("measure-b", "runBBBB", 200)
        )
        rival: list[int] = []

        def publish_rival(attempt: int) -> None:
            # B publishes inside A's window, before A's own push.
            if attempt == 1:
                rival.append(publish(second))

        self.assertEqual(0, publish(first, before_push=publish_rival))
        self.assertEqual([0], rival)
        shas = [
            row_text.split('"sha": "')[1].split('"')[0]
            for row_text in self._published_rows()
        ]
        self.assertIn("runAAAA", shas)
        self.assertIn("runBBBB", shas)
        self.assertEqual(SEED_ROWS + 2, len(shas))

    def test_repeated_rejections_fail_loudly_instead_of_clobbering(self):
        spec = self._spec(
            "publish-a", self._measurements("measure-a", "runAAAA", 100), attempts=2
        )

        moved: list[int] = []

        def always_move(_attempt: int) -> None:
            mover = self.root / "mover"
            if not (mover / ".git").exists():
                mover.mkdir(exist_ok=True)
                run("init", "--quiet", cwd=mover)
                run("config", "user.name", "rival", cwd=mover)
                run("config", "user.email", "rival@example.com", cwd=mover)
            run(
                "fetch",
                "--quiet",
                str(self.origin),
                "refs/heads/benchmark-stats",
                cwd=mover,
            )
            run("reset", "--hard", "--quiet", "FETCH_HEAD", cwd=mover)
            moved.append(1)
            (mover / "rival.txt").write_text(f"rival {len(moved)}", encoding="utf-8")
            run("add", "-A", cwd=mover)
            run("commit", "--quiet", "-m", "rival", cwd=mover)
            run(
                "push",
                "--quiet",
                "--force",
                str(self.origin),
                "HEAD:refs/heads/benchmark-stats",
                cwd=mover,
            )

        with self.assertRaises(PublishError):
            publish(spec, before_push=always_move)

    def test_an_unchanged_site_does_not_push(self):
        measurements = self._measurements("measure-a", "runAAAA", 100)
        first = self._spec("publish-a", measurements)
        self.assertEqual(0, publish(first))
        tip = run("rev-parse", "refs/heads/benchmark-stats", cwd=self.origin).strip()
        second = self._spec("publish-a-again", measurements)
        self.assertEqual(0, publish(second))
        self.assertEqual(
            tip, run("rev-parse", "refs/heads/benchmark-stats", cwd=self.origin).strip()
        )

    def test_a_measurement_less_run_keeps_the_published_report(self):
        first = self._spec("publish-a", self._measurements("measure-a", "runAAAA", 100))
        self.assertEqual(0, publish(first))
        self.assertEqual(0, publish(self._spec("publish-mirror-only", None)))
        self.assertEqual("<html>runAAAA</html>", self._published_file("index.html"))
        self.assertEqual('{"schema": 3}', self._published_file("mirror/manifest.json"))
        self.assertEqual("{}", self._published_file("mirror/nested/owner.json"))
        shas = [
            row_text.split('"sha": "')[1].split('"')[0]
            for row_text in self._published_rows()
        ]
        self.assertIn("runAAAA", shas)

    def test_the_push_is_lease_protected(self):
        source = (Path(__file__).resolve().parent / "publish_bench_site.py").read_text()
        self.assertIn(
            "\"--force-with-lease=refs/heads/{spec.branch}:{expected or ''}\"", source
        )
        self.assertNotIn('"--force"', source)


if __name__ == "__main__":
    unittest.main()

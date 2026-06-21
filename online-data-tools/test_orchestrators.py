#!/usr/bin/env -S uv run --no-project --with pytest --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pytest"]
# ///
"""End-to-end tests for the YAML-delegated orchestrators:

- update_www.run(): given a fake online-data tree and an empty www tree,
  produces a populated www directory + an annotated online manifest.
- setup_www_worktree.setup(): branches both code paths (remote has branch /
  remote does not), exercised with a fake git runner.
- publish_branch.publish(): exercised end-to-end with a real git repo in
  tempdir, asserting commit + push (push goes to a `--bare` remote stand-in
  so no network involved).

No network access. No real sql.js download. Everything stubbed.
"""

from __future__ import annotations

import io
import json
import sqlite3
import subprocess
import sys
import zipfile
from pathlib import Path
from typing import Callable

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import publish_branch       # noqa: E402
import setup_www_worktree   # noqa: E402
import update_www           # noqa: E402


# --------------------------------------------------------------------------- #
# Fixtures: a minimal online-data tree + the real seed_mcu_to_vid.json
# --------------------------------------------------------------------------- #

@pytest.fixture
def workspace(tmp_path: Path) -> Path:
    """Mirror the bits of the real repo that update_www reads."""
    ws = tmp_path / "ws"
    tools = ws / "online-data-tools"
    static = tools / "www_static"
    tools.mkdir(parents=True)
    static.mkdir()
    # Copy the seed and the static-site assets from the real source tree.
    # We do not duplicate the data, just point update_www at the real files.
    for name in ("seed_mcu_to_vid.json",):
        (tools / name).write_bytes((HERE / name).read_bytes())
    for name in ("index.html", "app.js", "style.css"):
        (static / name).write_bytes((HERE / "www_static" / name).read_bytes())
    return ws


@pytest.fixture
def online_worktree(tmp_path: Path) -> Path:
    """Tiny online-data with everything build_sqlite needs."""
    ot = tmp_path / "online"
    data = ot / "data"
    data.mkdir(parents=True)
    (data / "usb-vid.json").write_text(json.dumps({
        "303a": {"vendor": "Espressif Systems",
                 "products": [["4002", "ESP32-S3"]]},
        "2e8a": {"vendor": "Raspberry Pi",
                 "products": [["000a", "Pico SDK CDC UART"]]},
    }), encoding="utf-8")
    (data / "pio-boards.json").write_text(json.dumps({
        "esp32-s3-devkitc-1": {
            "id": "esp32-s3-devkitc-1",
            "name": "Espressif ESP32-S3-DevKitC-1",
            "vendor": "Espressif", "mcu": "ESP32S3",
            "platform": "espressif32", "frameworks": ["arduino"],
            "url": "https://example.invalid",
        },
        "rpipico": {
            "id": "rpipico", "name": "Raspberry Pi Pico",
            "vendor": "Raspberry Pi", "mcu": "RP2040",
            "platform": "raspberrypi", "frameworks": ["arduino"],
            "url": "https://example.invalid",
        },
    }), encoding="utf-8")
    (data / "vendor_boards.json").write_text(json.dumps({}), encoding="utf-8")
    # Pre-existing online manifest from build_manifest.py.
    (ot / "manifest.json").write_text(json.dumps({
        "schema_version": "1.2",
        "generated_at":   "2026-06-20T04:17:00Z",
        "datasets": {
            "usb-vid":    {"url": "https://example/u.json", "entries": 2},
            "pio-boards": {"url": "https://example/p.json", "entries": 2},
        },
    }), encoding="utf-8")
    return ot


@pytest.fixture
def www_worktree(tmp_path: Path) -> Path:
    ww = tmp_path / "www"
    ww.mkdir()
    return ww


def _fake_sqljs_zip() -> bytes:
    """Build an in-memory zip mimicking the sql-js release archive layout."""
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr("sqljs-wasm-1.10.3/sql-wasm.js",   b"/* fake sql-wasm.js */")
        zf.writestr("sqljs-wasm-1.10.3/sql-wasm.wasm", b"\0asm\1\0\0\0fake")
    return buf.getvalue()


# --------------------------------------------------------------------------- #
# update_www.run — full pipeline
# --------------------------------------------------------------------------- #

def test_update_www_first_run_bootstraps_and_populates(
    workspace: Path, online_worktree: Path, www_worktree: Path,
) -> None:
    cfg = update_www.Config(
        workspace       = workspace,
        online_worktree = online_worktree,
        www_worktree    = www_worktree,
        today           = "2026-06-20",
        website_url     = "https://example.invalid/fbuild/",
        sqljs_zip_url   = "https://unused.invalid/sqljs.zip",
    )
    summary = update_www.run(cfg, fetch_sqljs=lambda _url: _fake_sqljs_zip())

    # mcu_to_vid was bootstrapped from the seed on the first run.
    assert summary["mcu_to_vid_bootstrapped"] is True
    assert (online_worktree / "data" / "mcu_to_vid.json").is_file()

    # The day's DB exists and is queryable.
    db = www_worktree / "2026-06-20.db"
    assert db.is_file() and db.stat().st_size > 0
    with sqlite3.connect(db) as conn:
        rows = conn.execute("SELECT vendor FROM usb_vendor WHERE vid = ?",
                            (int("303a", 16),)).fetchall()
    assert rows and rows[0][0] == "Espressif Systems"

    # Static assets + sql.js were staged.
    for name in ("index.html", "app.js", "style.css", "sql-wasm.js", "sql-wasm.wasm"):
        assert (www_worktree / name).is_file(), name

    # www manifest points at today's DB; no previous yet.
    m = json.loads((www_worktree / "manifest.json").read_text(encoding="utf-8"))
    assert m["current_db"] == "2026-06-20.db"
    assert "previous_db" not in m

    # Online manifest gained the website + databases blocks.
    om = json.loads((online_worktree / "manifest.json").read_text(encoding="utf-8"))
    assert om["website"]["url"] == "https://example.invalid/fbuild/"
    assert om["databases"]["current"] == "https://example.invalid/fbuild/2026-06-20.db"
    assert "previous" not in om["databases"]
    # Existing datasets entries are preserved verbatim.
    assert om["datasets"]["usb-vid"]["entries"] == 2


def test_update_www_second_run_keeps_existing_mcu_to_vid(
    workspace: Path, online_worktree: Path, www_worktree: Path,
) -> None:
    # Seed a custom mcu_to_vid.json on online-data — simulating it has already
    # been committed and curators have made edits.
    custom = [{"mcu_family": "ESP32S3", "vid": "303a",
               "score": 0.99, "reason": "custom curated"}]
    (online_worktree / "data" / "mcu_to_vid.json").write_text(
        json.dumps(custom), encoding="utf-8"
    )
    cfg = update_www.Config(
        workspace       = workspace,
        online_worktree = online_worktree,
        www_worktree    = www_worktree,
        today           = "2026-06-20",
        website_url     = "https://example.invalid/fbuild/",
        sqljs_zip_url   = "https://unused.invalid/sqljs.zip",
    )
    summary = update_www.run(cfg, fetch_sqljs=lambda _url: _fake_sqljs_zip())
    assert summary["mcu_to_vid_bootstrapped"] is False
    # The custom score made it into the DB.
    with sqlite3.connect(www_worktree / "2026-06-20.db") as conn:
        row = conn.execute(
            "SELECT score FROM mcu_to_vid WHERE mcu_family=? AND vid=?",
            ("ESP32S3", int("303a", 16)),
        ).fetchone()
    assert row[0] == pytest.approx(0.99)


def test_update_www_rotation_drops_old_dbs(
    workspace: Path, online_worktree: Path, www_worktree: Path,
) -> None:
    # Pre-seed www with three old DBs; the rotation step should keep only
    # today + the newest old one (i.e. one day-old).
    for d in ("2026-06-15.db", "2026-06-16.db", "2026-06-19.db"):
        (www_worktree / d).write_bytes(b"old")
    cfg = update_www.Config(
        workspace       = workspace,
        online_worktree = online_worktree,
        www_worktree    = www_worktree,
        today           = "2026-06-20",
        website_url     = "https://example.invalid/fbuild/",
        sqljs_zip_url   = "https://unused.invalid/sqljs.zip",
    )
    update_www.run(cfg, fetch_sqljs=lambda _url: _fake_sqljs_zip())
    survivors = {p.name for p in www_worktree.iterdir() if p.suffix == ".db"}
    assert survivors == {"2026-06-20.db", "2026-06-19.db"}, survivors


# --------------------------------------------------------------------------- #
# setup_www_worktree.setup — git runner stubbed
# --------------------------------------------------------------------------- #

class FakeGit:
    """Record git invocations + return canned outputs by command pattern."""
    def __init__(self, *, ls_remote_output: str = ""):
        self.calls: list[tuple[str, ...]] = []
        self._ls_remote_output = ls_remote_output

    def __call__(self, *args: str, cwd: Path | None = None, check: bool = True):
        self.calls.append(args)
        # `git ls-remote --heads <remote> <branch>` is the only call whose
        # output influences setup_www_worktree's branching.
        if args[:3] == ("git", "ls-remote", "--heads"):
            return subprocess.CompletedProcess(
                args=args, returncode=0,
                stdout=self._ls_remote_output, stderr="",
            )
        return subprocess.CompletedProcess(args=args, returncode=0, stdout="", stderr="")


def test_setup_branch_exists_on_remote(tmp_path: Path) -> None:
    fake = FakeGit(ls_remote_output="abc1234\trefs/heads/www\n")
    state = setup_www_worktree.setup(
        worktree=tmp_path / "www", branch="www", remote="origin", runner=fake,
    )
    assert state == "fetched"
    # We expect a fetch + worktree add, NOT an orphan checkout.
    assert ("git", "fetch", "origin", "www:www") in fake.calls
    assert any(c[:3] == ("git", "worktree", "add") for c in fake.calls)
    assert not any(c[:3] == ("git", "checkout", "--orphan") for c in fake.calls)


def test_setup_branch_missing_bootstraps_orphan(tmp_path: Path) -> None:
    fake = FakeGit(ls_remote_output="")  # remote knows nothing about www
    state = setup_www_worktree.setup(
        worktree=tmp_path / "www", branch="www", remote="origin", runner=fake,
    )
    assert state == "bootstrapped"
    assert any(c[:3] == ("git", "worktree", "add") for c in fake.calls)
    assert any(c[:3] == ("git", "checkout", "--orphan") for c in fake.calls)
    # No fetch attempt for a non-existent branch.
    assert not any(c[:3] == ("git", "fetch", "origin") for c in fake.calls)


# --------------------------------------------------------------------------- #
# publish_branch.publish — end-to-end with a real local git
# --------------------------------------------------------------------------- #

def _git_available() -> bool:
    try:
        subprocess.run(["git", "--version"], capture_output=True, check=True)
        return True
    except Exception:
        return False


pytestmark = pytest.mark.skipif(not _git_available(), reason="git not on PATH")


def _init_repo_with_remote(tmp_path: Path) -> tuple[Path, Path]:
    remote = tmp_path / "remote.git"
    work   = tmp_path / "work"
    subprocess.run(["git", "init", "--bare", str(remote)], check=True, capture_output=True)
    subprocess.run(["git", "init", str(work)], check=True, capture_output=True)
    subprocess.run(["git", "-C", str(work), "config", "user.email", "t@t.invalid"], check=True)
    subprocess.run(["git", "-C", str(work), "config", "user.name", "t"], check=True)
    subprocess.run(["git", "-C", str(work), "remote", "add", "origin", str(remote)], check=True)
    return work, remote


def test_publish_no_changes_is_noop(tmp_path: Path) -> None:
    work, _remote = _init_repo_with_remote(tmp_path)
    # Make initial commit so HEAD exists.
    (work / "seed.txt").write_text("x", encoding="utf-8")
    subprocess.run(["git", "-C", str(work), "add", "-A"], check=True)
    subprocess.run(["git", "-C", str(work), "commit", "-m", "seed"], check=True)
    out = publish_branch.publish(
        worktree=work, remote="origin", branch="main",
        message="noop", history_limit=200,
    )
    assert out["changed"] is False


def test_publish_first_push_falls_back_to_plain(tmp_path: Path) -> None:
    work, remote = _init_repo_with_remote(tmp_path)
    (work / "hello.txt").write_text("hi", encoding="utf-8")
    out = publish_branch.publish(
        worktree=work, remote="origin", branch="main",
        message="bootstrap", history_limit=200,
    )
    assert out["changed"] is True
    assert out["push"] == "plain"
    # Bare remote now has the branch.
    cp = subprocess.run(
        ["git", "-C", str(remote), "rev-parse", "refs/heads/main"],
        capture_output=True, text=True, check=True,
    )
    assert cp.stdout.strip()


def test_publish_second_push_uses_force_with_lease(tmp_path: Path) -> None:
    work, _remote = _init_repo_with_remote(tmp_path)
    (work / "a.txt").write_text("a", encoding="utf-8")
    publish_branch.publish(
        worktree=work, remote="origin", branch="main",
        message="first", history_limit=200,
    )
    (work / "b.txt").write_text("b", encoding="utf-8")
    out = publish_branch.publish(
        worktree=work, remote="origin", branch="main",
        message="second", history_limit=200,
    )
    assert out["changed"] is True
    assert out["push"] == "force-with-lease"


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))

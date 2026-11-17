#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["boto3>=1.36", "httpx>=0.27"]
# ///
"""Rotate DO droplets to work around PlatformIO's per-IP daily download cap.

The mirror uploader runs on a droplet; `dl.registry.platformio.org` caps each
source IP at roughly 36 GB / 24 h ("Download limit exceeded. Try again in 24
hours"). The catalog is ~102 GB, so one IP can never finish it. This supervisor
keeps the job moving: when the droplet reports its quota as exhausted (the
uploader writes `/var/lib/fbuild-mirror/quota_until`), it snapshots the SQLite
manifest to Spaces, destroys the capped droplet, creates a fresh one in the same
region (new IP = fresh quota), restores the manifest and restarts the service.

Terminates only when the manifest has no pending/error rows, then snapshots the
final manifest, destroys the droplet and writes a summary to the status file.

The bucket's `_state/manifest.db` is the source of truth (FastLED/fbuild#2040).
A fresh droplet is always restored from it, never from the local work copy; a
local copy that has progress the bucket lacks stops the run before a droplet is
created, and a droplet snapshot that is behind the bucket is never uploaded.
Every write of `_state/manifest.db` is conditional on the ETag read before it
(If-Match), so a concurrent writer makes the upload fail instead of being
overwritten. When the registry refuses the droplet's client (any 403; see
`mirror.RegistryRefused`) the supervisor stops with exit 3 rather than rotating.

Usage:
    uv run --no-project --script ci/mirror/rotate.py                     # run the supervisor (long-lived)
    uv run --no-project --script ci/mirror/rotate.py --status            # print the recorded state, no changes
    uv run --no-project --script ci/mirror/rotate.py --print-user-agent  # the UA the droplet will send
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import sqlite3
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import mirror  # sibling script: ci/mirror/mirror.py

DROPLET_NAME = "fbuild-mirror"
SIZE = "s-1vcpu-1gb"
REGION = "sfo3"
IMAGE = "ubuntu-24-04-x64"
# The DigitalOcean account's SSH key id (`doctl compute ssh-key list`) and the
# matching private key. Account-specific, so never committed.
SSH_KEY_ID = os.environ.get("FBUILD_MIRROR_SSH_KEY_ID", "")
SSH_KEY_PATH = Path(
    os.environ.get("FBUILD_MIRROR_SSH_KEY", str(Path.home() / ".ssh/id_rsa"))
)
SSH_OPTS = [
    "-o",
    "StrictHostKeyChecking=accept-new",
    "-o",
    "LogLevel=ERROR",
    "-o",
    "ConnectTimeout=15",
]

BUCKET = "fbuild"
BUCKET_REGION = "sfo3"
STATE_PREFIX = "_state/"
# Also read by ci/gen_mirror_manifest.py (its STATE_DB_KEY).
STATE_KEY = STATE_PREFIX + "manifest.db"
# Local supervisor state (status file, manifest snapshots); never in the repo.
LOCAL_DIR = Path(
    os.environ.get(
        "FBUILD_MIRROR_WORKDIR", str(Path.home() / ".local/state/fbuild-mirror")
    )
)
SCRIPT = Path(mirror.__file__).resolve()
CREDS = Path(
    os.environ.get(
        "FBUILD_MIRROR_CREDS", str(Path.home() / ".config/do-spaces/credentials.json")
    )
)
STATUS = LOCAL_DIR / "rotation-status.json"
WORK = LOCAL_DIR / "rotation-work"

MAX_ROTATIONS = 40


def log(msg: str) -> None:
    # The launcher redirects stdout into rotation.log; writing here as well would
    # duplicate every line.
    print(f"{time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())} {msg}", flush=True)


def run(cmd: list[str], *, check: bool = True, input_bytes: bytes | None = None) -> str:
    proc = subprocess.run(cmd, capture_output=True, input=input_bytes, timeout=900)
    if check and proc.returncode != 0:
        raise RuntimeError(
            f"{' '.join(cmd)} -> {proc.returncode}: {proc.stderr.decode()[:400]}"
        )
    return proc.stdout.decode()


def s3() -> Any:
    import boto3
    from botocore.client import Config

    creds = json.loads(CREDS.read_text())
    return boto3.client(
        "s3",
        region_name=BUCKET_REGION,
        endpoint_url=f"https://{BUCKET_REGION}.digitaloceanspaces.com",
        aws_access_key_id=creds["access_key"],
        aws_secret_access_key=creds["secret_key"],
        config=Config(retries={"max_attempts": 5, "mode": "standard"}),
    )


def ssh(ip: str, payload: str, *, timeout: int = 600) -> str:
    return run(
        [
            "ssh",
            "-i",
            str(SSH_KEY_PATH),
            "-T",
            *SSH_OPTS,
            f"root@{ip}",
            "bash -euo pipefail -s",
        ],
        input_bytes=payload.encode(),
    )


def droplets() -> list[dict[str, Any]]:
    raw = (
        run(["doctl", "compute", "droplet", "list", "--output", "json"], check=False)
        or "[]"
    )
    try:
        items = json.loads(raw)
    except json.JSONDecodeError:
        return []
    return [d for d in items if d.get("name") == DROPLET_NAME]


def public_ip(droplet: dict[str, Any]) -> str:
    for net in (droplet.get("networks") or {}).get("v4") or []:
        if net.get("type") == "public":
            return str(net.get("ip_address"))
    return ""


def create_droplet() -> dict[str, Any]:
    raw = run(
        [
            "doctl",
            "compute",
            "droplet",
            "create",
            DROPLET_NAME,
            "--size",
            SIZE,
            "--image",
            IMAGE,
            "--region",
            REGION,
            "--ssh-keys",
            SSH_KEY_ID,
            "--output",
            "json",
        ]
    )
    created = json.loads(raw)
    droplet = created[0] if isinstance(created, list) else created
    did = droplet["id"]
    log(f"created droplet {did}")
    for _ in range(60):
        time.sleep(10)
        got = run(
            ["doctl", "compute", "droplet", "get", str(did), "--output", "json"],
            check=False,
        )
        try:
            info = json.loads(got)
            info = info[0] if isinstance(info, list) else info
        except json.JSONDecodeError:
            continue
        if info.get("status") == "active" and public_ip(info):
            log(f"droplet {did} active at {public_ip(info)}")
            return info
    raise RuntimeError(f"droplet {did} never became reachable")


def wait_for_ssh(ip: str, timeout: int = 300) -> None:
    """`active` from the API does not mean sshd is listening; poll before giving up."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        probe_cmd = [
            "ssh",
            "-i",
            str(SSH_KEY_PATH),
            "-T",
            *SSH_OPTS,
            f"root@{ip}",
            "true",
        ]
        if subprocess.run(probe_cmd, capture_output=True, timeout=60).returncode == 0:
            log(f"ssh ready on {ip}")
            return
        time.sleep(10)
    raise RuntimeError(f"ssh never became ready on {ip} within {timeout}s")


def destroy_droplet(did: int | str) -> None:
    run(["doctl", "compute", "droplet", "delete", str(did), "--force"], check=False)
    log(f"destroyed droplet {did}")


@dataclass(frozen=True)
class ManifestSummary:
    """How far a state DB has got: `files` rows, `done` rows, newest `updated_at`."""

    rows: int
    done: int
    newest: int

    def ahead_of(self, other: ManifestSummary) -> list[str]:
        """The measures in which this copy has progress `other` lacks."""
        return [
            name
            for name in ("rows", "done", "newest")
            if getattr(self, name) > getattr(other, name)
        ]


class ManifestConflict(RuntimeError):
    """Two copies of the state DB disagree in a way a human must resolve."""


@dataclass(frozen=True)
class BucketCopy:
    """The bucket's state DB as downloaded: its progress and the ETag read."""

    summary: ManifestSummary
    etag: str


def summarize_manifest(path: Path) -> ManifestSummary:
    conn = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    try:
        rows, done, newest = conn.execute(
            "select count(*), coalesce(sum(state = 'done'), 0),"
            " coalesce(max(updated_at), 0) from files"
        ).fetchone()
    finally:
        conn.close()
    return ManifestSummary(rows, done, newest)


def check_restore_sources(
    local: ManifestSummary | None, bucket: ManifestSummary | None
) -> bool:
    """Whether to restore the bucket copy (`False`: first-ever run, start empty).

    The bucket is the source of truth. A local copy is never restored; it only
    stops the run when it holds progress the bucket lacks (a snapshot that never
    reached the bucket), because restoring the bucket copy would then lose it and
    the next snapshot would make that permanent."""
    if bucket is None:
        if local is not None:
            raise ManifestConflict(
                f"the bucket has no {STATE_KEY} but a local copy exists ({local});"
                " upload it deliberately or remove it before rotating"
            )
        return False
    if local is not None:
        ahead = local.ahead_of(bucket)
        if ahead:
            raise ManifestConflict(
                f"the local manifest copy ({local}) is ahead of the bucket's"
                f" ({bucket}) in {', '.join(ahead)}; a snapshot probably failed to"
                " upload. Inspect both and upload the right one deliberately."
            )
    return True


def check_snapshot_upload(
    snapshot: ManifestSummary, bucket: ManifestSummary | None
) -> None:
    """Refuse to overwrite the bucket copy with one that is behind it."""
    if bucket is None:
        return
    behind = bucket.ahead_of(snapshot)
    if behind:
        raise ManifestConflict(
            f"refusing to upload a droplet snapshot ({snapshot}): the bucket copy"
            f" ({bucket}) is ahead in {', '.join(behind)}"
        )


def _error_code(exc: Any) -> str:
    return str(getattr(exc, "response", {}).get("Error", {}).get("Code", ""))


def download_bucket_manifest(dest: Path) -> BucketCopy | None:
    """Fetch the bucket's state DB to `dest`; `None` only when there is none.

    Any other failure raises: treating a transient error as "no snapshot" would
    start an empty manifest whose first snapshot overwrites the real one. The
    body and the ETag come from one GET, so the ETag describes these bytes."""
    from botocore.exceptions import ClientError

    try:
        obj = s3().get_object(Bucket=BUCKET, Key=STATE_KEY)
    except ClientError as exc:
        if _error_code(exc) in ("404", "NoSuchKey"):
            return None
        raise
    with dest.open("wb") as fh:
        shutil.copyfileobj(obj["Body"], fh)
    return BucketCopy(summarize_manifest(dest), obj["ETag"])


def put_bucket_manifest(path: Path, read: BucketCopy | None) -> None:
    """Upload `path` as the bucket's state DB, but only over the copy `read`
    (If-Match on its ETag), or only if there still is none (If-None-Match)."""
    from botocore.exceptions import ClientError

    condition = {"IfMatch": read.etag} if read else {"IfNoneMatch": "*"}
    try:
        s3().put_object(
            Bucket=BUCKET,
            Key=STATE_KEY,
            Body=path.read_bytes(),
            ContentType="binary/octet-stream",
            **condition,
        )
    except ClientError as exc:
        if _error_code(exc) in (
            "PreconditionFailed",
            "412",
            "ConditionalRequestConflict",
        ):
            raise ManifestConflict(
                f"the bucket's {STATE_KEY} changed since it was read"
                f" ({condition}); not overwriting it. {path} holds this run's copy."
            ) from exc
        raise


def backup_bucket_manifest(read: BucketCopy) -> str:
    """Server-side copy of the state DB, only if it is still the copy `read`."""
    key = f"{STATE_KEY}.bak-{time.strftime('%Y%m%dT%H%M%SZ', time.gmtime())}"
    s3().copy_object(
        Bucket=BUCKET,
        Key=key,
        CopySource={"Bucket": BUCKET, "Key": STATE_KEY},
        CopySourceIfMatch=read.etag,
    )
    return key


def plan_restore() -> Path | None:
    """The DB to restore onto a fresh droplet (the bucket copy), or `None`."""
    WORK.mkdir(parents=True, exist_ok=True)
    fetched = WORK / "manifest.bucket.db"
    read = download_bucket_manifest(fetched)
    bucket = read.summary if read else None
    local_path = WORK / "manifest.db"
    local = None
    if local_path.exists() and local_path.stat().st_size:
        local = summarize_manifest(local_path)
    if not check_restore_sources(local, bucket):
        log("no manifest in the bucket; starting a fresh catalog")
        return None
    if local is not None and bucket is not None and bucket.ahead_of(local):
        log(
            f"ignoring the stale local manifest ({local}); restoring the bucket's ({bucket})"
        )
    return fetched


def snapshot_manifest(ip: str) -> Path | None:
    """Consistent-copy the SQLite manifest off the box and push it to Spaces."""
    WORK.mkdir(parents=True, exist_ok=True)
    local = WORK / "manifest.db"
    pulled = WORK / "manifest.snapshot.db"
    try:
        # sqlite3's backup API gives a consistent copy even with the uploader writing.
        ssh(
            ip,
            "python3 - <<'PY'\n"
            "import sqlite3\n"
            "src = sqlite3.connect('/var/lib/fbuild-mirror/manifest.db')\n"
            "dst = sqlite3.connect('/tmp/manifest-snapshot.db')\n"
            "src.backup(dst)\n"
            "dst.close(); src.close()\n"
            "PY\n",
            timeout=300,
        )
    except Exception as exc:  # a dying box may refuse; keep the last good copy
        log(f"WARNING manifest snapshot failed: {exc}")
        return None
    try:
        run(
            [
                "scp",
                "-i",
                str(SSH_KEY_PATH),
                *SSH_OPTS[2:],
                f"root@{ip}:/tmp/manifest-snapshot.db",
                str(pulled),
            ]
        )
    except Exception as exc:
        log(f"WARNING manifest download failed: {exc}")
        return None
    if pulled.stat().st_size == 0:
        log("WARNING manifest snapshot was empty")
        return None
    snapshot = summarize_manifest(pulled)
    read = download_bucket_manifest(WORK / "manifest.bucket.db")
    check_snapshot_upload(snapshot, read.summary if read else None)
    put_bucket_manifest(pulled, read)
    pulled.replace(local)
    log(f"manifest snapshot uploaded ({local.stat().st_size} bytes, {snapshot})")
    return local


def restore_manifest(ip: str, source: Path) -> None:
    raw = source.read_bytes()
    run(
        [
            "ssh",
            "-i",
            str(SSH_KEY_PATH),
            "-T",
            *SSH_OPTS,
            f"root@{ip}",
            "cat > /var/lib/fbuild-mirror/manifest.db",
        ],
        input_bytes=raw,
    )
    log(f"manifest restored on {ip} ({len(raw)} bytes from {source.name})")


def b64_payload(data: bytes, dest: str) -> str:
    import base64

    encoded = base64.b64encode(data).decode()
    return f"printf '%s' '{encoded}' | base64 -d > {dest}\n"


def provision(ip: str, source: Path | None, user_agent: str) -> None:
    setup = b64_payload(SCRIPT.read_bytes(), "/tmp/mirror.py")
    creds = b64_payload(CREDS.read_bytes(), "/etc/fbuild-mirror/spaces.json")
    unit = """[Unit]
Description=fbuild open-source package mirror (PlatformIO registry -> Spaces bucket fbuild)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
Environment=PATH=/root/.local/bin:/usr/local/bin:/usr/bin:/bin
Environment=FBUILD_MIRROR_STATE=/var/lib/fbuild-mirror
Environment=FBUILD_MIRROR_CREDS=/etc/fbuild-mirror/spaces.json
Environment=FBUILD_MIRROR_BUCKET=fbuild
Environment=FBUILD_MIRROR_REGION=sfo3
Environment=FBUILD_MIRROR_CONCURRENCY=6
Environment=UV_HTTP_TIMEOUT=120
Environment="{ua_env}={user_agent}"
ExecStart=/root/.local/bin/uv run --quiet /opt/fbuild-mirror/mirror.py run
Restart=always
RestartSec=15
# mirror.py exits {refused} when the registry refuses the client; restarting
# cannot fix that, and rotate.py stops on the marker file instead.
RestartPreventExitStatus={refused}
StandardOutput=append:/var/log/fbuild-mirror.log
StandardError=append:/var/log/fbuild-mirror.log

[Install]
WantedBy=multi-user.target
""".format(ua_env=mirror.UA_ENV, user_agent=user_agent, refused=mirror.EXIT_REFUSED)
    payload = (
        "set -euo pipefail\n"
        "export DEBIAN_FRONTEND=noninteractive\n"
        "install -d -m 755 /opt/fbuild-mirror /var/lib/fbuild-mirror\n"
        "install -d -m 700 /etc/fbuild-mirror\n"
        + setup
        + creds
        + "chmod 600 /etc/fbuild-mirror/spaces.json\n"
        "chmod 644 /tmp/mirror.py\n"
        "mv /tmp/mirror.py /opt/fbuild-mirror/mirror.py\n"
        "rm -f /var/lib/fbuild-mirror/quota_until\n"
        "if ! command -v uv >/dev/null; then curl -LsSf https://astral.sh/uv/install.sh | sh -s -- -q; fi\n"
        "if ! swapon --show | grep -q swap; then fallocate -l 2G /swapfile && chmod 600 /swapfile"
        " && mkswap -q /swapfile && swapon /swapfile && echo '/swapfile none swap sw 0 0' >> /etc/fstab; fi\n"
        f"cat > /etc/systemd/system/fbuild-mirror.service <<'UNIT'\n{unit}UNIT\n"
        "systemctl daemon-reload\n"
    )
    ssh(ip, payload)
    # Restore the manifest BEFORE the service starts so the two never race on the DB.
    if source is not None:
        restore_manifest(ip, source)
    ssh(
        ip,
        "systemctl enable fbuild-mirror.service\n"
        "systemctl restart fbuild-mirror.service\n"
        "sleep 12\n"
        "systemctl is-active fbuild-mirror.service\n",
    )


PROBE = r"""
python3 - <<'PY'
import json, os, sqlite3
db = "/var/lib/fbuild-mirror/manifest.db"
out = {"quota": os.path.exists("/var/lib/fbuild-mirror/quota_until")}
if os.path.exists("/var/lib/fbuild-mirror/registry_refused"):
    out["refused"] = open("/var/lib/fbuild-mirror/registry_refused").read()[:400]
try:
    c = sqlite3.connect(f"file:{db}?mode=ro", uri=True, timeout=20)
    out["state"] = dict(c.execute("select state, count(*) from files group by state").fetchall())
    out["bytes_done"] = c.execute("select coalesce(sum(size),0) from files where state='done'").fetchone()[0]
except Exception as exc:
    out["error"] = str(exc)
print(json.dumps(out))
PY
"""


def probe(ip: str) -> dict[str, Any] | None:
    try:
        raw = ssh(ip, PROBE, timeout=120).strip().splitlines()
    except Exception:  # unreachable box == rotate
        return None
    for line in reversed(raw):
        line = line.strip()
        if line.startswith("{"):
            try:
                return json.loads(line)
            except json.JSONDecodeError:
                return None
    return None


def write_status(**fields: Any) -> None:
    STATUS.write_text(json.dumps(fields, indent=1, sort_keys=True))


def _start_droplet(user_agent: str) -> tuple[int, str] | None:
    """Create and provision a fresh droplet; `None` if it had to be destroyed."""
    # Decide what to restore before paying for a droplet: a conflict stops here.
    source = plan_restore()
    drip = create_droplet()
    did, ip = int(drip["id"]), public_ip(drip)
    for attempt in range(2):
        try:
            wait_for_ssh(ip)
            # Restore the manifest whenever a snapshot exists (i.e. not the very
            # first droplet), then resume where the last box stopped.
            provision(ip, source, user_agent)
            return did, ip
        except Exception as exc:
            log(f"provision attempt {attempt + 1} failed on droplet {did}: {exc}")
            time.sleep(30)
    log(f"giving up on droplet {did}; rotating")
    destroy_droplet(did)
    return None


def _record_progress(
    ip: str, did: int, rotations: int, info: dict[str, Any]
) -> tuple[int, int, int]:
    states = info.get("state") or {}
    done = int(states.get("done", 0))
    pending = int(states.get("pending", 0))
    error = int(states.get("error", 0))
    write_status(
        ip=ip,
        droplet_id=did,
        rotations=rotations,
        phase="monitoring",
        done=done,
        pending=pending,
        error=error,
        bytes_done=info.get("bytes_done", 0),
        quota=bool(info.get("quota")),
    )
    log(
        f"progress done={done} pending={pending} error={error} {(info.get('bytes_done') or 0) / 1e9:.2f}GB"
    )
    return done, pending, error


def stall_action(unchanged: int, restarted: bool) -> str | None:
    """`"restart"` / `"rotate"` once progress has stalled for long enough."""
    if unchanged >= 3 and not restarted:
        return "restart"
    if unchanged >= 6:
        return "rotate"
    return None


def _monitor(ip: str, did: int, rotations: int) -> str | None:
    """Watch one droplet: `"complete"`, `"capped"` (rotate it), `"refused"`
    (stop: a new IP will not help), or `None` (still healthy)."""
    last_seen: tuple[int, int, int] | None = None
    unchanged = 0
    restarted = False
    for _ in range(600):
        info = probe(ip)
        if info is None:
            log("droplet unreachable; rotating")
            return "capped"
        done, pending, error = now_seen = _record_progress(ip, did, rotations, info)
        if pending == 0 and error == 0 and done > 0:
            log("catalog fully mirrored — finishing")
            snapshot_manifest(ip)
            destroy_droplet(did)
            write_status(phase="complete", done=done, ip=ip)
            return "complete"
        if info.get("refused"):
            log(f"REGISTRY REFUSED THE MIRROR CLIENT; stopping: {info['refused']}")
            return "refused"
        if info.get("quota"):
            log("quota exhausted on this droplet; rotating")
            return "capped"
        unchanged = unchanged + 1 if now_seen == last_seen else 0
        last_seen = now_seen
        action = stall_action(unchanged, restarted)
        if action == "restart":
            # A process parked in a long sleep looks identical to a healthy one
            # from the outside; bounce the unit before spending a rotation.
            log("no progress for 3 probes; restarting the uploader service")
            ssh(ip, "systemctl restart fbuild-mirror.service\n", timeout=120)
            restarted = True
            unchanged = 0
        elif action == "rotate":
            log("still no progress after a restart; rotating")
            return "capped"
        time.sleep(60)
    return None


def _preflight() -> str | None:
    """Handle the read-only flags; the UA to run with, or `None` when done."""
    ap = argparse.ArgumentParser()
    ap.add_argument("--status", action="store_true")
    ap.add_argument("--print-user-agent", action="store_true")
    args = ap.parse_args()
    if args.status:
        print(STATUS.read_text() if STATUS.exists() else "no status yet")
        return None
    user_agent = mirror.resolve_user_agent(os.environ)
    if args.print_user_agent:
        print(user_agent)
        return None
    if not SSH_KEY_ID:
        raise SystemExit("set FBUILD_MIRROR_SSH_KEY_ID (doctl compute ssh-key list)")
    return user_agent


def main() -> int:
    user_agent = _preflight()
    if user_agent is None:
        return 0
    LOCAL_DIR.mkdir(parents=True, exist_ok=True)
    WORK.mkdir(parents=True, exist_ok=True)
    rotations = 0
    while rotations <= MAX_ROTATIONS:
        live = droplets()
        if live:
            did, ip = int(live[0]["id"]), public_ip(live[0])
            if not ip:
                log(f"droplet {did} has no public IP yet; waiting")
                time.sleep(20)
                continue
        else:
            started = _start_droplet(user_agent)
            if started is None:
                rotations += 1
                continue
            did, ip = started
        write_status(ip=ip, droplet_id=did, rotations=rotations, phase="monitoring")
        log(f"monitoring droplet {did} at {ip}")
        outcome = _monitor(ip, did, rotations)
        if outcome == "complete":
            return 0
        if outcome in ("capped", "refused"):
            snapshot_manifest(ip)
            destroy_droplet(did)
            if outcome == "refused":
                write_status(phase="refused", rotations=rotations)
                return mirror.EXIT_REFUSED
            rotations += 1
            write_status(phase="rotating", rotations=rotations)
        # None: long-running, healthy box: keep monitoring.
    log("rotation budget exhausted")
    return 1


if __name__ == "__main__":
    sys.exit(main())

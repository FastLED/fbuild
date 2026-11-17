#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["boto3>=1.34", "httpx>=0.27"]
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

Usage:
    uv run rotate.py            # run the supervisor (long-lived)
    uv run rotate.py --status   # print the recorded state, no changes
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

import boto3
from botocore.client import Config

DROPLET_NAME = "fbuild-mirror"
SIZE = "s-1vcpu-1gb"
REGION = "sfo3"
IMAGE = "ubuntu-24-04-x64"
# The DigitalOcean account's SSH key id (`doctl compute ssh-key list`) and the
# matching private key. Account-specific, so never committed.
SSH_KEY_ID = os.environ.get("FBUILD_MIRROR_SSH_KEY_ID", "")
SSH_KEY_PATH = Path(os.environ.get("FBUILD_MIRROR_SSH_KEY", str(Path.home() / ".ssh/id_rsa")))
SSH_OPTS = ["-o", "StrictHostKeyChecking=accept-new", "-o", "LogLevel=ERROR", "-o", "ConnectTimeout=15"]

BUCKET = "fbuild"
BUCKET_REGION = "sfo3"
STATE_PREFIX = "_state/"
# Local supervisor state (status file, manifest snapshots); never in the repo.
LOCAL_DIR = Path(
    os.environ.get("FBUILD_MIRROR_WORKDIR", str(Path.home() / ".local/state/fbuild-mirror"))
)
SCRIPT = Path(__file__).resolve().with_name("mirror.py")
CREDS = Path(
    os.environ.get("FBUILD_MIRROR_CREDS", str(Path.home() / ".config/do-spaces/credentials.json"))
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
        raise RuntimeError(f"{' '.join(cmd)} -> {proc.returncode}: {proc.stderr.decode()[:400]}")
    return proc.stdout.decode()


def s3() -> Any:
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
        ["ssh", "-i", str(SSH_KEY_PATH), "-T", *SSH_OPTS, f"root@{ip}", "bash -euo pipefail -s"],
        input_bytes=payload.encode(),
    )


def droplets() -> list[dict[str, Any]]:
    raw = run(["doctl", "compute", "droplet", "list", "--output", "json"], check=False) or "[]"
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
            "doctl", "compute", "droplet", "create", DROPLET_NAME,
            "--size", SIZE, "--image", IMAGE, "--region", REGION,
            "--ssh-keys", SSH_KEY_ID, "--output", "json",
        ]
    )
    created = json.loads(raw)
    droplet = created[0] if isinstance(created, list) else created
    did = droplet["id"]
    log(f"created droplet {did}")
    for _ in range(60):
        time.sleep(10)
        got = run(["doctl", "compute", "droplet", "get", str(did), "--output", "json"], check=False)
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
        probe_cmd = ["ssh", "-i", str(SSH_KEY_PATH), "-T", *SSH_OPTS, f"root@{ip}", "true"]
        if subprocess.run(probe_cmd, capture_output=True, timeout=60).returncode == 0:
            log(f"ssh ready on {ip}")
            return
        time.sleep(10)
    raise RuntimeError(f"ssh never became ready on {ip} within {timeout}s")


def destroy_droplet(did: int | str) -> None:
    run(["doctl", "compute", "droplet", "delete", str(did), "--force"], check=False)
    log(f"destroyed droplet {did}")


def snapshot_manifest(ip: str) -> Path | None:
    """Consistent-copy the SQLite manifest off the box and push it to Spaces."""
    WORK.mkdir(parents=True, exist_ok=True)
    local = WORK / "manifest.db"
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
    except Exception as exc:  # noqa: BLE001 - a dying box may refuse; keep the last good copy
        log(f"WARNING manifest snapshot failed: {exc}")
        return None
    try:
        run(["scp", "-i", str(SSH_KEY_PATH), *SSH_OPTS[2:], f"root@{ip}:/tmp/manifest-snapshot.db", str(local)])
    except Exception as exc:  # noqa: BLE001
        log(f"WARNING manifest download failed: {exc}")
        return None
    if local.stat().st_size == 0:
        log("WARNING manifest snapshot was empty")
        return None
    s3().upload_file(str(local), BUCKET, STATE_PREFIX + "manifest.db")
    log(f"manifest snapshot uploaded ({local.stat().st_size} bytes)")
    return local


def restore_manifest(ip: str) -> bool:
    obj = WORK / "manifest.db"
    if not obj.exists() or obj.stat().st_size == 0:
        try:
            s3().download_file(BUCKET, STATE_PREFIX + "manifest.db", str(obj))
        except Exception as exc:  # noqa: BLE001 - first-ever run has no snapshot
            log(f"no manifest snapshot to restore ({exc})")
            return False
    payload = b64_payload(SCRIPT.read_bytes(), "/tmp/mirror.py")
    ssh(ip, payload)
    ssh(
        ip,
        "install -d -m 755 /opt/fbuild-mirror /var/lib/fbuild-mirror\n"
        "install -d -m 700 /etc/fbuild-mirror\n"
        "mv /tmp/mirror.py /opt/fbuild-mirror/mirror.py\n",
    )
    raw = obj.read_bytes()
    run(
        ["ssh", "-i", str(SSH_KEY_PATH), "-T", *SSH_OPTS, f"root@{ip}",
         "cat > /var/lib/fbuild-mirror/manifest.db"],
        input_bytes=raw,
    )
    log(f"manifest restored on {ip} ({len(raw)} bytes)")
    return True


def b64_payload(data: bytes, dest: str) -> str:
    import base64

    encoded = base64.b64encode(data).decode()
    return f"printf '%s' '{encoded}' | base64 -d > {dest}\n"


def provision(ip: str, restore: bool) -> None:
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
ExecStart=/root/.local/bin/uv run --quiet /opt/fbuild-mirror/mirror.py run
Restart=always
RestartSec=15
StandardOutput=append:/var/log/fbuild-mirror.log
StandardError=append:/var/log/fbuild-mirror.log

[Install]
WantedBy=multi-user.target
"""
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
    if restore:
        restore_manifest(ip)
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
    except Exception:  # noqa: BLE001 - unreachable box == rotate
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


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--status", action="store_true")
    args = ap.parse_args()
    if args.status:
        print(STATUS.read_text() if STATUS.exists() else "no status yet")
        return 0

    LOCAL_DIR.mkdir(parents=True, exist_ok=True)
    WORK.mkdir(parents=True, exist_ok=True)
    rotations = 0
    while rotations <= MAX_ROTATIONS:
        live = droplets()
        first = not live
        if live:
            drip = live[0]
            did, ip = int(drip["id"]), public_ip(drip)
            if not ip:
                log(f"droplet {did} has no public IP yet; waiting")
                time.sleep(20)
                continue
        else:
            drip = create_droplet()
            did, ip = int(drip["id"]), public_ip(drip)
            restore = (WORK / "manifest.db").exists() or bool(
                s3()
                .list_objects_v2(Bucket=BUCKET, Prefix=STATE_PREFIX + "manifest.db", MaxKeys=1)
                .get("Contents")
            )
            provisioned = False
            for attempt in range(2):
                try:
                    wait_for_ssh(ip)
                    # Restore the manifest whenever a snapshot exists (i.e. not the very
                    # first droplet), then resume where the last box stopped.
                    provision(ip, restore=restore)
                    provisioned = True
                    break
                except Exception as exc:  # noqa: BLE001
                    log(f"provision attempt {attempt + 1} failed on droplet {did}: {exc}")
                    time.sleep(30)
            if not provisioned:
                log(f"giving up on droplet {did}; rotating")
                destroy_droplet(did)
                rotations += 1
                continue
        write_status(ip=ip, droplet_id=did, rotations=rotations, phase="monitoring")
        log(f"monitoring droplet {did} at {ip}")

        capped = False
        last_seen: tuple[int, int, int] | None = None
        unchanged = 0
        restarted = False
        for _ in range(600):
            info = probe(ip)
            if info is None:
                log("droplet unreachable; rotating")
                capped = True
                break
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
            log(f"progress done={done} pending={pending} error={error} "
                f"{(info.get('bytes_done') or 0) / 1e9:.2f}GB")
            if pending == 0 and error == 0 and done > 0:
                log("catalog fully mirrored — finishing")
                snapshot_manifest(ip)
                destroy_droplet(did)
                write_status(phase="complete", done=done, ip=ip)
                return 0
            if info.get("quota"):
                log("quota exhausted on this droplet; rotating")
                capped = True
                break
            now_seen = (done, pending, error)
            unchanged = unchanged + 1 if now_seen == last_seen else 0
            last_seen = now_seen
            if unchanged >= 3 and not restarted:
                # A process parked in a long sleep looks identical to a healthy one
                # from the outside; bounce the unit before spending a rotation.
                log("no progress for 3 probes; restarting the uploader service")
                ssh(ip, "systemctl restart fbuild-mirror.service\n", timeout=120)
                restarted = True
                unchanged = 0
            elif unchanged >= 6:
                log("still no progress after a restart; rotating")
                capped = True
                break
            time.sleep(60)

        if capped:
            snapshot_manifest(ip)
            destroy_droplet(did)
            rotations += 1
            write_status(phase="rotating", rotations=rotations)
            continue
        # Long-running, healthy box: keep monitoring.
    log("rotation budget exhausted")
    return 1


if __name__ == "__main__":
    sys.exit(main())

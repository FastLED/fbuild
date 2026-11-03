# /// script
# requires-python = ">=3.11"
# dependencies = ["zstandard"]
# ///
"""Manually build a pilot framework and package its engine-exported exact plan.

uv run --no-project --with zstandard python ci/build_prebuilt.py --help
Upload commands are printed, never executed. Withdrawal MUST delete the bucket
object (and its sidecar), then regenerate the Index. Removing only an Index row
leaves stale clients able to fetch the withdrawn artifact for up to 24 hours.
"""
from __future__ import annotations
import argparse
import hashlib
import io
import json
import shlex
import subprocess
import tarfile
import tempfile
import tomllib
from pathlib import Path
import zstandard

ROOT = Path(__file__).resolve().parent.parent

def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()

def add_file(archive, name, data):
    info = tarfile.TarInfo(name)
    info.size = len(data)
    info.mode = 0o644
    info.mtime = 0
    archive.addfile(info, io.BytesIO(data))

def package(metadata, output, source):
    """Package only an engine-produced plan, without deriving compiler inputs."""
    plan = metadata["plan"]
    fp = hashlib.sha256(canonical(plan)).hexdigest()
    if fp != metadata["fp"] or plan["abi_version"] != 1:
        raise ValueError("engine fingerprint or ABI mismatch")
    files = {}
    for argv in plan["argv"]:
        obj = Path(argv[argv.index("-o") + 1])
        # The engine's -o may be relative to the project compile cwd; the
        # exported core directory supplies the actual local object location.
        path = Path(metadata["core_dir"]) / obj.name
        files["obj/" + path.name] = path.read_bytes()
    for number, include in enumerate(metadata["include_dirs"]):
        directory = Path(include)
        if directory.is_dir():
            for header in sorted(directory.rglob("*")):
                if header.is_file() and header.suffix.lower() in {".h", ".hpp", ".inc"}:
                    files[f"include/{number}/" + header.relative_to(directory).as_posix()] = header.read_bytes()
    if not files or not any(p.startswith("include/") for p in files):
        raise ValueError("artifact must contain framework objects and headers")
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    manifest = {"fp": fp, "abi_version": 1, "inputs": plan,
        "builder_version": "fbuild-prebuilt-v1", "fbuild_version": version, "source": source,
        "files": {name: hashlib.sha256(data).hexdigest() for name, data in sorted(files.items())}}
    output.mkdir(parents=True, exist_ok=True)
    artifact = output / "prebuilt.tar.zst"
    with artifact.open("wb") as raw:
        with zstandard.ZstdCompressor(level=19, threads=-1).stream_writer(raw) as compressed:
            with tarfile.open(fileobj=compressed, mode="w|") as archive:
                add_file(archive, "fbuild-prebuilt.json", canonical(manifest))
                for name, data in sorted(files.items()):
                    add_file(archive, name, data)
    data = artifact.read_bytes()
    optimization = next((a[1:] for a in reversed(plan["argv"][0]) if a.startswith("-O")), "Os")
    sidecar = {"platform": {"os": "none", "arch": plan["mcu"],
        "abi": plan["toolchain"]["id"] + "@" + plan["toolchain"]["version"]},
        "variant": {"optimization": optimization, "flavor": "fp-" + fp[:16]},
        "source": source, "fp": fp, "size_bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
    sidecar_path = artifact.with_name(artifact.name + ".release.json")
    sidecar_path.write_bytes(canonical(sidecar) + b"\n")
    key = f"prebuilt/arduino-esp32/{plan['framework']['version']}/{plan['mcu']}/fp-{fp[:16]}/{artifact.name}"
    return artifact, sidecar_path, key

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", type=Path, required=True)
    parser.add_argument("--board", choices=["esp32dev", "esp32-s3-devkitc-1"], required=True)
    parser.add_argument("--environment", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--source-repo", required=True)
    parser.add_argument("--source-ref", required=True, help="resolved framework source commit SHA")
    parser.add_argument("--fbuild", type=Path, help="built v1 fbuild executable; otherwise soldr cargo run")
    parser.add_argument("--bucket", default="s3://fbuild")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="fbuild-prebuilt-") as root:
        command = [str(args.fbuild.resolve())] if args.fbuild else ["soldr", "cargo", "run", "-p", "fbuild-cli", "--"]
        subprocess.run(command + ["build", str(args.project.resolve()), "-e", args.environment,
            "--no-prebuilt", "--output-dir", root], cwd=ROOT, check=True)
        plans = list(Path(root).rglob("prebuilt-plan.json"))
        if len(plans) != 1:
            raise ValueError("expected exactly one engine-exported pilot plan with pinned package checksums")
        metadata = json.loads(plans[0].read_bytes())
        if metadata["board"] != args.board:
            raise ValueError("requested pilot board does not match resolved plan")
        artifact, sidecar, key = package(metadata, args.output, {
            "vcs": "git", "repo_url": args.source_repo, "ref": args.source_ref})
    for path, remote in [(artifact, key), (sidecar, key + ".release.json")]:
        print(shlex.join(["aws", "s3", "cp", str(path), args.bucket.rstrip("/") + "/" + remote]))
    print("# WITHDRAWAL: delete the bucket artifact AND sidecar, then regenerate the Index:")
    for remote in [key, key + ".release.json"]:
        print("# " + shlex.join(["aws", "s3", "rm", args.bucket.rstrip("/") + "/" + remote]))
    print("# Run the byte-identical firmware/ELF acceptance gate before uploading.")

if __name__ == "__main__":
    main()

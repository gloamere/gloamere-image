"""Create a developer source archive with paired notices, never an install ZIP.

Requires an actual Git commit. This does not package an uncommitted working tree
or build an installer; use build-release.ps1 for end-user delivery.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import io
import json
import stat
import subprocess
import zipfile
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("installer_staging", Path(__file__).with_name("stage-installer-payload.py"))
staging = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(staging)
packaging = staging.packaging
PROJECT = Path(__file__).resolve().parent.parent


def package_source(commit_ref: str, notices: Path, output: Path, project: Path = PROJECT) -> dict:
    output = packaging.ensure_under_dist(output, project)
    if output.exists():
        raise ValueError("Use a fresh source archive destination")
    if output.suffix.lower() != ".zip" or "source" not in output.stem.lower():
        raise ValueError("Name this developer archive explicitly as source, with a .zip suffix")
    commit = subprocess.check_output(
        ["git", "rev-parse", "--verify", "--end-of-options", commit_ref + "^{commit}"], cwd=project, text=True,
    ).strip()
    source = subprocess.check_output(["git", "archive", "--format=zip", commit], cwd=project)
    files = {}
    with zipfile.ZipFile(io.BytesIO(source)) as archive:
        for entry in archive.infolist():
            if entry.is_dir():
                continue
            path = Path(entry.filename)
            packaging.safe_relative_path(entry.filename)
            mode = entry.external_attr >> 16
            if stat.S_ISLNK(mode):
                raise ValueError(f"Source symlinks are not included: {entry.filename}")
            if path.name.lower() in ("agents.md", "skill.md") or path.parts[0] in (
                ".beads", ".agents", ".codex", ".git", "dist", "target", "test-output", "third-party-notices"
            ) or entry.filename == "SOURCE-MANIFEST.json":
                continue
            if entry.filename in files:
                raise ValueError("Duplicate source archive entry")
            files[entry.filename] = archive.read(entry)
    inventory = packaging.load_json(notices / "inventory.json")
    expected = {"inventory.json"}
    for record in inventory["packages"]:
        if not record.get("bundled_notices"):
            raise ValueError("Use paired staged notices, with bundled hashes")
        for local, _ in staging.notice_sources(record, notices):
            expected.add(local.relative_to(notices).as_posix())
    actual = {local.relative_to(notices).as_posix() for local in packaging.regular_tree(notices)}
    if actual != expected:
        raise ValueError("Unlisted or missing notice files: " + repr(sorted(actual ^ expected)))
    for relative in sorted(expected):
        files["third-party-notices/" + relative] = (notices / relative).read_bytes()
    packaging.check_path_collisions(list(files) + ["SOURCE-MANIFEST.json"])
    manifest = {
        "product": packaging.PRODUCT, "version": packaging.VERSION, "source_commit": commit,
        "kind": "developer source with paired Windows dependency notices; not an installation archive",
        "registry_sources_vendored": False, "execution_instructions_included": False,
        "application_screenshots_included": False, "notice_packages": len(inventory["packages"]),
        "files": [{"path": name, "size_bytes": len(content), "sha256": hashlib.sha256(content).hexdigest()}
                  for name, content in sorted(files.items())],
    }
    files["SOURCE-MANIFEST.json"] = (json.dumps(manifest, ensure_ascii=False, indent=2) + "\n").encode()
    output.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(output, "x", zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        for name, content in sorted(files.items()):
            archive.writestr(name, content)
    with zipfile.ZipFile(output) as archive:
        if archive.testzip() is not None:
            raise ValueError("Source ZIP CRC validation failed")
    return {"local_path": str(output), "source_commit": commit, "kind": "developer source archive",
            "size_bytes": output.stat().st_size, "sha256": packaging.sha256(output),
            "files": len(files), "notice_packages": len(inventory["packages"]), "zip_crc_verified": True}


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--notices", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(argv)
    print(json.dumps(package_source(args.commit, args.notices, args.output), indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (ValueError, OSError, KeyError, subprocess.CalledProcessError) as error:
        raise SystemExit(str(error)) from error

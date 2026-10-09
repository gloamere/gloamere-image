"""Stage internal installer inputs, with hashes; never create an install ZIP.

The directory is a developer/QA input to build-installer.py, not an end-user
installation path. All inputs must be local. No network requests are made.
"""
from __future__ import annotations

import argparse
import copy
import importlib.util
import json
import shutil
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("installer_packaging", Path(__file__).with_name("build-installer.py"))
packaging = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(packaging)
PROJECT = Path(__file__).resolve().parent.parent


def notice_sources(record: dict, licenses: Path) -> list[tuple[Path, str]]:
    """Read new inventories and historical paired-notice inventories explicitly."""
    result = []
    if record.get("bundled_notices"):
        for notice in record["bundled_notices"]:
            relative = notice.get("payload_path", notice.get("portable_path"))
            packaging.safe_relative_path(relative)
            if not relative.startswith("third-party-notices/"):
                raise ValueError("Bundled notice must be beneath third-party-notices")
            local = licenses / relative.removeprefix("third-party-notices/")
            packaging.require_plain_ancestors(local)
            packaging.require_regular_file(local)
            if packaging.sha256(local) != notice.get("sha256"):
                raise ValueError(f"Cached notice hash mismatch: {relative}")
            result.append((local, notice.get("original_published_path", relative)))
    else:
        relatives = list(record.get("notices", []))
        if record.get("license_declaration_notice"):
            relatives.append(record["license_declaration_notice"])
        for relative in relatives:
            packaging.safe_relative_path(relative)
            local = licenses / relative
            packaging.require_plain_ancestors(local)
            packaging.require_regular_file(local)
            result.append((local, relative))
    if not result:
        raise ValueError(f'No bundled notice or explicit declaration for {record.get("name")}')
    return result


def stage_payload(release: Path, licenses: Path, output: Path, commit: str,
                  report: Path, project: Path = PROJECT, provenance: dict | None = None) -> dict:
    output = packaging.ensure_under_dist(output, project)
    if output.exists():
        raise ValueError("Use a fresh staging directory; no existing files are replaced")
    if not commit or any(ord(char) < 32 for char in commit):
        raise ValueError("A nonempty source provenance label is required")
    inventory = copy.deepcopy(packaging.load_json(licenses / "inventory.json"))
    if not isinstance(inventory.get("packages"), list) or not inventory["packages"]:
        raise ValueError("License inventory must contain dependency packages")
    copies = [(release / name, name) for name in ("gloamere-image.exe", "gloamere-image-worker.exe")]
    copies.extend((project / name, name) for name in ("README.md", "LICENSE-MIT", "LICENSE-APACHE", "assets/gloamere-image.ico"))
    copies.append((report, "VALIDATION.md"))
    package_ids = set()
    for record in inventory["packages"]:
        package_id = f'{record["name"]}-{record["version"]}'
        packaging.safe_relative_path(package_id)
        if "/" in package_id or package_id.casefold() in package_ids:
            raise ValueError("Invalid or duplicated license package")
        package_ids.add(package_id.casefold())
        bundled = []
        for index, (source, original) in enumerate(notice_sources(record, licenses)):
            destination = f"third-party-notices/{package_id}/{index:02d}-{source.name}"
            copies.append((source, destination))
            bundled.append({"original_published_path": original, "payload_path": destination,
                            "sha256": packaging.sha256(source)})
        record["bundled_notices"] = bundled
    packaging.check_path_collisions([relative for _, relative in copies] + ["third-party-notices/inventory.json", "MANIFEST.json"])
    for source, _ in copies:
        packaging.require_plain_ancestors(source)
        packaging.require_regular_file(source)
    output.mkdir(parents=True)
    try:
        for source, relative in copies:
            destination = output / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, destination)
        (output / "third-party-notices/inventory.json").write_text(
            json.dumps(inventory, ensure_ascii=False, indent=2) + "\n", "utf-8")
        records = []
        for local in packaging.regular_tree(output):
            relative = local.relative_to(output).as_posix()
            if local.suffix.lower() in (".png", ".jpg", ".jpeg", ".cs", ".ttf", ".ttc", ".key"):
                raise ValueError(f"Unexpected screenshot/font/client/key in staging: {relative}")
            records.append({"path": relative, "size_bytes": local.stat().st_size,
                            "sha256": packaging.sha256(local)})
        manifest = {
            "product": packaging.PRODUCT, "version": packaging.VERSION,
            "kind": "internal installer staging; not an end-user distribution",
            "source_commit": commit, "architecture": "x86_64-pc-windows-msvc",
            "graphics_backend": "D3D12", "signature_status": "NotSigned",
            "distribution": "Setup EXE only", "files": records,
            "image_uploads": False, "telemetry": False,
            "application_screenshots_included": False,
        }
        if provenance is not None:
            manifest["provenance"] = provenance
        (output / "MANIFEST.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", "utf-8")
        packaging.validate_payload(output)
    except BaseException:
        shutil.rmtree(output)
        raise
    return {"payload": str(output), "files": len(records) + 1,
            "manifest_sha256": packaging.sha256(output / "MANIFEST.json"),
            "distribution_archive_created": False, "payload_validation": "passed"}


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release", type=Path, required=True)
    parser.add_argument("--licenses", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--provenance", type=Path)
    args = parser.parse_args(argv)
    provenance = packaging.load_json(args.provenance) if args.provenance else None
    print(json.dumps(stage_payload(args.release, args.licenses, args.output, args.commit, args.report,
                                   provenance=provenance), ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (ValueError, OSError, KeyError) as error:
        raise SystemExit(str(error)) from error

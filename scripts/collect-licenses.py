"""Collect locked Windows dependencies locally; never download missing notices.

Missing published notices may use exact hash-verified, matching package records
already checked into third-party-notices. Explicit historical license-declaration
fallbacks remain labeled, not upgraded into legal approval. New gaps fail closed.
"""
from __future__ import annotations

import argparse
import copy
import importlib.util
import json
import re
import shutil
import tomllib
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("installer_staging", Path(__file__).with_name("stage-installer-payload.py"))
staging = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(staging)
packaging = staging.packaging
PROJECT = Path(__file__).resolve().parent.parent


def workspace_packages(project: Path) -> set[tuple[str, str]]:
    """Read every explicitly declared local member; reject unsafe/glob paths.

    This release workspace uses literal paths. Fail closed if it adopts member
    globbing or inherited package identities rather than silently omitting them.
    Match name AND version so an unrelated registry version is still collected.
    """
    packaging.require_plain_ancestors(project)
    root_path = project / "Cargo.toml"
    packaging.require_regular_file(root_path)
    root = tomllib.loads(root_path.read_text("utf-8"))
    manifests = [root]
    for member in root.get("workspace", {}).get("members", []):
        packaging.safe_relative_path(member)
        path = project / member / "Cargo.toml"
        packaging.require_plain_ancestors(path)
        packaging.require_regular_file(path)
        manifests.append(tomllib.loads(path.read_text("utf-8")))
    result = set()
    for manifest in manifests:
        package = manifest.get("package")
        if package is None:
            continue
        name, version = package.get("name"), package.get("version")
        if not isinstance(name, str) or not name or not isinstance(version, str) or not version:
            raise ValueError("Workspace package must declare literal name and version")
        result.add((name, version))
    return result


def collect(tree: Path, registry: Path, output: Path, cache: Path | None = None) -> dict:
    packaging.require_plain_ancestors(output)
    if output.exists():
        raise ValueError("Use a fresh license output directory")
    packaging.require_regular_file(tree)
    packages = sorted(set(re.findall(r"^([\w-]+) v([0-9][^\s]+)", tree.read_text("utf-8-sig"), re.M)))
    if not packages:
        raise ValueError("Empty dependency tree; use cargo tree --prefix none --edges normal,build")
    own_packages = workspace_packages(PROJECT)
    if not any(package not in own_packages for package in packages):
        raise ValueError("No external dependencies parsed; use cargo tree --prefix none --edges normal,build")
    cached = {}
    if cache is not None:
        cached_inventory = packaging.load_json(cache / "inventory.json")
        for record in cached_inventory["packages"]:
            key = (record["name"], record["version"])
            if key in cached:
                raise ValueError("Duplicate cached license package")
            cached[key] = record
    records, missing, declarations = [], [], []
    output.mkdir(parents=True)
    for name, version in packages:
        if (name, version) in own_packages:
            continue
        package_id = f"{name}-{version}"
        packaging.safe_relative_path(package_id)
        sources = list(registry.glob(f"*/{package_id}"))
        if len(sources) != 1:
            missing.append(f"{package_id}: published source missing or ambiguous")
            continue
        source = sources[0]
        packaging.require_plain_ancestors(source)
        package = tomllib.loads((source / "Cargo.toml").read_text("utf-8"))["package"]
        if package.get("name") != name or package.get("version") != version:
            raise ValueError(f"Registry manifest identity mismatch: {package_id}")
        files = [path for path in source.rglob("*") if path.is_file() and
                 not any(part.startswith(".") for part in path.relative_to(source).parts) and
                 (re.match(r"^(license|licence|copying|notice)([._-]|$)", path.name, re.I)
                  or re.search(r"license.*\.txt$", path.name, re.I)
                  or path.name.lower() in ("ofl.txt", "ufl.txt", "hack-regular.txt"))]
        license_file = package.get("license-file")
        if license_file:
            packaging.safe_relative_path(license_file)
            if (source / license_file).is_file():
                files.append(source / license_file)
        record = {"name": name, "version": version, "license": package.get("license"),
                  "license_file": license_file, "repository": package.get("repository"), "notices": []}
        for path in sorted(set(files)):
            relative = path.relative_to(source).as_posix()
            packaging.safe_relative_path(relative)
            packaging.require_plain_ancestors(path)
            packaging.require_regular_file(path)
            destination = output / package_id / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, destination)
            record["notices"].append(destination.relative_to(output).as_posix())
        if not files:
            old = cached.get((name, version))
            if old is None or old.get("license") != record["license"] or old.get("repository") != record["repository"]:
                missing.append(f"{package_id}: no published notice or matching checked-in fallback")
            else:
                # Only verified bundled records can be used as an offline cache.
                if not old.get("bundled_notices"):
                    raise ValueError(f"Cached notices lack hashes: {package_id}")
                for index, (path, original) in enumerate(staging.notice_sources(old, cache)):
                    relative = f"{package_id}/cached-{index:02d}-{path.name}"
                    destination = output / relative
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(path, destination)
                    if original == old.get("license_declaration_notice"):
                        record["license_declaration_notice"] = relative
                        record["test_distribution_license_choice"] = old.get("test_distribution_license_choice")
                        declarations.append(package_id)
                    else:
                        record["notices"].append(relative)
                record["cached_notice_provenance"] = copy.deepcopy(old.get("fetched_notice_sources", []))
        records.append(record)
    result = {"scope": "locked Windows workspace normal and build dependencies",
              "not_legal_review": True, "packages": records, "missing_notice_files": missing,
              "notice_declaration_fallbacks": declarations}
    (output / "inventory.json").write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", "utf-8")
    if missing:
        raise ValueError("Unresolved license notices; inventory saved for inspection: " + "; ".join(missing))
    return {"packages": len(records), "notices": sum(len(record["notices"]) for record in records),
            "declaration_fallbacks": declarations, "missing": missing}


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tree", type=Path, required=True)
    parser.add_argument("--registry", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cache", type=Path, default=PROJECT / "third-party-notices")
    args = parser.parse_args(argv)
    print(json.dumps(collect(args.tree, args.registry, args.output, args.cache), indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (ValueError, OSError, KeyError) as error:
        raise SystemExit(str(error)) from error

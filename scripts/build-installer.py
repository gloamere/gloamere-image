"""Validate internal staging and compile the EXE-only Windows distribution.

Use a locally installed official Inno Setup 6.7.3 compiler. This script never
fetches a compiler. Importing this module is safe on Linux; compilation is Windows
only. The generated report distinguishes compilation from unrun lifecycle QA.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import re
import stat
import subprocess
import tempfile
import unicodedata
from pathlib import Path

PRODUCT = "gloamere-image"
VERSION = "0.4.0"
PRODUCT_ID = "9CCB6F43-7782-4F69-9E91-67C6D144737B"
OWNER_MARKER = "LIGHT-IMAGE-INSTALL.txt"
COMPILER_VERSION = "6.7.3"
COMPILER_MAX_BYTES = 64 * 1024 * 1024
PROBE_MARKER = "GLOAMERE_COMPILER_VERSION_PROBE"
PROBE_INPUT = f"#error {PROBE_MARKER}\n"
PROBE_TIMEOUT_SECONDS = 15
PROBE_MAX_OUTPUT_BYTES = 64 * 1024
COMPILER_COMPONENTS = ("ISCC.exe", "ISCmplr.dll", "ISPP.dll")
SETUP_ORIGINAL_FILENAME_CAPACITY = 50
RESOURCE_HELPER = Path(__file__).resolve().parent.parent / "build-support/verify_windows_resources.py"
INSTALLER_NAME = f"{PRODUCT}-{VERSION}-Win64-Setup-unsigned.exe"
REQUIRED_FILES = {
    "gloamere-image.exe", "gloamere-image-worker.exe", "assets/gloamere-image.ico",
    "README.md", "VALIDATION.md", "LICENSE-MIT", "LICENSE-APACHE",
    "third-party-notices/inventory.json",
}
RESERVED_FILES = {"manifest.json", OWNER_MARKER.casefold()}
PROJECT = Path(__file__).resolve().parent.parent


def sha256(path: Path) -> str:
    with path.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256")
    return digest.hexdigest()


def safe_relative_path(value: str) -> str:
    """Accept canonical slash paths with unambiguous Windows/Inno semantics."""
    if not isinstance(value, str) or not value or len(value) > 240:
        raise ValueError("Invalid or overlong payload path")
    if any(ord(c) < 32 or ord(c) == 127 or c in '\\<>:"|?*;{}' for c in value):
        raise ValueError(f"Unsafe payload path: {value!r}")
    parts = value.split("/")
    for part in parts:
        if not part or part in (".", "..") or part.endswith((".", " ")):
            raise ValueError(f"Noncanonical payload path: {value!r}")
        if part != unicodedata.normalize("NFC", part):
            raise ValueError(f"Noncanonical Unicode payload path: {value!r}")
        stem = part.split(".", 1)[0].upper()
        if re.fullmatch(r"CON|PRN|AUX|NUL|COM[1-9¹²³]|LPT[1-9¹²³]", stem):
            raise ValueError(f"Reserved Windows device path: {value!r}")
    return value


def check_path_collisions(paths: list[str]) -> None:
    nodes: dict[str, tuple[str, bool]] = {}
    for relative in paths:
        safe_relative_path(relative)
        parts = relative.split("/")
        for index in range(1, len(parts) + 1):
            text = "/".join(parts[:index])
            key = text.casefold()
            is_file = index == len(parts)
            previous = nodes.get(key)
            if previous is not None:
                if previous[0] != text or previous[1] != is_file or is_file:
                    raise ValueError(f"Duplicate, case collision or file/directory collision: {text}")
            else:
                nodes[key] = (text, is_file)


def reject_reparse(path: Path) -> os.stat_result:
    result = path.lstat()
    # st_file_attributes is available on Windows. Avoid Path.is_junction(),
    # which is absent on Python 3.11 and misses other reparse point types.
    if stat.S_ISLNK(result.st_mode) or getattr(result, "st_file_attributes", 0) & 0x400:
        raise ValueError(f"Reparse/symlink path is not permitted: {path}")
    return result


def require_regular_file(path: Path) -> None:
    if not stat.S_ISREG(reject_reparse(path).st_mode):
        raise ValueError(f"Not a regular file: {path}")


def require_plain_ancestors(path: Path) -> None:
    absolute = Path(os.path.abspath(path))
    for parent in reversed((absolute, *absolute.parents)):
        if parent.exists() or parent.is_symlink():
            reject_reparse(parent)


def ensure_under_dist(path: Path, project: Path = PROJECT) -> Path:
    require_plain_ancestors(path)
    result = path.resolve()
    dist = (project / "dist").resolve()
    result.relative_to(dist)
    if result == dist:
        raise ValueError("Choose a subdirectory/file inside dist, not dist itself")
    return result


def regular_tree(root: Path) -> list[Path]:
    require_plain_ancestors(root)
    if not stat.S_ISDIR(reject_reparse(root).st_mode):
        raise ValueError(f"Not a directory: {root}")
    files = []
    directories = []
    for current, dirs, names in os.walk(root, followlinks=False):
        for name in dirs:
            local = Path(current) / name
            if not stat.S_ISDIR(reject_reparse(local).st_mode):
                raise ValueError(f"Not a regular directory: {local}")
            directories.append(local.relative_to(root).as_posix())
        for name in names:
            local = Path(current) / name
            require_regular_file(local)
            files.append(local)
    paths = [local.relative_to(root).as_posix() for local in files]
    check_path_collisions(paths)
    for directory in directories:
        safe_relative_path(directory)
        if not any(name.startswith(directory + "/") for name in paths):
            raise ValueError(f"Unexpected empty directory: {directory}")
    return sorted(files)


def load_json(path: Path) -> dict:
    require_regular_file(path)
    def unique_object(pairs):
        value = {}
        for key, item in pairs:
            if key in value:
                raise ValueError(f"Duplicate JSON key: {key}")
            value[key] = item
        return value
    value = json.loads(path.read_text("utf-8-sig"), object_pairs_hook=unique_object)
    if not isinstance(value, dict):
        raise ValueError(f"Expected JSON object: {path}")
    return value


def validate_payload(payload: Path) -> dict:
    payload = Path(payload)
    actual = {local.relative_to(payload).as_posix() for local in regular_tree(payload)}
    manifest = load_json(payload / "MANIFEST.json")
    if manifest.get("product") != PRODUCT or manifest.get("version") != VERSION:
        raise ValueError("Unexpected product or version")
    if manifest.get("architecture") != "x86_64-pc-windows-msvc":
        raise ValueError("Unexpected payload architecture")
    if not isinstance(manifest.get("source_commit"), str) or not manifest["source_commit"]:
        raise ValueError("Missing source provenance")
    records = manifest.get("files")
    if not isinstance(records, list) or not records:
        raise ValueError("Missing payload file records")
    paths = []
    for entry in records:
        if not isinstance(entry, dict) or set(entry) != {"path", "size_bytes", "sha256"}:
            raise ValueError("Malformed payload record")
        relative = safe_relative_path(entry["path"])
        if relative.casefold() in RESERVED_FILES:
            raise ValueError(f"Reserved manifest path: {relative}")
        if type(entry["size_bytes"]) is not int or entry["size_bytes"] < 0:
            raise ValueError(f"Invalid payload size: {relative}")
        if not isinstance(entry["sha256"], str) or not re.fullmatch(r"[0-9a-f]{64}", entry["sha256"]):
            raise ValueError(f"Invalid SHA-256: {relative}")
        paths.append(relative)
    check_path_collisions(paths + ["MANIFEST.json", OWNER_MARKER])
    expected = set(paths) | {"MANIFEST.json"}
    if actual != expected:
        raise ValueError("Unexpected or missing payload files: " + repr(sorted(actual ^ expected)))
    if not REQUIRED_FILES.issubset(paths):
        raise ValueError("Required payload files missing: " + repr(sorted(REQUIRED_FILES - set(paths))))
    for entry in records:
        local = payload / entry["path"]
        require_regular_file(local)
        if local.stat().st_size != entry["size_bytes"] or sha256(local) != entry["sha256"]:
            raise ValueError("Payload hash/size mismatch: " + entry["path"])
    icon = (payload / "assets/gloamere-image.ico").read_bytes()
    if len(icon) < 6 or icon[:4] != b"\x00\x00\x01\x00" or icon[4:6] == b"\x00\x00":
        raise ValueError("Required product icon is not an ICO file")
    for name in ("gloamere-image.exe", "gloamere-image-worker.exe"):
        with (payload / name).open("rb") as stream:
            if stream.read(2) != b"MZ":
                raise ValueError(f"Expected Windows executable: {name}")
    return manifest


def generate_includes(paths: list[str], directory: Path) -> tuple[Path, Path]:
    check_path_collisions(paths + [OWNER_MARKER])
    entries = []
    for relative in sorted(paths):
        source = relative.replace("/", "\\")
        folder = source.rpartition("\\")[0]
        destination = "{app}" + ("\\" + folder if folder else "")
        entries.append(f'Source: "{{#PayloadDir}}\\{source}"; DestDir: "{destination}"; Flags: ignoreversion')
    entries.append('Source: "{#OwnerMarker}"; DestDir: "{app}"; Flags: ignoreversion')
    file_list = directory / "payload-files.iss"
    file_list.write_text("\n".join(entries) + "\n", "utf-8")
    recovery = ["procedure PopulateRecoveryPaths(Paths: TStringList);", "begin"]
    for relative in sorted(paths + [OWNER_MARKER]):
        escaped = relative.replace("/", "\\").replace("'", "''")
        recovery.append(f"  Paths.Add('{escaped}');")
    recovery.append("end;")
    recovery_list = directory / "payload-recovery.iss"
    recovery_list.write_text("\n".join(recovery) + "\n", "utf-8")
    return file_list, recovery_list


def running_on_windows() -> bool:
    return os.name == "nt"


def compiler_identity(compiler: Path) -> dict:
    """Read the selected front-end file's descriptive PE identity, not its engine version.

    This is a compatibility/identity check, not publisher authentication. The
    developer still supplies the preinstalled compiler from its official source.
    """
    require_regular_file(compiler)
    if compiler.name.casefold() != "iscc.exe":
        raise ValueError("Supply the installed official Inno Setup ISCC.exe")
    with compiler.open("rb") as stream:
        data = stream.read(COMPILER_MAX_BYTES + 1)
    if len(data) > COMPILER_MAX_BYTES:
        raise ValueError("Compiler exceeds the bounded PE inspection size")
    spec = importlib.util.spec_from_file_location("gloamere_pe_resources", RESOURCE_HELPER)
    if spec is None or spec.loader is None:
        raise ValueError("Required PE resource verifier is unavailable")
    verifier = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(verifier)
    try:
        resources = verifier.pe_resources(data)
        versions = [(key, block) for key, block in resources.items() if key[0] == 16]
        if not versions or len(versions) > 16:
            raise ValueError("Compiler has missing or excessive RT_VERSION resources")
        observed_versions = set()
        for _key, block in versions:
            root, fixed, children = verifier.version_node(block)
            if root != "VS_VERSION_INFO" or len(fixed) != 52:
                raise ValueError("Compiler has invalid VS_FIXEDFILEINFO")
            numbers = verifier.unpack("<13I", fixed)
            if numbers[0] != 0xFEEF04BD or numbers[1] != 0x10000:
                raise ValueError("Compiler has invalid fixed-version signature/structure")
            file_version = (numbers[2] >> 16, numbers[2] & 0xFFFF,
                            numbers[3] >> 16, numbers[3] & 0xFFFF)
            product_version = (numbers[4] >> 16, numbers[4] & 0xFFFF,
                               numbers[5] >> 16, numbers[5] & 0xFFFF)
            observed_versions.add((file_version, product_version))
            if len(observed_versions) != 1:
                raise ValueError("Compiler has conflicting descriptive fixed-version resources")
            tables = []
            for child_name, _child_value, child_tables in children:
                if child_name == "StringFileInfo":
                    tables.extend(child_tables)
            if not tables or len(tables) > 16:
                raise ValueError("Compiler version resource has missing/excessive identity tables")
            for table_name, _table_value, fields in tables:
                if not re.fullmatch(r"[0-9a-fA-F]{8}", table_name):
                    raise ValueError("Compiler version resource has an invalid language/codepage")
                strings = {}
                for field, value, nested in fields:
                    if field in strings or nested:
                        raise ValueError("Compiler version resource has conflicting identity fields")
                    strings[field] = value.decode("utf-16-le").rstrip("\0")
                if strings.get("ProductName") != "Inno Setup" or strings.get("FileDescription") != "Inno Setup Command-Line Compiler":
                    raise ValueError("Compiler version resource identity is not Inno Setup Command-Line Compiler")
                # Official OriginalFilename/InternalName may be empty. Human
                # FileVersion text is not the authoritative fixed version.
    except RecursionError as error:
        raise ValueError("Compiler version resource nesting is invalid") from error
    return {
        "file_version": ".".join(map(str, file_version)),
        "product_version": ".".join(map(str, product_version)),
        "product_name": "Inno Setup",
        "file_description": "Inno Setup Command-Line Compiler",
        "version_source": "PE RT_VERSION / VS_FIXEDFILEINFO",
        "component": "ISCC.exe command-line front end",
        "engine_version_authoritative": False,
        "sha256": hashlib.sha256(data).hexdigest(),
        "publisher_authentication": "not_performed",
    }


def compiler_snapshot(compiler: Path) -> dict[str, str]:
    """Hash the known front end, engine and preprocessor used by Inno 6.7.3."""
    result = {}
    for name in COMPILER_COMPONENTS:
        path = compiler if name == "ISCC.exe" else compiler.parent / name
        require_regular_file(path)
        if path.stat().st_size > COMPILER_MAX_BYTES:
            raise ValueError(f"Compiler component exceeds inspection size: {name}")
        result[name] = sha256(path)
    return result


def require_compiler_snapshot(compiler: Path, expected: dict[str, str]) -> None:
    if compiler_snapshot(compiler) != expected:
        raise ValueError("Compiler components changed after inspection; refusing compilation/publication")


def parse_engine_probe(result: subprocess.CompletedProcess) -> dict:
    if result.returncode != 2:
        raise ValueError(f"Compiler version probe must fail intentionally with exit 2, got {result.returncode}")
    stdout, stderr = result.stdout or "", result.stderr or ""
    if len((stdout + stderr).encode("utf-8")) > PROBE_MAX_OUTPUT_BYTES:
        raise ValueError("Compiler version probe output exceeds the accepted size")
    output_lines = [line.strip() for line in stdout.splitlines() if line.strip()]
    error_lines = [line.strip() for line in stderr.splitlines() if line.strip()]
    engine_lines = [line for line in output_lines + error_lines if "compiler engine version" in line.casefold()]
    if engine_lines != [f"Compiler engine version: Inno Setup {COMPILER_VERSION}"]:
        raise ValueError("Compiler probe did not report exactly one supported engine version")
    expected_error = f"Error on line 1 in <stdin>: {PROBE_MARKER}."
    if (len(error_lines) != 2 or error_lines[0] != expected_error
            or error_lines[1] != "Compile aborted."
            or (stdout + stderr).count(PROBE_MARKER) != 1
            or any(re.match(r"(?i)^(?:error|internal error|fatal error)\b", line) for line in output_lines)
            or any(re.search(r"(?i)successful compile|output was disabled", line) for line in output_lines)):
        raise ValueError("Compiler probe did not produce only the expected line-1 #error failure")
    return {"engine_version": COMPILER_VERSION,
            "version_source": "ISCC stdin #error probe / compiler engine banner",
            "expected_failure_marker": PROBE_MARKER, "exit_code": 2,
            "outputs_created": False}


def probe_compiler_engine(compiler: Path, expected: dict[str, str]) -> dict:
    require_compiler_snapshot(compiler, expected)
    with tempfile.TemporaryDirectory(prefix="gloamere-compiler-probe-") as directory:
        try:
            result = subprocess.run([str(compiler), "-"], input=PROBE_INPUT,
                                    cwd=directory, capture_output=True, text=True,
                                    encoding="utf-8", errors="replace", check=False,
                                    timeout=PROBE_TIMEOUT_SECONDS)
        except subprocess.TimeoutExpired as error:
            raise ValueError("Compiler version probe timed out") from error
        require_compiler_snapshot(compiler, expected)
        if any(Path(directory).iterdir()):
            raise ValueError("Compiler version probe created unexpected files/directories")
        return parse_engine_probe(result)


def validate_setup_filename(name: str) -> None:
    if len(name.encode("utf-16-le")) // 2 > SETUP_ORIGINAL_FILENAME_CAPACITY:
        raise ValueError("Setup filename exceeds Inno 6.7.3 OriginalFileName capacity (50 UTF-16 units)")


def compile_installer(compiler: Path, payload: Path, output: Path) -> dict:
    if not running_on_windows():
        raise ValueError("Inno compilation requires Windows; use --validate-only elsewhere")
    payload = ensure_under_dist(payload, PROJECT)
    output = ensure_under_dist(output, PROJECT)
    if output == payload or payload in output.parents or output in payload.parents:
        raise ValueError("Payload and installer output must be separate directories")
    manifest = validate_payload(payload)
    compiler = compiler.resolve(strict=True)
    compiler_metadata = compiler_identity(compiler)
    component_hashes = compiler_snapshot(compiler)
    if component_hashes["ISCC.exe"] != compiler_metadata["sha256"]:
        raise ValueError("Compiler changed after its identity inspection")
    engine_probe = probe_compiler_engine(compiler, component_hashes)
    validate_setup_filename(INSTALLER_NAME)
    output.mkdir(parents=True, exist_ok=True)
    installer = output / INSTALLER_NAME
    hash_file = output / (INSTALLER_NAME + ".sha256")
    report_file = output / "installer-build.json"
    if any(path.exists() for path in (installer, hash_file, report_file)):
        raise ValueError("Use a fresh output directory; release files are never overwritten")
    for path in (payload, output, compiler):
        if any(character in str(path) for character in '\r\n"{};'):
            raise ValueError("Unsafe Inno build location")
    with tempfile.TemporaryDirectory(prefix="inno-build-", dir=payload.parent) as temporary:
        internal = Path(temporary)
        marker = internal / OWNER_MARKER
        marker.write_text(PRODUCT_ID, "ascii")
        file_list, recovery_list = generate_includes(
            [entry["path"] for entry in manifest["files"]] + ["MANIFEST.json"], internal)
        # Recheck immediately before the compiler reads the exact generated list.
        validate_payload(payload)
        require_compiler_snapshot(compiler, component_hashes)
        subprocess.run([
            str(compiler), "/Qp", "/DPayloadDir=" + str(payload),
            "/DOutputDir=" + str(internal), "/DProductVersion=" + VERSION,
            "/DPayloadFileList=" + str(file_list), "/DPayloadRecoveryList=" + str(recovery_list),
            "/DOwnerMarker=" + str(marker), str(PROJECT / "installer/light-image-tool.iss"),
        ], check=True)
        require_compiler_snapshot(compiler, component_hashes)
        built = internal / INSTALLER_NAME
        require_regular_file(built)
        with built.open("rb") as stream:
            if stream.read(2) != b"MZ":
                raise ValueError("Compiler did not produce a Windows Setup executable")
        validate_payload(payload)
        result = {
            "product": PRODUCT, "version": VERSION, "artifact": INSTALLER_NAME,
            "size_bytes": built.stat().st_size, "sha256": sha256(built),
            "source_commit": manifest["source_commit"], "product_id": PRODUCT_ID,
            "compiler": f"Inno Setup {COMPILER_VERSION}", "compiler_sha256": compiler_metadata["sha256"],
            "compiler_identity": compiler_metadata, "compiler_engine_probe": engine_probe,
            "compiler_components_sha256": component_hashes,
            "signature_status": "NotSigned", "distribution": "Setup EXE only",
            "payload_manifest_sha256": sha256(payload / "MANIFEST.json"),
            "stages": {"payload_validation": "passed", "inno_compile": "passed",
                       "windows_install_upgrade_rollback_uninstall": "not_run",
                       "windows_visual_qa": "not_run"},
            "per_user": True, "default_associations_changed": False,
            "application_screenshots_included": False,
            "limitations": ["Unsigned test build; no signing or trust validation claimed.",
                            "Local path/hash checks do not provide atomic protection against concurrent mutation."],
        }
        built.rename(installer)
    hash_file.write_text(f'{result["sha256"]}  {INSTALLER_NAME}\n', "ascii")
    report_file.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", "utf-8")
    return result


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compiler", type=Path, help="Preinstalled official Inno Setup 6.7.3 ISCC.exe")
    parser.add_argument("--payload", type=Path, required=True)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--validate-only", action="store_true")
    args = parser.parse_args(argv)
    if args.validate_only:
        manifest = validate_payload(args.payload)
        print(json.dumps({"payload_validation": "passed", "files": len(manifest["files"]),
                          "version": manifest["version"], "inno_compile": "not_run"}, indent=2))
    else:
        if args.compiler is None or args.output is None:
            parser.error("--compiler and --output are required unless --validate-only")
        print(json.dumps(compile_installer(args.compiler, args.payload, args.output), indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        raise SystemExit(str(error)) from error

"""Linux-safe packaging/security contracts; no real PE build or Inno execution.

Fixtures use synthetic MZ and ICO headers solely to test staging and validation.
Actual PE resources are checked separately in the Windows release pipeline.
"""
from __future__ import annotations

import copy
import hashlib
import importlib.util
import io
import json
import os
import subprocess
import struct
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent


def load(filename):
    spec = importlib.util.spec_from_file_location(filename.replace("-", "_"), ROOT / "scripts" / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


build = load("build-installer.py")
stage = load("stage-installer-payload.py")
collect = load("collect-licenses.py")
source = load("package-source.py")


class PackagingFixture(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.project = self.root / "project"
        self.project.mkdir()
        self.release = self.root / "release"
        self.release.mkdir()
        for name in ("gloamere-image.exe", "gloamere-image-worker.exe"):
            (self.release / name).write_bytes(b"MZfixture")
        for name in ("README.md", "LICENSE-MIT", "LICENSE-APACHE"):
            (self.project / name).write_text("fixture\n", "utf-8")
        (self.project / "assets").mkdir()
        (self.project / "assets/gloamere-image.ico").write_bytes(b"\0\0\1\0\1\0fixture")
        self.report = self.root / "VALIDATION.md"
        self.report.write_text("Fixture only; no Windows test executed.\n", "utf-8")
        self.licenses = self.root / "licenses"
        (self.licenses / "dependency-1.0.0").mkdir(parents=True)
        (self.licenses / "dependency-1.0.0/LICENSE").write_text("MIT fixture", "utf-8")
        self.inventory = {"packages": [{"name": "dependency", "version": "1.0.0", "license": "MIT",
                                        "repository": "https://example.invalid/source", "notices": ["dependency-1.0.0/LICENSE"]}]}
        self.write_inventory()
        self.payload = self.project / "dist/internal/payload"

    def write_inventory(self):
        (self.licenses / "inventory.json").write_text(json.dumps(self.inventory), "utf-8")

    def make_payload(self):
        return stage.stage_payload(self.release, self.licenses, self.payload, "fixture-commit", self.report, self.project)

    def read_manifest(self):
        return json.loads((self.payload / "MANIFEST.json").read_text("utf-8"))

    def write_manifest(self, manifest):
        (self.payload / "MANIFEST.json").write_text(json.dumps(manifest, indent=2), "utf-8")

    def add_manifest_file(self, relative, content=b"fixture"):
        local = self.payload / relative
        local.parent.mkdir(parents=True, exist_ok=True)
        local.write_bytes(content)
        manifest = self.read_manifest()
        manifest["files"].append({"path": relative, "size_bytes": len(content), "sha256": hashlib.sha256(content).hexdigest()})
        self.write_manifest(manifest)

    def test_imports_are_argument_parser_free(self):
        for filename in ("build-installer.py", "stage-installer-payload.py", "package-portable.py", "collect-licenses.py", "package-source.py"):
            with self.subTest(filename=filename), mock.patch("sys.argv", [filename, "--unexpected"]):
                load(filename)

    def test_stage_is_exact_and_creates_no_archive(self):
        result = self.make_payload()
        manifest = build.validate_payload(self.payload)
        self.assertEqual(manifest["product"], "gloamere-image")
        self.assertEqual(manifest["version"], "0.4.0")
        self.assertFalse(result["distribution_archive_created"])
        self.assertFalse(list(self.project.rglob("*.zip")))
        self.assertNotIn("light-image-tool.exe", [entry["path"] for entry in manifest["files"]])
        for entry in manifest["files"]:
            self.assertEqual(list(entry), ["path", "size_bytes", "sha256"])
        bundled = json.loads((self.payload / "third-party-notices/inventory.json").read_text())
        self.assertIn("payload_path", bundled["packages"][0]["bundled_notices"][0])

    def test_hash_and_size_tampering_fail(self):
        self.make_payload()
        local = self.payload / "README.md"
        original = local.read_bytes()
        for content in (b"other!!\n", original + b"x"):
            local.write_bytes(content)
            with self.assertRaisesRegex(ValueError, "hash/size mismatch"):
                build.validate_payload(self.payload)

    def test_unlisted_file_fails(self):
        self.make_payload()
        (self.payload / "surprise.txt").write_text("unexpected")
        with self.assertRaisesRegex(ValueError, "Unexpected or missing"):
            build.validate_payload(self.payload)

    def test_missing_file_fails(self):
        self.make_payload()
        (self.payload / "README.md").unlink()
        with self.assertRaisesRegex(ValueError, "Unexpected or missing"):
            build.validate_payload(self.payload)

    def test_required_icon_cannot_be_removed_from_manifest(self):
        self.make_payload()
        (self.payload / "assets/gloamere-image.ico").unlink()
        (self.payload / "assets").rmdir()
        manifest = self.read_manifest()
        manifest["files"] = [entry for entry in manifest["files"] if not entry["path"].endswith(".ico")]
        self.write_manifest(manifest)
        with self.assertRaisesRegex(ValueError, "Required payload files"):
            build.validate_payload(self.payload)

    def test_invalid_icon_and_non_pe_are_rejected_even_with_correct_hash(self):
        self.make_payload()
        for name in ("assets/gloamere-image.ico", "gloamere-image.exe", "gloamere-image-worker.exe"):
            with self.subTest(name=name):
                local = self.payload / name
                original = local.read_bytes()
                original_manifest = self.read_manifest()
                local.write_bytes(b"invalid content")
                altered = copy.deepcopy(original_manifest)
                entry = next(entry for entry in altered["files"] if entry["path"] == name)
                entry.update(size_bytes=local.stat().st_size, sha256=build.sha256(local))
                self.write_manifest(altered)
                with self.assertRaises(ValueError):
                    build.validate_payload(self.payload)
                local.write_bytes(original)
                self.write_manifest(original_manifest)

    def test_unsafe_windows_and_inno_paths(self):
        paths = ["../escape", "/absolute", "C:/drive", "a\\b", "a//b", "a/./b", "a/../b", "a:",
                 "a.", "a ", "dir /b", "NUL.txt", "con", "AUX", "COM1.log", "LPT9", "COM¹.txt",
                 "quote\".txt", "semi;.txt", "{macro}.txt", "a?", "a*", "a|", "a\n", "a\x00", "cafe\u0301.txt"]
        for value in paths:
            with self.subTest(path=value), self.assertRaises(ValueError):
                build.safe_relative_path(value)
        for value in ("normal file.txt", "third-party-notices/pkg-1.0/LICENSE", "apostrophe's.txt"):
            self.assertEqual(build.safe_relative_path(value), value)

    def test_case_file_directory_and_duplicate_collisions(self):
        for paths in (["a", "A"], ["a", "a"], ["a", "a/b"], ["a/b", "A/c"], ["a/b", "a"]):
            with self.subTest(paths=paths), self.assertRaises(ValueError):
                build.check_path_collisions(paths)
        build.check_path_collisions(["a/b", "a/c"])

    def test_duplicate_record_and_reserved_manifest_fail(self):
        self.make_payload()
        manifest = self.read_manifest()
        for name in (manifest["files"][0]["path"], "MANIFEST.json", "manifest.json", build.OWNER_MARKER):
            altered = copy.deepcopy(manifest)
            altered["files"].append({"path": name, "size_bytes": 0, "sha256": "0" * 64})
            self.write_manifest(altered)
            with self.subTest(path=name), self.assertRaises(ValueError):
                build.validate_payload(self.payload)

    def test_invalid_record_types_and_product_metadata(self):
        self.make_payload()
        original = self.read_manifest()
        changes = [("size_bytes", True), ("size_bytes", -1), ("sha256", "xyz"), ("path", None)]
        for key, value in changes:
            altered = copy.deepcopy(original)
            altered["files"][0][key] = value
            self.write_manifest(altered)
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                build.validate_payload(self.payload)
        for key, value in (("product", "other"), ("version", "9.0.0"), ("architecture", "x86"), ("source_commit", "")):
            altered = copy.deepcopy(original)
            altered[key] = value
            self.write_manifest(altered)
            with self.subTest(key=key), self.assertRaises(ValueError):
                build.validate_payload(self.payload)

    def test_duplicate_json_keys_fail(self):
        self.make_payload()
        path = self.payload / "MANIFEST.json"
        path.write_text('{"version":"0.4.0","version":"0.4.0"}')
        with self.assertRaisesRegex(ValueError, "Duplicate JSON key"):
            build.validate_payload(self.payload)

    def test_empty_directory_fails(self):
        self.make_payload()
        (self.payload / "empty").mkdir()
        with self.assertRaisesRegex(ValueError, "empty directory"):
            build.validate_payload(self.payload)

    def test_symlink_file_and_directory_are_rejected(self):
        self.make_payload()
        local = self.payload / "README.md"
        local.unlink()
        try:
            local.symlink_to(self.project / "README.md")
        except OSError as error:
            self.skipTest(f"Symlink privilege unavailable: {error}")
        with self.assertRaisesRegex(ValueError, "Reparse/symlink"):
            build.validate_payload(self.payload)
        local.unlink()
        local.write_text("fixture\n")
        (self.payload / "external").symlink_to(self.licenses, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "Reparse/symlink"):
            build.validate_payload(self.payload)

    @unittest.skipUnless(hasattr(os, "mkfifo"), "POSIX FIFO test")
    def test_nonregular_fifo_is_rejected_without_reading(self):
        self.make_payload()
        os.mkfifo(self.payload / "fifo")
        with self.assertRaisesRegex(ValueError, "Not a regular file"):
            build.validate_payload(self.payload)

    def test_reserved_owner_marker_is_not_part_of_staging(self):
        self.make_payload()
        (self.payload / build.OWNER_MARKER).write_text(build.PRODUCT_ID)
        with self.assertRaises(ValueError):
            build.validate_payload(self.payload)

    def test_missing_notice_and_traversal_refused_before_staging(self):
        for relative in ("../escape", "dependency-1.0.0/missing"):
            self.inventory["packages"][0]["notices"] = [relative]
            self.write_inventory()
            with self.subTest(relative=relative), self.assertRaises((ValueError, OSError)):
                self.make_payload()
            self.assertFalse(self.payload.exists())

    def test_staging_failure_removes_only_new_staging(self):
        (self.project / "assets/gloamere-image.ico").write_bytes(b"invalid")
        with self.assertRaises(ValueError):
            self.make_payload()
        self.assertFalse(self.payload.exists())
        self.assertTrue((self.project / "README.md").exists())

    def test_existing_staging_is_preserved(self):
        self.make_payload()
        before = (self.payload / "MANIFEST.json").read_bytes()
        with self.assertRaisesRegex(ValueError, "fresh staging"):
            self.make_payload()
        self.assertEqual((self.payload / "MANIFEST.json").read_bytes(), before)

    def test_staging_outside_dist_is_refused(self):
        with self.assertRaises(ValueError):
            stage.stage_payload(self.release, self.licenses, self.root / "outside", "fixture", self.report, self.project)

    def test_generated_includes_are_bounded_and_escape_pascal_quotes(self):
        names = ["gloamere-image.exe", "gloamere-image-worker.exe", "assets/gloamere-image.ico", "MANIFEST.json", "apostrophe's.txt"]
        files, recovery = build.generate_includes(names, self.root)
        self.assertIn("Paths.Add('apostrophe''s.txt');", recovery.read_text())
        self.assertEqual(recovery.read_text().count("Paths.Add("), len(names) + 1)
        self.assertIn('Source: "{#OwnerMarker}"', files.read_text())
        self.assertNotIn("*", files.read_text())
        self.assertIn(build.OWNER_MARKER, recovery.read_text())
        self.assertNotIn("light-image-tool.exe", recovery.read_text())

    def test_historical_notice_paths_require_hashes(self):
        record = self.inventory["packages"][0]
        notice = self.licenses / record["notices"][0]
        record["bundled_notices"] = [{"portable_path": "third-party-notices/dependency-1.0.0/LICENSE",
                                      "sha256": build.sha256(notice), "original_published_path": record["notices"][0]}]
        self.write_inventory()
        self.make_payload()
        record["bundled_notices"][0]["sha256"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "hash mismatch"):
            stage.notice_sources(record, self.licenses)

    def test_validate_only_cli_does_not_invoke_compiler(self):
        self.make_payload()
        result = subprocess.run([os.sys.executable, str(ROOT / "scripts/build-installer.py"), "--payload", str(self.payload), "--validate-only"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["inno_compile"], "not_run")

    @unittest.skipIf(os.name == "nt", "Windows supports compilation")
    def test_compile_on_linux_is_explicitly_blocked(self):
        with self.assertRaisesRegex(ValueError, "requires Windows"):
            build.compile_installer(self.root / "ISCC.exe", self.payload, self.project / "dist/release")

    def test_source_archive_excludes_stale_self_manifest_and_private_records(self):
        self.make_payload()
        archive = io.BytesIO()
        with zipfile.ZipFile(archive, "w") as output:
            output.writestr("Cargo.toml", "fixture")
            output.writestr("SOURCE-MANIFEST.json", "stale")
            output.writestr("AGENTS.md", "private")
            output.writestr(".codex/private", "private")
            output.writestr("third-party-notices/stale", "stale")
        destination = self.project / "dist/gloamere-image-0.4.0-source.zip"
        with mock.patch.object(source.subprocess, "check_output", side_effect=["a" * 40, archive.getvalue()]):
            result = source.package_source("HEAD", self.payload / "third-party-notices", destination, self.project)
        self.assertTrue(result["zip_crc_verified"])
        with zipfile.ZipFile(destination) as output:
            self.assertNotIn("AGENTS.md", output.namelist())
            self.assertNotIn(".codex/private", output.namelist())
            self.assertNotIn("third-party-notices/stale", output.namelist())
            manifest = json.loads(output.read("SOURCE-MANIFEST.json"))
            self.assertNotIn("SOURCE-MANIFEST.json", [entry["path"] for entry in manifest["files"]])

    def test_source_archive_rejects_unlisted_notices_and_install_name(self):
        self.make_payload()
        with self.assertRaisesRegex(ValueError, "explicitly as source"):
            source.package_source("HEAD", self.payload / "third-party-notices", self.project / "dist/portable.zip", self.project)
        (self.payload / "third-party-notices/extra").write_text("unexpected")
        archive = io.BytesIO()
        with zipfile.ZipFile(archive, "w") as output:
            output.writestr("Cargo.toml", "fixture")
        with mock.patch.object(source.subprocess, "check_output", side_effect=["a" * 40, archive.getvalue()]):
            with self.assertRaisesRegex(ValueError, "Unlisted or missing"):
                source.package_source("HEAD", self.payload / "third-party-notices", self.project / "dist/source.zip", self.project)



def compiler_pe(file_version=(6, 7, 3, 0), product_version=None, *,
                strings=None, codepage="040904e4", pe32=True, second_version=None,
                duplicate_field=False, include_version=True):
    """Release-shaped synthetic PE; no compiler is executed by these fixtures."""
    support_spec = importlib.util.spec_from_file_location(
        "gloamere_test_resources", ROOT / "build-support/test_windows_resources.py")
    support = importlib.util.module_from_spec(support_spec)
    support_spec.loader.exec_module(support)
    product_version = file_version if product_version is None else product_version
    fields = {"ProductName": "Inno Setup", "FileDescription": "Inno Setup Command-Line Compiler",
              "FileVersion": "6.7.3", "ProductVersion": "6.7.3",
              "OriginalFilename": "", "InternalName": "", "CompanyName": "Jordan Russell"}
    if strings is not None:
        fields = strings
    entries = [support.version_block(key, (value + "\0").encode("utf-16-le"), text=True)
               for key, value in fields.items()]
    if duplicate_field:
        entries.append(support.version_block("ProductName", "Other\0".encode("utf-16-le"), text=True))
    tables = support.version_block("StringFileInfo", children=[
        support.version_block(codepage, children=entries, text=True)], text=True)
    def fixed_block(version):
        fixed = struct.pack("<13I", 0xFEEF04BD, 0x10000,
                            version[0] << 16 | version[1], version[2] << 16 | version[3],
                            product_version[0] << 16 | product_version[1],
                            product_version[2] << 16 | product_version[3],
                            0x3F, 0, 4, 1, 0, 0, 0)
        return support.version_block("VS_VERSION_INFO", fixed, [tables])
    resources = {(16, 1, 1033): fixed_block(file_version)} if include_version else {}
    if second_version is not None:
        resources[(16, 1, 0)] = fixed_block(second_version)
    result = bytearray(support.sample_pe(resources))
    if pe32:
        optional = 0x98
        resource_rva, resource_size = struct.unpack_from("<II", result, optional + 128)
        section = bytes(result[optional + 240:optional + 280])
        result[optional:512] = bytes(512 - optional)
        struct.pack_into("<H", result, 0x84, 0x14C)
        struct.pack_into("<H", result, 0x94, 224)
        struct.pack_into("<H", result, 0x96, 0x102)
        struct.pack_into("<H", result, optional, 0x10B)
        struct.pack_into("<I", result, optional + 92, 16)
        struct.pack_into("<II", result, optional + 112, resource_rva, resource_size)
        result[optional + 224:optional + 264] = section
    return bytes(result)


class CompilerIdentityTests(unittest.TestCase):
    setUp = PackagingFixture.setUp
    write_inventory = PackagingFixture.write_inventory
    make_payload = PackagingFixture.make_payload

    def compiler(self, data=None):
        path = self.root / "ISCC.exe"
        path.write_bytes(compiler_pe((0, 0, 0, 0)) if data is None else data)
        for name in ("ISCmplr.dll", "ISPP.dll"):
            (self.root / name).write_bytes(b"MZsynthetic compiler component " + name.encode())
        return path

    @staticmethod
    def probe_result(version="6.7.3", exit_code=2):
        # Exact native protocol: generic stdout banner; marker ends in a literal dot.
        return subprocess.CompletedProcess(["ISCC.exe", "-"], exit_code,
            "Inno Setup 6 Command-Line Compiler\n\n"
            f"Compiler engine version: Inno Setup {version}\n"
            "Non-commercial use only\n\nPreprocessing\n",
            "Error on line 1 in <stdin>: GLOAMERE_COMPILER_VERSION_PROBE.\nCompile aborted.\n")

    def test_zero_frontend_version_is_descriptive_and_not_an_engine_gate(self):
        for pe32, codepage, version in ((True, "040904e4", (0, 0, 0, 0)),
                                       (False, "040904B0", (6, 7, 3, 0))):
            compiler = self.compiler(compiler_pe(version, pe32=pe32, codepage=codepage))
            with self.subTest(pe32=pe32), mock.patch.object(build.subprocess, "run") as runner:
                identity = build.compiler_identity(compiler)
            runner.assert_not_called()
            self.assertEqual(identity["file_version"], ".".join(map(str, version)))
            self.assertEqual(identity["product_version"], ".".join(map(str, version)))
            self.assertFalse(identity["engine_version_authoritative"])
            self.assertEqual(identity["sha256"], build.sha256(compiler))
            self.assertEqual(identity["publisher_authentication"], "not_performed")

    def test_missing_wrong_conflicting_identity_and_resources_fail(self):
        for fields in ({}, {"ProductName": "Other", "FileDescription": "Inno Setup Command-Line Compiler"},
                       {"ProductName": "Inno Setup", "FileDescription": "Other tool"}):
            with self.subTest(fields=fields), self.assertRaisesRegex(ValueError, "identity"):
                build.compiler_identity(self.compiler(compiler_pe(strings=fields)))
        with self.assertRaisesRegex(ValueError, "conflicting descriptive"):
            build.compiler_identity(self.compiler(compiler_pe(second_version=(6, 7, 2, 0))))
        with self.assertRaisesRegex(ValueError, "conflicting identity fields"):
            build.compiler_identity(self.compiler(compiler_pe(duplicate_field=True)))
        with self.assertRaisesRegex(ValueError, "missing or excessive RT_VERSION"):
            build.compiler_identity(self.compiler(compiler_pe(include_version=False)))

    def test_corrupt_wrong_name_oversize_and_missing_components_fail_closed(self):
        for data in (b"not PE", b"MZ", compiler_pe()[:511]):
            with self.subTest(length=len(data)), self.assertRaises(ValueError):
                build.compiler_identity(self.compiler(data))
        compiler = self.compiler()
        with mock.patch.object(build, "COMPILER_MAX_BYTES", 16), self.assertRaisesRegex(ValueError, "bounded"):
            build.compiler_identity(compiler)
        other = self.root / "other.exe"
        other.write_bytes(compiler_pe())
        with self.assertRaisesRegex(ValueError, "ISCC.exe"):
            build.compiler_identity(other)
        (self.root / "ISCmplr.dll").unlink()
        with self.assertRaises((ValueError, OSError)):
            build.compiler_snapshot(compiler)

    def test_exact_native_probe_protocol_including_marker_period_passes(self):
        result = build.parse_engine_probe(self.probe_result())
        self.assertEqual(result["engine_version"], "6.7.3")
        self.assertEqual(result["exit_code"], 2)
        self.assertFalse(result["outputs_created"])

    def test_probe_rejects_missing_wrong_prefix_prerelease_or_duplicate_versions(self):
        variants = [self.probe_result(version) for version in ("6", "6.7.2", "6.7.30", "6.7.3-beta", "6.7.3 extra")]
        for change in ("missing", "duplicate", "conflict", "prefix"):
            result = self.probe_result()
            if change == "missing": result.stdout = "Inno Setup 6 Command-Line Compiler\n"
            elif change == "prefix": result.stdout = result.stdout.replace("Compiler engine version:", "Fake Compiler engine version:")
            else: result.stdout += "Compiler engine version: Inno Setup " + ("6.7.3" if change == "duplicate" else "6.7.4") + "\n"
            variants.append(result)
        for result in variants:
            with self.subTest(stdout=result.stdout), self.assertRaisesRegex(ValueError, "exactly one supported"):
                build.parse_engine_probe(result)

    def test_probe_rejects_unexpected_exit_marker_errors_and_success_messages(self):
        variants = [self.probe_result(exit_code=code) for code in (0, 1, 3)]
        for change in ("no-period", "no-marker", "wrong-line", "extra-error", "duplicate-marker", "success", "output-disabled"):
            result = self.probe_result()
            if change == "no-period": result.stderr = result.stderr.replace("_PROBE.", "_PROBE")
            elif change == "no-marker": result.stderr = "Compile aborted.\n"
            elif change == "wrong-line": result.stderr = result.stderr.replace("line 1", "line 2")
            elif change == "extra-error": result.stderr += "Error: something else\n"
            elif change == "duplicate-marker": result.stdout += build.PROBE_MARKER + "\n"
            elif change == "success": result.stdout += "Successful compile (0.1 sec).\n"
            else: result.stdout += "Output was disabled.\n"
            variants.append(result)
        for result in variants:
            with self.subTest(stdout=result.stdout, stderr=result.stderr), self.assertRaises(ValueError):
                build.parse_engine_probe(result)
        result = self.probe_result()
        result.stdout += "x" * build.PROBE_MAX_OUTPUT_BYTES
        with self.assertRaisesRegex(ValueError, "accepted size"):
            build.parse_engine_probe(result)

    def test_probe_uses_fixed_stdin_isolated_cwd_and_explicit_timeout(self):
        compiler = self.compiler()
        hashes = build.compiler_snapshot(compiler)
        observed = []
        def run(arguments, **kwargs):
            self.assertEqual(arguments, [str(compiler), "-"])
            self.assertEqual(kwargs["input"], "#error GLOAMERE_COMPILER_VERSION_PROBE\n")
            self.assertEqual(kwargs["timeout"], 15)
            directory = Path(kwargs["cwd"])
            self.assertTrue(directory.is_dir())
            self.assertEqual(list(directory.iterdir()), [])
            self.assertNotEqual(directory, self.project)
            observed.append(directory)
            return self.probe_result()
        with mock.patch.object(build.subprocess, "run", side_effect=run):
            build.probe_compiler_engine(compiler, hashes)
        self.assertFalse(observed[0].exists())

    def test_probe_timeout_or_any_created_artifact_is_rejected(self):
        compiler = self.compiler()
        hashes = build.compiler_snapshot(compiler)
        with mock.patch.object(build.subprocess, "run", side_effect=subprocess.TimeoutExpired([str(compiler), "-"], 15)):
            with self.assertRaisesRegex(ValueError, "timed out"):
                build.probe_compiler_engine(compiler, hashes)
        def artifact(_arguments, **kwargs):
            (Path(kwargs["cwd"]) / "Output").mkdir()
            return self.probe_result()
        with mock.patch.object(build.subprocess, "run", side_effect=artifact):
            with self.assertRaisesRegex(ValueError, "unexpected files/directories"):
                build.probe_compiler_engine(compiler, hashes)

    def test_frontend_or_engine_or_preprocessor_mutation_during_probe_is_rejected(self):
        for name in build.COMPILER_COMPONENTS:
            compiler = self.compiler()
            hashes = build.compiler_snapshot(compiler)
            def changed(_arguments, **_kwargs):
                path = self.root / name
                path.write_bytes(path.read_bytes() + b"changed")
                return self.probe_result()
            with self.subTest(name=name), mock.patch.object(build.subprocess, "run", side_effect=changed):
                with self.assertRaisesRegex(ValueError, "components changed"):
                    build.probe_compiler_engine(compiler, hashes)

    def test_zero_pe_and_realistic_engine_probe_permit_actual_compile_not_help(self):
        self.make_payload()
        compiler = self.compiler()
        output = self.project / "dist/release"
        def run(arguments, **kwargs):
            if arguments[1:] == ["-"]:
                self.assertEqual(kwargs["input"], build.PROBE_INPUT)
                return self.probe_result()
            self.assertEqual(arguments[1], "/Qp")
            temporary = Path(next(arg.removeprefix("/DOutputDir=") for arg in arguments if arg.startswith("/DOutputDir=")))
            (temporary / build.INSTALLER_NAME).write_bytes(b"MZsynthetic setup")
            return subprocess.CompletedProcess(arguments, 0)
        with mock.patch.object(build, "running_on_windows", return_value=True), \
             mock.patch.object(build, "PROJECT", self.project), \
             mock.patch.object(build.subprocess, "run", side_effect=run) as runner:
            result = build.compile_installer(compiler, self.payload, output)
        self.assertEqual(runner.call_count, 2, "one intentional failure probe, one real compile; never /?")
        self.assertEqual(result["compiler_identity"]["file_version"], "0.0.0.0")
        self.assertEqual(result["compiler_engine_probe"]["engine_version"], "6.7.3")
        self.assertEqual(set(result["compiler_components_sha256"]), set(build.COMPILER_COMPONENTS))
        self.assertEqual(result["stages"]["windows_install_upgrade_rollback_uninstall"], "not_run")

    def test_wrong_engine_is_rejected_before_setup_compile(self):
        self.make_payload()
        compiler = self.compiler()
        with mock.patch.object(build, "running_on_windows", return_value=True), \
             mock.patch.object(build, "PROJECT", self.project), \
             mock.patch.object(build.subprocess, "run", return_value=self.probe_result("6.7.2")) as runner:
            with self.assertRaisesRegex(ValueError, "supported engine version"):
                build.compile_installer(compiler, self.payload, self.project / "dist/release")
        self.assertEqual(runner.call_count, 1)
        self.assertEqual(runner.call_args.args[0][1:], ["-"])

    def test_changed_compiler_after_identity_is_rejected_before_probe(self):
        self.make_payload()
        compiler = self.compiler()
        original = build.compiler_identity
        def changed(path):
            identity = original(path)
            path.write_bytes(path.read_bytes() + b"changed")
            return identity
        with mock.patch.object(build, "running_on_windows", return_value=True), \
             mock.patch.object(build, "PROJECT", self.project), \
             mock.patch.object(build, "compiler_identity", side_effect=changed), \
             mock.patch.object(build.subprocess, "run") as runner:
            with self.assertRaisesRegex(ValueError, "Compiler changed"):
                build.compile_installer(compiler, self.payload, self.project / "dist/release")
        runner.assert_not_called()

    def test_engine_mutation_during_compile_prevents_setup_publication(self):
        self.make_payload()
        compiler = self.compiler()
        output = self.project / "dist/release"
        def changed(arguments, **_kwargs):
            if arguments[1:] == ["-"]: return self.probe_result()
            temporary = Path(next(arg.removeprefix("/DOutputDir=") for arg in arguments if arg.startswith("/DOutputDir=")))
            (temporary / build.INSTALLER_NAME).write_bytes(b"MZsynthetic setup")
            engine = self.root / "ISCmplr.dll"
            engine.write_bytes(engine.read_bytes() + b"changed")
            return subprocess.CompletedProcess(arguments, 0)
        with mock.patch.object(build, "running_on_windows", return_value=True), \
             mock.patch.object(build, "PROJECT", self.project), \
             mock.patch.object(build.subprocess, "run", side_effect=changed):
            with self.assertRaisesRegex(ValueError, "components changed"):
                build.compile_installer(compiler, self.payload, output)
        self.assertFalse((output / build.INSTALLER_NAME).exists())
        self.assertFalse((output / "installer-build.json").exists())

    def test_setup_name_capacity_and_fixed_text_versions_are_explicit(self):
        build.validate_setup_filename(build.INSTALLER_NAME)
        self.assertEqual(len(build.INSTALLER_NAME), 45)
        build.validate_setup_filename("a" * 46 + ".exe")
        with self.assertRaisesRegex(ValueError, "50 UTF-16"):
            build.validate_setup_filename("a" * 47 + ".exe")
        with self.assertRaisesRegex(ValueError, "50 UTF-16"):
            build.validate_setup_filename("\U0001f305" * 24 + ".exe")
        script = (ROOT / "installer/light-image-tool.iss").read_text("utf-8")
        for key in ("VersionInfoVersion", "VersionInfoTextVersion", "VersionInfoProductVersion", "VersionInfoProductTextVersion"):
            self.assertIn(key + "={#ProductVersion}\n", script)
        self.assertIn("VersionInfoOriginalFileName=gloamere-image-{#ProductVersion}-Win64-Setup-unsigned.exe", script)


class LicenseCollectionTests(unittest.TestCase):
    setUp = PackagingFixture.setUp
    write_inventory = PackagingFixture.write_inventory
    def test_collection_excludes_only_current_workspace_packages(self):
        registry = self.root / "registry/index/dependency-1.0.0"
        registry.mkdir(parents=True)
        (registry / "Cargo.toml").write_text('[package]\nname="dependency"\nversion="1.0.0"\nlicense="MIT"\n')
        (registry / "LICENSE").write_text("MIT fixture")
        tree = self.root / "tree.txt"
        workspace_names = []
        import tomllib
        for local in (ROOT / "Cargo.toml", ROOT / "crates/provenance-worker/Cargo.toml"):
            workspace_names.append(tomllib.loads(local.read_text())["package"]["name"])
        tree.write_text("\n".join([name + " v0.4.0" for name in workspace_names] + ["dependency v1.0.0"]))
        result = collect.collect(tree, self.root / "registry", self.root / "collected", None)
        self.assertEqual(result["packages"], 1)
        self.assertEqual(result["notices"], 1)

    def test_new_license_gap_fails_with_inventory_evidence(self):
        registry = self.root / "registry/index/dependency-1.0.0"
        registry.mkdir(parents=True)
        (registry / "Cargo.toml").write_text('[package]\nname="dependency"\nversion="1.0.0"\nlicense="MIT"\n')
        tree = self.root / "tree.txt"
        tree.write_text("dependency v1.0.0")
        with self.assertRaisesRegex(ValueError, "Unresolved license notices"):
            collect.collect(tree, self.root / "registry", self.root / "collected", None)
        self.assertTrue((self.root / "collected/inventory.json").is_file())


class ReleaseCommandContracts(unittest.TestCase):
    def test_one_command_contains_all_gates_and_no_download(self):
        script = (ROOT / "scripts/build-release.ps1").read_text()
        gates = ["'packaging_contracts'", "'installer_contracts'", "'migration_contracts'", "'resource_contracts'", "'format'", "'clippy'", "'debug_build'", "'debug_tests'", "'release_build'", "'release_tests'", "'pe_resources'", "'stage_payload'", "'validate_payload'", "'compile_setup'"]
        positions = [script.index("Invoke-Gate " + gate) for gate in gates]
        self.assertEqual(positions, sorted(positions))
        self.assertLess(script.index("if ($env:GLOAMERE_WINDOWS_RESOURCE_CHECK_ONLY)"), script.index("Invoke-Gate 'release_build'"))
        self.assertIn("target-feature=+crt-static", script)
        self.assertIn("'--locked', '--offline'", script)
        self.assertIn("$env:CARGO_TARGET_DIR = $targetRoot", script)
        self.assertIn("$binaryRoot = Join-Path $targetRoot ($target + '\\release')", script)
        for gate in ("debug_build", "debug_tests", "release_build", "release_tests"):
            line = next(line for line in script.splitlines() if "Invoke-Gate '" + gate + "'" in line)
            self.assertIn("'--target', $target", line)
            self.assertIn("'--all-targets'", line)
        self.assertIn("windows_lifecycle_qa = 'not_run'", script)
        self.assertIn("windows_visual_qa = 'not_run'", script)
        for command in ("Invoke-WebRequest", "Start-BitsTransfer", "Expand-Archive", "Compress-Archive"):
            self.assertNotIn(command, script)
        wrapper = (ROOT / "scripts/package-portable.py").read_text()
        self.assertNotIn("zipfile", wrapper)
        self.assertIn("stage-installer-payload.py", wrapper)

    def test_silent_success_has_nonnull_log_lines(self):
        script = (ROOT / "scripts/build-release.ps1").read_text()
        gate = script.split("function Invoke-Gate", 1)[1].split("Push-Location", 1)[0]
        self.assertIn("$captured = @()", gate)
        self.assertIn("$lines = [string[]]@()", gate)
        self.assertIn("if ($null -ne $captured)", gate)
        self.assertIn("($Name + '.log')), $lines,", gate)

    def test_stable_identity_and_unsigned_exe_name(self):
        self.assertEqual(build.PRODUCT_ID, "9CCB6F43-7782-4F69-9E91-67C6D144737B")
        self.assertEqual(build.OWNER_MARKER, "LIGHT-IMAGE-INSTALL.txt")
        self.assertEqual(build.INSTALLER_NAME, "gloamere-image-0.4.0-Win64-Setup-unsigned.exe")


if __name__ == "__main__":
    unittest.main(verbosity=2)

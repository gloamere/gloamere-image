"""Ephemeral fixtures only. Every private key is disposed with its temp folder.

These tests must never populate source/bootstrap, Library, or a live repository.
Set TUF_ROOT_PROBE to the separately built debug verifier for cross-language QA.
"""
import contextlib
import datetime as dt
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import provision_keys as p

TEST_PASSWORD = b"EPHEMERAL-NOT-PRODUCTION-TEST-PASSWORD"


def sign(body, record):
    return p.json_bytes({"signed": body, "signatures": [
        {"keyid": record["id"], "sig": record["key"].sign(p.canonical(body)).hex()}]})


def fixture_bundle(records, root):
    now = dt.datetime.now(dt.timezone.utc) - dt.timedelta(seconds=1)
    body = {"spec_version": "1.0.0", "version": 1, "issued": p.utc(now)}
    targets = sign(dict(body, _type="targets", expires=p.utc(now + dt.timedelta(days=7)), targets={}), records["targets"])
    reference = lambda data: {"version": 1, "length": len(data), "hashes": {"sha256": hashlib.sha256(data).hexdigest()}}
    snapshot = sign(dict(body, _type="snapshot", expires=p.utc(now + dt.timedelta(days=7)), meta={"targets.json": reference(targets)}), records["snapshot"])
    timestamp = sign(dict(body, _type="timestamp", expires=p.utc(now + dt.timedelta(days=1)), meta={"snapshot.json": reference(snapshot)}), records["timestamp"])
    root_bytes = p.json_bytes(root)
    return {"test_only": True, "root": root_bytes.decode(), "public_bundle": p.json_bytes(p.public_bundle(root_bytes)).decode(),
            "metadata": {"timestamp.json": timestamp.decode(), "1.snapshot.json": snapshot.decode(), "1.targets.json": targets.decode()}}


class OfflineProvisionerTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="gloamere-EPHEMERAL-TEST-")
        self.parent = Path(self.temporary.name)
        self.vault = self.parent / "NEVER-PRODUCTION-vault"

    def tearDown(self):
        self.temporary.cleanup()

    def test_default_is_read_only_dry_run(self):
        output = io.StringIO()
        with patch.object(p, "generate_material", side_effect=AssertionError("must not generate")), contextlib.redirect_stdout(output):
            self.assertEqual(p.main(["--vault", str(self.vault)]), 0)
        self.assertIn("DRY RUN", output.getvalue())
        self.assertFalse(self.vault.exists())

    def test_production_create_rejects_arbitrary_destination_before_keygen(self):
        with patch.object(p, "generate_material", side_effect=AssertionError("must not generate")):
            with self.assertRaises(p.Refused):
                p.create_vault(self.vault, TEST_PASSWORD)
        self.assertFalse(self.vault.exists())

    def test_secure_prompt_rejects_redirected_input(self):
        with patch.object(p.sys.stdin, "isatty", return_value=False), patch.object(p.getpass, "getpass", side_effect=AssertionError("must not prompt")):
            with self.assertRaises(p.Refused):
                p.local_password()

    def test_secure_prompt_rejects_echo_fallback(self):
        with patch.object(p.sys.stdin, "isatty", return_value=True), patch.object(p.sys.stderr, "isatty", return_value=True), patch.object(p.getpass, "getpass", side_effect=p.getpass.GetPassWarning("test")):
            with self.assertRaises(p.Refused):
                p.local_password()

    def test_no_password_argument_or_environment_option(self):
        with contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit):
                p.main(["--password", "EPHEMERAL-TEST"])

    def test_refuses_existing_empty_directory(self):
        self.vault.mkdir()
        with self.assertRaises(p.Refused):
            p.validate_vault(self.vault, test_only=True)

    def test_refuses_existing_nonempty_directory(self):
        self.vault.mkdir()
        sentinel = self.vault / "keep.txt"
        sentinel.write_text("keep")
        with self.assertRaises(p.Refused):
            p.create_vault(self.vault, TEST_PASSWORD, test_only=True)
        self.assertEqual(sentinel.read_text(), "keep")

    @unittest.skipIf(os.name == "nt", "symlink privilege is host-specific on Windows")
    def test_refuses_symlink_ancestor(self):
        real = self.parent / "real"
        real.mkdir(mode=0o700)
        alias = self.parent / "alias"
        alias.symlink_to(real, target_is_directory=True)
        with self.assertRaises(p.Refused):
            p.validate_vault(alias / "keys", test_only=True)

    @unittest.skipIf(os.name == "nt", "Unix-only permission case")
    def test_refuses_exposed_parent_without_changing_permissions(self):
        exposed = self.parent / "exposed"
        exposed.mkdir(mode=0o755)
        before = exposed.stat().st_mode
        with self.assertRaises(p.Refused):
            p.validate_vault(exposed / "keys", test_only=True)
        self.assertEqual(exposed.stat().st_mode, before)

    def test_rejects_dangerous_paths(self):
        for path in (Path("relative"), self.parent / ".." / "bad", Path("/")):
            with self.subTest(path=path), self.assertRaises(p.Refused):
                p.validate_vault(path, test_only=True)

    def test_rejects_noncanonical_input(self):
        for value in (1.0, -1, "中文", "a\nb", 'a"b', "a\\b"):
            with self.subTest(value=value), self.assertRaises(p.Refused):
                p.canonical(value)

    def test_no_clobber_persistence(self):
        target = self.parent / "keep"
        target.write_bytes(b"original")
        with self.assertRaises(FileExistsError):
            p.persist_new(target, b"replacement")
        self.assertEqual(target.read_bytes(), b"original")

    def test_encrypted_vault_roundtrip_and_public_only_output(self):
        audit = p.create_vault(self.vault, TEST_PASSWORD, test_only=True)
        serialization, _ = p.crypto()
        root_bytes = (self.vault / "public/root.json").read_bytes()
        root = json.loads(root_bytes)
        self.assertTrue(audit["test_only"])
        self.assertEqual(len(root["signed"]["keys"]), 6)
        self.assertEqual(root["signed"]["roles"]["root"]["threshold"], 2)
        self.assertEqual(len(root["signatures"]), 3)
        self.assertEqual(hashlib.sha256(root_bytes).hexdigest(), audit["root_sha256"])
        labels = [label for labels in p.ROLES.values() for label in labels]
        for label in labels:
            encrypted = (self.vault / "private" / f"{label}.encrypted.pk8.pem").read_bytes()
            self.assertTrue(encrypted.startswith(b"-----BEGIN ENCRYPTED PRIVATE KEY-----"))
            with self.assertRaises((ValueError, TypeError)):
                serialization.load_pem_private_key(encrypted, b"wrong-ephemeral-password")
            key = serialization.load_pem_private_key(encrypted, TEST_PASSWORD)
            raw = key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw).hex()
            self.assertIn(raw, {value["keyval"]["public"] for value in root["signed"]["keys"].values()})
        for signature in root["signatures"]:
            from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
            raw = bytes.fromhex(root["signed"]["keys"][signature["keyid"]]["keyval"]["public"])
            Ed25519PublicKey.from_public_bytes(raw).verify(bytes.fromhex(signature["sig"]), p.canonical(root["signed"]))
        for file in (self.vault / "public").iterdir():
            self.assertNotIn(b"PRIVATE KEY", file.read_bytes())
            self.assertNotIn(TEST_PASSWORD, file.read_bytes())
        self.assertTrue((self.vault / "COMPLETE.json").exists())
        self.assertFalse(list(self.vault.rglob("*.pending")))
        self.assertEqual(p.verify_recovery(self.vault, TEST_PASSWORD, test_only=True), audit["root_sha256"])
        with self.assertRaises(p.Refused):
            p.verify_recovery(self.vault, TEST_PASSWORD)
        with self.assertRaises(ValueError):
            p.verify_recovery(self.vault, b"wrong-ephemeral-password", test_only=True)

    def test_partial_write_never_publishes_public_root(self):
        original = p.persist_new
        count = 0
        def fail_second(path, content):
            nonlocal count
            count += 1
            if count == 2:
                raise OSError("simulated test disk failure")
            original(path, content)
        with patch.object(p, "persist_new", side_effect=fail_second), self.assertRaises(OSError):
            p.create_vault(self.vault, TEST_PASSWORD, test_only=True)
        self.assertFalse((self.vault / "public").exists())
        self.assertFalse((self.vault / "COMPLETE.json").exists())
        with self.assertRaises(p.Refused):
            p.create_vault(self.vault, TEST_PASSWORD, test_only=True)

    @unittest.skipIf(os.name == "nt", "Unix-only file-mode case; Windows DACL tested separately")
    def test_recovery_refuses_exposed_private_file(self):
        p.create_vault(self.vault, TEST_PASSWORD, test_only=True)
        private = self.vault / "private/root-1.encrypted.pk8.pem"
        private.chmod(0o644)
        with self.assertRaises(p.Refused):
            p.verify_recovery(self.vault, TEST_PASSWORD, test_only=True)

    def test_windows_acl_audit_refuses_broad_file_grant(self):
        acl = {"user": "S-1-5-21-TEST", "owner": "S-1-5-21-TEST", "rules": [
            {"sid": "S-1-5-21-TEST", "type": 0, "propagation": 0},
            {"sid": "S-1-1-0", "type": 0, "propagation": 0}]}
        fake_result = SimpleNamespace(stdout=json.dumps(acl).encode())
        with patch.object(p, "windows_system_directory", return_value=self.parent / "trusted-system"), patch.object(p.subprocess, "run", return_value=fake_result) as run:
            with self.assertRaises(p.Refused):
                p.windows_private_directory(self.parent / "encrypted-test-file")
            command = run.call_args.args[0]
            self.assertEqual(command[0], str(self.parent / "trusted-system/WindowsPowerShell/v1.0/powershell.exe"))
    @unittest.skipUnless(os.environ.get("TUF_ROOT_PROBE"), "debug Rust probe has not been built")
    def test_tough_accepts_root_and_profile_rejects_tampering(self):
        records, root = p.generate_material(TEST_PASSWORD, dt.datetime.now(dt.timezone.utc) - dt.timedelta(seconds=1))
        bundle = fixture_bundle(records, root)
        cases = [("valid", bundle, True)]
        for name, mutate in (
            ("below-threshold", lambda r: r.update(signatures=r["signatures"][:1])),
            ("corrupted-signature", lambda r: r["signatures"][0].update(sig="00" * 64) or r["signatures"][1].update(sig="00" * 64)),
            ("root-body-tamper", lambda r: r["signed"].update(version=2)),
        ):
            changed = json.loads(json.dumps(bundle))
            changed_root = json.loads(changed["root"])
            mutate(changed_root)
            changed["root"] = p.json_bytes(changed_root).decode()
            changed["public_bundle"] = p.json_bytes(p.public_bundle(changed["root"].encode())).decode()
            cases.append((name, changed, False))
        for name, field, value in (
            ("wrong-origin", "repository_origin", "https://example.invalid"),
            ("wrong-root-length", "root_length", 1),
            ("private-manifest-field", "private_key", "FORBIDDEN-EPHEMERAL-TEST-FIELD"),
        ):
            changed = json.loads(json.dumps(bundle))
            manifest = json.loads(changed["public_bundle"])
            manifest[field] = value
            changed["public_bundle"] = p.json_bytes(manifest).decode()
            cases.append((name, changed, False))
        for name, mutate in (
            ("expired-root", lambda r: r["signed"].update(issued=p.utc(dt.datetime.now(dt.timezone.utc)-dt.timedelta(days=366)), expires=p.utc(dt.datetime.now(dt.timezone.utc)-dt.timedelta(days=1)))),
            ("future-root", lambda r: r["signed"].update(issued=p.utc(dt.datetime.now(dt.timezone.utc)+dt.timedelta(days=1)))),
            ("private-key-field", lambda r: next(iter(r["signed"]["keys"].values()))["keyval"].update(private="FORBIDDEN-EPHEMERAL-TEST-FIELD")),
            ("shared-role-key", lambda r: r["signed"]["roles"].update(targets=r["signed"]["roles"]["timestamp"])),
        ):
            changed = json.loads(json.dumps(bundle))
            changed_root = json.loads(changed["root"])
            mutate(changed_root)
            changed_root["signatures"] = [{"keyid": records[label]["id"], "sig": records[label]["key"].sign(p.canonical(changed_root["signed"])).hex()} for label in p.ROLES["root"]]
            changed["root"] = p.json_bytes(changed_root).decode()
            changed["public_bundle"] = p.json_bytes(p.public_bundle(changed["root"].encode())).decode()
            cases.append((name, changed, False))
        for name, case, success in cases:
            file = self.parent / f"EPHEMERAL-{name}.json"
            file.write_bytes(p.json_bytes(case))
            result = subprocess.run([os.environ["TUF_ROOT_PROBE"], str(file)], capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode == 0, success, f"{name}: {result.stderr}")


if __name__ == "__main__":
    unittest.main(verbosity=2)

#!/usr/bin/env python3
"""Offline operator tool. No key generation without --create and local prompts.

Never run production creation inside an agent terminal. The operator must review
the documented approval, run this themselves, and enter the password locally.
No networking, deployment, ACL mutation, automatic integration, or key rotation.
"""
from __future__ import annotations

import argparse
import base64
import ctypes
import datetime as dt
import getpass
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import warnings

ROLES = {"root": ("root-1", "root-2", "root-3"), "targets": ("targets",),
         "snapshot": ("snapshot",), "timestamp": ("timestamp",)}
SOURCE_ROOT = Path(__file__).resolve().parents[2]
REPARSE_POINT = 0x400


class Refused(Exception):
    """Expected fail-closed condition; messages contain no secrets."""


def canonical(value: object) -> bytes:
    """TUF canonical JSON for this tool's deliberately restricted ASCII values.

    Not a general canonical-JSON implementation: non-ASCII, escapes, floats,
    arbitrary input metadata, and negative integers are forbidden.
    """
    def check(item: object) -> None:
        if isinstance(item, str):
            if any(ord(c) < 32 or ord(c) > 126 or c in '\\"' for c in item):
                raise Refused("value outside restricted canonical JSON profile")
        elif isinstance(item, bool) or item is None:
            pass
        elif isinstance(item, int) and item >= 0:
            pass
        elif isinstance(item, list):
            for child in item:
                check(child)
        elif isinstance(item, dict) and all(isinstance(k, str) for k in item):
            for key, child in item.items():
                check(key)
                check(child)
        else:
            raise Refused("value outside restricted canonical JSON profile")
    check(value)
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode("ascii")


def json_bytes(value: object) -> bytes:
    return json.dumps(value, sort_keys=True, indent=2, ensure_ascii=True).encode("ascii") + b"\n"


def public_bundle(root_bytes: bytes) -> dict:
    return {"schema": 1, "product": "gloamere-image", "platform": "windows-x64", "channel": "stable",
            "repository_origin": "https://175.24.190.175", "publisher_policy": "tuf-authenticated-unsigned",
            "key_profile": "root-2-of-3-six-keys", "root_version": 1,
            "root_length": len(root_bytes), "root_sha256": hashlib.sha256(root_bytes).hexdigest()}


def utc(value: dt.datetime) -> str:
    return value.astimezone(dt.timezone.utc).replace(microsecond=0).strftime("%Y-%m-%dT%H:%M:%SZ")


def crypto():
    try:
        from cryptography.hazmat.primitives import serialization
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
    except ImportError:
        raise Refused("Install the reviewed optional cryptography operator dependency first") from None
    return serialization, Ed25519PrivateKey


def no_links(path: Path) -> None:
    """Inspect every existing ancestor without resolving a link or junction."""
    for component in reversed((path, *path.parents)):
        try:
            status = component.lstat()
        except FileNotFoundError:
            continue
        if stat.S_ISLNK(status.st_mode) or getattr(status, "st_file_attributes", 0) & REPARSE_POINT:
            raise Refused("symbolic links, junctions, and reparse points are forbidden")
        if not stat.S_ISDIR(status.st_mode):
            raise Refused("vault ancestors must be directories")


def windows_private_directory(path: Path) -> None:
    """Read-only Windows DACL audit, intentionally refusing unfamiliar grants."""
    quoted = str(path).replace("'", "''")
    script = (
        "$ErrorActionPreference='Stop';"
        "[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false);"
        f"$a=if([IO.File]::GetAttributes('{quoted}') -band [IO.FileAttributes]::Directory)"
        f"{{[IO.Directory]::GetAccessControl('{quoted}')}}"
        f"else{{[IO.File]::GetAccessControl('{quoted}')}};"
        "$u=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value;"
        "$o=$a.GetOwner([Security.Principal.SecurityIdentifier]).Value;"
        "$r=@($a.Access | ForEach-Object { @{"
        "sid=$_.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value;"
        "type=[int]$_.AccessControlType;propagation=[int]$_.PropagationFlags} });"
        "@{user=$u;owner=$o;protected=$a.AreAccessRulesProtected;rules=$r} | ConvertTo-Json -Compress -Depth 5"
    )
    try:
        result = subprocess.run(
            [str(windows_system_directory() / "WindowsPowerShell/v1.0/powershell.exe"),
             "-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand",
             base64.b64encode(script.encode("utf-16-le")).decode("ascii")],
            check=True, capture_output=True, timeout=20,
        )
        acl = json.loads(result.stdout.decode("utf-8-sig"))
        allowed = {acl["user"], "S-1-5-18", "S-1-5-32-544"}
        if acl["owner"] != acl["user"] or not acl["rules"]:
            raise Refused("vault parent must be owned by the current Windows user with a readable DACL")
        for rule in acl["rules"]:
            # InheritOnly does not grant access to this directory itself.
            # Do not ignore it: an unexpected grant can expose new child files.
            if rule["type"] == 0 and rule["sid"] not in allowed:
                if rule["sid"] == "S-1-3-0" and rule["propagation"] & 2:
                    continue
                raise Refused("Windows DACL grants another principal access; review locally, do not auto-change ACLs")
    except Refused:
        raise
    except (OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError):
        raise Refused("could not verify Windows directory ownership and DACL; no ACL was changed") from None


def windows_system_directory() -> Path:
    """Use the OS API, never PATH, cwd, SystemRoot, or other environment lookup."""
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    get_directory = kernel.GetSystemDirectoryW
    get_directory.argtypes = [ctypes.c_wchar_p, ctypes.c_uint]
    get_directory.restype = ctypes.c_uint
    buffer = ctypes.create_unicode_buffer(32768)
    size = get_directory(buffer, len(buffer))
    if not size or size >= len(buffer):
        raise Refused("cannot resolve trusted Windows system directory")
    return Path(buffer.value)


def publish_no_replace(pending: Path, path: Path) -> None:
    if os.name == "nt":
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        move = kernel.MoveFileExW
        move.argtypes = [ctypes.c_wchar_p, ctypes.c_wchar_p, ctypes.c_uint]
        move.restype = ctypes.c_int
        # No REPLACE_EXISTING and no COPY_ALLOWED. Both paths are on the same
        # reviewed local filesystem. WRITE_THROUGH requests on-disk completion.
        if not move(str(pending), str(path), 0x8):
            raise ctypes.WinError(ctypes.get_last_error())
    else:
        os.link(pending, path, follow_symlinks=False)
        pending.unlink()


def private_directory(path: Path) -> None:
    no_links(path)
    status = path.lstat()
    if os.name == "nt":
        windows_private_directory(path)
    elif status.st_uid != os.getuid() or status.st_mode & 0o077:
        raise Refused("vault parent must be owned by this user and have no group/other permissions")


def private_file(path: Path) -> None:
    """A private directory does not prove a copied file has a private DACL."""
    no_links(path.parent)
    status = path.lstat()
    if not stat.S_ISREG(status.st_mode) or getattr(status, "st_file_attributes", 0) & REPARSE_POINT or status.st_nlink != 1:
        raise Refused("private key must be an ordinary unlinked file")
    if os.name == "nt":
        windows_private_directory(path)  # Get-Acl audits the named object's DACL.
    elif status.st_uid != os.getuid() or status.st_mode & 0o077:
        raise Refused("private key must be owned by this user with no group/other permissions")


def validate_vault(path: Path, *, test_only: bool = False) -> Path:
    if not path.is_absolute() or ".." in path.parts or len(path.parts) < 3:
        raise Refused("provide an absolute, non-root path without parent traversal")
    if os.name == "nt":
        if str(path).startswith(("\\\\", "//")):
            raise Refused("network shares and device namespace paths are forbidden")
        reserved = {"CON", "PRN", "AUX", "NUL", *(f"COM{i}" for i in range(1, 10)), *(f"LPT{i}" for i in range(1, 10))}
        if any(p.endswith((".", " ")) or ":" in p or p.split(".")[0].upper() in reserved for p in path.parts[1:]):
            raise Refused("ambiguous Windows path component")
    no_links(path)
    if path.exists():
        raise Refused("vault already exists; empty directories also cannot be reused")
    if not test_only:
        home = Path.home()
        # A narrow allowlist avoids accidentally writing secrets into a repo,
        # synced Documents/Desktop, temporary storage, or a server share.
        expected = home / "GloamereReleaseKeys" / "v0.7"
        if path != expected or path.is_relative_to(SOURCE_ROOT):
            raise Refused("production vault must be exactly the current user's GloamereReleaseKeys/v0.7")
        # Its parent must be pre-existing and privately owned. This script never
        # fixes permission failures or makes unapproved Windows ACL changes.
    if not path.parent.is_dir():
        raise Refused("create and review the private GloamereReleaseKeys parent locally first")
    private_directory(path.parent)
    return path


def sync_directory(path: Path) -> None:
    if os.name == "nt":
        return  # Python has no portable Windows directory FlushFileBuffers.
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0))
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def persist_new(path: Path, content: bytes) -> None:
    """Durable file contents, then atomic no-clobber publication in same folder."""
    private_directory(path.parent)
    pending = path.with_name(path.name + ".pending")
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(pending, flags, 0o600)
    with os.fdopen(fd, "wb") as out:
        out.write(content)
        out.flush()
        os.fsync(out.fileno())
    # Failure is fail-closed. Never fall back to an overwrite-capable replace.
    publish_no_replace(pending, path)
    sync_directory(path.parent)


def generate_material(password: bytes, now: dt.datetime) -> tuple[dict, dict]:
    """Use only after operator confirmation, or for disposable test material."""
    serialization, Ed25519PrivateKey = crypto()
    if len(password) < 16 or len(password) > 1024:
        raise Refused("passphrase must encode to between 16 and 1024 UTF-8 bytes")
    records = {}
    public_keys = {}
    for labels in ROLES.values():
        for label in labels:
            key = Ed25519PrivateKey.generate()
            public = {"keytype": "ed25519", "scheme": "ed25519", "keyval": {"public": key.public_key().public_bytes(
                serialization.Encoding.Raw, serialization.PublicFormat.Raw).hex()}}
            key_id = hashlib.sha256(canonical(public)).hexdigest()
            encrypted = key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8,
                                          serialization.BestAvailableEncryption(password))
            records[label] = {"key": key, "id": key_id, "encrypted": encrypted}
            public_keys[key_id] = public
    issued = now.astimezone(dt.timezone.utc).replace(microsecond=0)
    body = {"_type": "root", "spec_version": "1.0.0", "version": 1, "consistent_snapshot": True,
            "issued": utc(issued), "expires": utc(issued + dt.timedelta(days=365)),
            "keys": public_keys,
            "roles": {role: {"keyids": [records[label]["id"] for label in labels],
                             "threshold": 2 if role == "root" else 1} for role, labels in ROLES.items()}}
    root = {"signed": body, "signatures": [
        {"keyid": records[label]["id"], "sig": records[label]["key"].sign(canonical(body)).hex()}
        for label in ROLES["root"]]}
    return records, root


def create_vault(path: Path, password: bytes, *, test_only: bool = False) -> dict:
    path = validate_vault(path, test_only=test_only)
    # Reserve destination first. mkdir fails for any existing entry, including
    # a junction or a racing process. Failures intentionally preserve partial
    # encrypted output for operator inspection, never silently regenerate keys.
    path.mkdir(mode=0o700)
    private_directory(path)
    records, root = generate_material(password, dt.datetime.now(dt.timezone.utc))
    private = path / "private"
    private.mkdir(mode=0o700)
    for label, record in records.items():
        persist_new(private / f"{label}.encrypted.pk8.pem", record["encrypted"])
    # Prove each durable encrypted file decrypts with the supplied passphrase
    # and matches the planned public key before public trust is published.
    serialization, _ = crypto()
    for label, record in records.items():
        file = private / f"{label}.encrypted.pk8.pem"
        private_file(file)
        recovered = serialization.load_pem_private_key(file.read_bytes(), password)
        if recovered.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw) != record["key"].public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw):
            raise Refused("encrypted key readback failed; do not use this partial vault")
    sync_directory(private)
    public = path / "public"
    public.mkdir(mode=0o700)
    root_bytes = json_bytes(root)
    audit = {"format": "gloamere-tuf-key-audit-v1", "test_only": test_only,
             "status": "EPHEMERAL TEST ONLY; NEVER TRUST" if test_only else "generated; not integrated or deployed",
             "root_sha256": hashlib.sha256(root_bytes).hexdigest(),
             "issued": root["signed"]["issued"], "expires": root["signed"]["expires"],
             "roles": root["signed"]["roles"],
             "custody_warning": "All initial keys share one vault/passphrase. Separate offline root custody is still required."}
    persist_new(public / "root.json", root_bytes)
    persist_new(public / "public-bundle.json", json_bytes(public_bundle(root_bytes)))
    persist_new(public / "public-audit.json", json_bytes(audit))
    # Completion marker is last. Its absence means no output is eligible for use.
    persist_new(path / "COMPLETE.json", json_bytes({"root_sha256": audit["root_sha256"], "test_only": test_only}))
    sync_directory(path)
    return audit


def local_password() -> bytes:
    if not sys.stdin.isatty() or not sys.stderr.isatty():
        raise Refused("user action pending: open your own local interactive terminal; redirected input is forbidden")
    with warnings.catch_warnings():
        warnings.simplefilter("error", getpass.GetPassWarning)
        try:
            first = getpass.getpass("New encryption passphrase (hidden, at least 16 characters): ")
            second = getpass.getpass("Confirm encryption passphrase (hidden): ")
        except (getpass.GetPassWarning, EOFError):
            raise Refused("user action pending: secure local hidden password entry unavailable") from None
    if first != second or len(first) < 16:
        raise Refused("passphrase mismatch or fewer than 16 characters; no keys created")
    return first.encode("utf-8")


def verify_recovery(path: Path, password: bytes, *, test_only: bool = False) -> str:
    """Read-only recovery drill: decrypt only in memory, never export secrets."""
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
    serialization, _ = crypto()
    if not path.is_absolute() or ".." in path.parts or str(path).startswith(("\\\\", "//")):
        raise Refused("backup must be an absolute local path without traversal")
    private_directory(path)
    private_directory(path / "private")
    private_directory(path / "public")

    def read(relative: str) -> bytes:
        file = path / relative
        if relative.startswith("private/"):
            private_file(file)
        status = file.lstat()
        if not stat.S_ISREG(status.st_mode) or getattr(status, "st_file_attributes", 0) & REPARSE_POINT or status.st_nlink != 1:
            raise Refused("backup contains a non-regular, linked, or reparse file")
        if status.st_size > 65536:
            raise Refused("backup file exceeds size limit")
        fd = os.open(file, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
        with os.fdopen(fd, "rb") as stream:
            result = stream.read(65537)
        if len(result) > 65536:
            raise Refused("backup file exceeds size limit")
        return result

    complete = json.loads(read("COMPLETE.json"))
    audit = json.loads(read("public/public-audit.json"))
    root_bytes = read("public/root.json")
    root = json.loads(root_bytes)
    bundle = json.loads(read("public/public-bundle.json"))
    root_hash = hashlib.sha256(root_bytes).hexdigest()
    if complete.get("test_only") is not test_only or audit.get("test_only") is not test_only:
        raise Refused("test and production vaults must not be interchanged")
    if complete.get("root_sha256") != root_hash or audit.get("root_sha256") != root_hash or bundle != public_bundle(root_bytes):
        raise Refused("backup public manifest or completion marker does not match root")
    body = root["signed"]
    expected_ids = set()
    for role, labels in ROLES.items():
        ids = []
        for label in labels:
            encrypted = read(f"private/{label}.encrypted.pk8.pem")
            if not encrypted.startswith(b"-----BEGIN ENCRYPTED PRIVATE KEY-----"):
                raise Refused("backup private key is not encrypted PKCS#8 PEM")
            key = serialization.load_pem_private_key(encrypted, password)
            raw = key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw).hex()
            public = {"keytype": "ed25519", "scheme": "ed25519", "keyval": {"public": raw}}
            key_id = hashlib.sha256(canonical(public)).hexdigest()
            if body["keys"].get(key_id) != public:
                raise Refused("recovered key does not match trusted public root")
            ids.append(key_id)
        expected_ids.update(ids)
        if body["roles"].get(role) != {"keyids": ids, "threshold": 2 if role == "root" else 1}:
            raise Refused("backup role assignment differs from approved six-key profile")
    if len(expected_ids) != 6 or set(body["keys"]) != expected_ids:
        raise Refused("backup keys must be six distinct role-specific keys")
    verified = set()
    for signature in root["signatures"]:
        key_id = signature["keyid"]
        if key_id not in body["roles"]["root"]["keyids"]:
            raise Refused("unexpected root signature")
        Ed25519PublicKey.from_public_bytes(bytes.fromhex(body["keys"][key_id]["keyval"]["public"])).verify(
            bytes.fromhex(signature["sig"]), canonical(body))
        verified.add(key_id)
    if len(verified) < 2:
        raise Refused("root signature threshold not met")
    return root_hash


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vault", type=Path, default=Path.home() / "GloamereReleaseKeys" / "v0.7")
    actions = parser.add_mutually_exclusive_group()
    actions.add_argument("--create", action="store_true", help="locally create NEW keys only after explicit user approval and prompts")
    actions.add_argument("--verify-backup", action="store_true", help="read-only local recovery drill; password stays in memory")
    args = parser.parse_args(argv)
    print("OFFLINE PLAN: six distinct Ed25519 keys; root 2 of 3; targets/snapshot/timestamp 1 of 1.")
    print(f"Encrypted local vault: {args.vault}")
    print("Public root: version 1, consistent snapshots, 365-day validity. No server or app changes.")
    print("One vault/passphrase is NOT independent threshold custody. Arrange separate offline root backups.")
    if not args.create and not args.verify_backup:
        print("DRY RUN: no keys generated, no files created. Existing trust must be reused or rotated, never replaced here.")
        return 0
    try:
        if args.verify_backup:
            password = local_password()
            root_hash = verify_recovery(args.vault, password)
            del password
            print("Read-only recovery verification passed. Public root SHA-256: " + root_hash)
            return 0
        validate_vault(args.vault)
        if not sys.stdin.isatty() or not sys.stderr.isatty():
            raise Refused("user action pending: run --create yourself in a local terminal after explicit approval")
        print("Proceed only if you have no existing TUF root to reuse, and approved this exact new-key action and storage path.")
        print("This script does not grant authorization. Do not run it inside a remote agent terminal or recorded session.")
        if input("Type CREATE NEW GLOAMERE TUF ROOT to confirm locally: ") != "CREATE NEW GLOAMERE TUF ROOT":
            raise Refused("local creation confirmation not given; no keys created")
        password = local_password()
        audit = create_vault(args.vault, password)
        del password
        print("Encrypted vault complete. Public root SHA-256: " + audit["root_sha256"])
        print("Keep the private folder offline; verify a separate recovery copy before any integration.")
        return 0
    except (Exception, KeyboardInterrupt) as error:
        # Do not print arbitrary crypto/OS exception details or secret-bearing data.
        message = str(error) if isinstance(error, Refused) else "operation stopped; inspect for a partial vault locally; never reuse or overwrite it"
        print("REFUSED: " + message, file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())

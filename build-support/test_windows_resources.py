#!/usr/bin/env python3
"""Structural verifier tests using synthetic PE fixtures, not native PE evidence."""
import copy
import importlib.util
from pathlib import Path
import struct
import unittest

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("verify", HERE / "verify_windows_resources.py")
verify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify)


def version_block(key, value=b"", children=(), text=False):
    data = bytearray(b"\0" * 6 + (key + "\0").encode("utf-16-le"))
    data += b"\0" * (-len(data) % 4)
    data += value
    for child in children:
        data += b"\0" * (-len(data) % 4)
        data += child
    struct.pack_into("<HHH", data, 0, len(data), len(value) // 2 if text else len(value), int(text))
    return bytes(data)


def sample_resources(filename="gloamere-image.exe", version="0.4.0"):
    icon = (HERE.parent / "assets/gloamere-image.ico").read_bytes()
    frames = verify.icon_frames(icon)
    group = bytearray(struct.pack("<HHH", 0, 1, len(frames)))
    resources = {}
    for resource_id, (size, image) in enumerate(sorted(frames.items()), 1):
        group += struct.pack("<BBBBHHIH", size % 256, size % 256, 0, 0, 1, 32, len(image), resource_id)
        resources[(3, resource_id, 1033)] = image
    resources[(14, 1, 1033)] = bytes(group)
    fields = {
        "CompanyName": "gloamere-image contributors",
        "ProductName": "gloamere-image", "FileDescription": verify.IDENTITIES[filename],
        "OriginalFilename": filename, "InternalName": filename.removesuffix(".exe"),
        "FileVersion": version, "ProductVersion": version,
    }
    strings = [version_block(k, (v + "\0").encode("utf-16-le"), text=True) for k, v in fields.items()]
    info = version_block("StringFileInfo", children=[version_block("040904B0", children=strings, text=True)], text=True)
    var = version_block("VarFileInfo", children=[version_block("Translation", struct.pack("<HH", 1033, 1200))], text=True)
    major, minor, patch = map(int, version.split('.'))
    high, low = major << 16 | minor, patch << 16
    fixed = struct.pack("<13I", 0xFEEF04BD, 0x10000, high, low, high, low, 0x3F, 0, 0x40004, 1, 0, 0, 0)
    resources[(16, 1, 1033)] = version_block("VS_VERSION_INFO", fixed, [info, var])
    return resources, icon


def sample_pe(resources):
    tree = {}
    for (kind, name, language), value in resources.items():
        tree.setdefault(kind, {}).setdefault(name, {})[language] = value
    rsrc = bytearray()

    def allocate(size):
        rsrc.extend(b"\0" * (-len(rsrc) % 4))
        start = len(rsrc)
        rsrc.extend(b"\0" * size)
        return start

    def directory(node):
        start = allocate(16 + 8 * len(node))
        struct.pack_into("<HH", rsrc, start + 12, 0, len(node))
        for i, (key, value) in enumerate(sorted(node.items())):
            if isinstance(value, dict):
                entry = directory(value) | 0x80000000
            else:
                entry = allocate(16)
                content = allocate(len(value))
                rsrc[content:content + len(value)] = value
                struct.pack_into("<IIII", rsrc, entry, 0x1000 + content, len(value), 0, 0)
            struct.pack_into("<II", rsrc, start + 16 + 8 * i, key, entry)
        return start

    assert directory(tree) == 0
    resource_size = len(rsrc)
    rsrc.extend(b"\0" * (-len(rsrc) % 512))
    pe = bytearray(b"\0" * 512)
    pe[:2] = b"MZ"
    struct.pack_into("<I", pe, 0x3C, 0x80)
    pe[0x80:0x84] = b"PE\0\0"
    struct.pack_into("<HHIIIHH", pe, 0x84, 0x8664, 1, 0, 0, 0, 240, 0x22)
    optional = 0x98
    struct.pack_into("<H", pe, optional, 0x20B)
    struct.pack_into("<I", pe, optional + 108, 16)
    struct.pack_into("<II", pe, optional + 128, 0x1000, resource_size)
    section = optional + 240
    pe[section:section + 8] = b".rsrc\0\0\0"
    struct.pack_into("<IIII", pe, section + 8, resource_size, 0x1000, len(rsrc), 512)
    return bytes(pe + rsrc)


class ResourceTests(unittest.TestCase):
    def test_exact_main_and_worker_resources(self):
        for version in ("0.4.0", "0.5.0"):
            for filename in verify.IDENTITIES:
                with self.subTest(version=version, filename=filename):
                    resources, icon = sample_resources(filename, version)
                    parsed = verify.pe_resources(sample_pe(resources))
                    self.assertEqual(parsed, resources)
                    verify.check_resources(parsed, icon, filename, version)

    def test_reject_missing_icon_or_wrong_frame(self):
        resources, icon = sample_resources()
        missing = copy.copy(resources)
        del missing[(3, 1, 1033)]
        with self.assertRaisesRegex(ValueError, "RT_ICON"):
            verify.check_resources(missing, icon, "gloamere-image.exe", "0.4.0")
        wrong = copy.copy(resources)
        wrong[(3, 1, 1033)] = bytes(len(wrong[(3, 1, 1033)]))
        with self.assertRaisesRegex(ValueError, "does not match"):
            verify.check_resources(wrong, icon, "gloamere-image.exe", "0.4.0")

    def test_reject_old_identity_or_wrong_version(self):
        resources, icon = sample_resources()
        with self.assertRaisesRegex(ValueError, "unexpected executable"):
            verify.check_resources(resources, icon, "light-image-tool.exe", "0.4.0")
        with self.assertRaisesRegex(ValueError, "version mismatch"):
            verify.check_resources(resources, icon, "gloamere-image.exe", "0.5.0")
        with self.assertRaisesRegex(ValueError, "FileDescription|OriginalFilename|InternalName"):
            verify.check_resources(resources, icon, "gloamere-image-worker.exe", "0.4.0")

    def test_reject_fake_or_truncated_executable(self):
        for data in [b"", b"MZ" + b"\0" * 510, b"not PE"]:
            with self.assertRaises(ValueError):
                verify.pe_resources(data)
        resources, _icon = sample_resources()
        pe = sample_pe(resources)
        for length in [100, 511, len(pe) - 1024]:
            with self.assertRaises(ValueError):
                verify.pe_resources(pe[:length])

    def test_reject_malformed_version_tree(self):
        resources, _icon = sample_resources()
        version = bytearray(resources[(16, 1, 1033)])
        struct.pack_into("<H", version, 0, 65535)
        with self.assertRaises(ValueError):
            verify.version_node(bytes(version))


if __name__ == "__main__":
    unittest.main()

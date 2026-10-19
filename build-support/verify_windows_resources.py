#!/usr/bin/env python3
"""Verify native PE branding with Python's standard library (no EXE execution).

python build-support/verify_windows_resources.py --version 0.4.0 \
  target/release/gloamere-image.exe target/release/gloamere-image-worker.exe

This structural release check does not verify Explorer cache, taskbar behavior,
installer registration, or a Windows runtime. Run native acceptance tests too.
"""
from __future__ import annotations

import argparse
from pathlib import Path
import struct

SIZES = {16, 20, 24, 32, 40, 48, 64, 128, 256}
IDENTITIES = {
    "gloamere-image.exe": "gloamere-image",
    "gloamere-image-ui.exe": "gloamere-image viewer",
    "gloamere-image-worker.exe": "gloamere-image metadata and provenance worker",
}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def bounded(data: bytes, offset: int, size: int) -> bytes:
    require(offset >= 0 and size >= 0 and offset + size <= len(data), "truncated or out-of-range binary data")
    return data[offset:offset + size]


def unpack(fmt: str, data: bytes, offset: int = 0) -> tuple:
    return struct.unpack(fmt, bounded(data, offset, struct.calcsize(fmt)))


def pe_resources(data: bytes) -> dict[tuple[int, int, int], bytes]:
    require(bounded(data, 0, 2) == b"MZ", "not a DOS/PE executable")
    pe, = unpack("<I", data, 0x3C)
    require(bounded(data, pe, 4) == b"PE\0\0", "missing PE signature")
    sections, = unpack("<H", data, pe + 6)
    optional_size, = unpack("<H", data, pe + 20)
    optional = pe + 24
    magic, = unpack("<H", data, optional)
    require(magic in (0x10B, 0x20B), "unsupported PE optional header")
    directory_offset = 112 if magic == 0x20B else 96
    require(optional_size >= directory_offset + 24, "PE has no resource data directory")
    count, = unpack("<I", data, optional + directory_offset - 4)
    require(count >= 3, "PE has no resource data directory")
    resource_rva, resource_size = unpack("<II", data, optional + directory_offset + 16)
    require(resource_rva != 0 and resource_size >= 16, "PE has no resources")
    section_data = []
    for i in range(sections):
        entry = optional + optional_size + 40 * i
        vsize, address, raw_size, raw = unpack("<IIII", data, entry + 8)
        section_data.append((address, max(vsize, raw_size), raw, raw_size))

    def rva_bytes(address: int, size: int) -> bytes:
        for start, virtual_size, raw, raw_size in section_data:
            delta = address - start
            if 0 <= delta < virtual_size:
                require(delta + size <= raw_size, "resource extends beyond file-backed PE section")
                return bounded(data, raw + delta, size)
        raise ValueError("resource RVA is outside all sections")

    directory = rva_bytes(resource_rva, resource_size)
    result = {}
    visited = set()

    def walk(offset: int, keys: tuple[int, ...]) -> None:
        require(len(keys) < 3 and offset not in visited, "invalid resource directory depth/cycle")
        visited.add(offset)
        named, numeric = unpack("<HH", directory, offset + 12)
        require(named + numeric < 4096, "unreasonable resource directory size")
        for i in range(named + numeric):
            identifier, entry = unpack("<II", directory, offset + 16 + 8 * i)
            # Custom named resources are outside this check. Required Windows
            # icon/version resources use the numeric IDs established by build.rs.
            if identifier & 0x80000000:
                continue
            path = (*keys, identifier)
            if entry & 0x80000000:
                walk(entry & 0x7FFFFFFF, path)
            else:
                require(len(path) == 3, "resource data found at unexpected depth")
                address, size, _codepage, _reserved = unpack("<IIII", directory, entry)
                require(path not in result, "duplicate resource identity")
                result[path] = rva_bytes(address, size)
    walk(0, ())
    return result


def icon_frames(data: bytes) -> dict[int, bytes]:
    require(unpack("<HHH", data) == (0, 1, len(SIZES)), "ICO must have nine images")
    frames = {}
    for i in range(len(SIZES)):
        w, h, _colors, _reserved, _planes, depth, size, offset = unpack("<BBBBHHII", data, 6 + 16 * i)
        width, height = w or 256, h or 256
        require(width == height and width in SIZES and width not in frames and depth == 32, "incorrect ICO frame")
        frames[width] = bounded(data, offset, size)
    require(set(frames) == SIZES, "ICO is missing a required size")
    return frames


def aligned(value: int) -> int:
    return (value + 3) & ~3


def version_node(data: bytes, start: int = 0, limit: int | None = None) -> tuple[str, bytes, list]:
    limit = len(data) if limit is None else limit
    length, value_length, kind = unpack("<HHH", data, start)
    require(length >= 8 and start + length <= limit, "invalid VERSIONINFO block length")
    end = start + length
    offset = start + 6
    key_end = offset
    while key_end + 2 <= end and bounded(data, key_end, 2) != b"\0\0":
        key_end += 2
    require(key_end + 2 <= end, "unterminated VERSIONINFO key")
    key = bounded(data, offset, key_end - offset).decode("utf-16-le")
    value_start = aligned(key_end + 2)
    value_size = value_length * 2 if kind == 1 else value_length
    require(value_start + value_size <= end, "invalid VERSIONINFO value length")
    value = bounded(data, value_start, value_size)
    offset = aligned(value_start + value_size)
    children = []
    while offset + 6 <= end:
        child_length, = unpack("<H", data, offset)
        require(child_length > 0, "zero-length VERSIONINFO child")
        children.append(version_node(data, offset, end))
        offset = aligned(offset + child_length)
    return key, value, children


def check_resources(resources: dict, reference_icon: bytes, filename: str, version: str) -> None:
    require(filename in IDENTITIES, f"unexpected executable name: {filename}")
    expected_icons = icon_frames(reference_icon)
    groups = [(key, value) for key, value in resources.items() if key[0] == 14]
    require(len(groups) == 1 and groups[0][0][1] == 1, "expected exactly one RT_GROUP_ICON, ID 1")
    (group_type, _group_id, language), group = groups[0]
    require(group_type == 14 and unpack("<HHH", group) == (0, 1, len(SIZES)), "incorrect icon group")
    require(len(group) == 6 + 14 * len(SIZES), "incorrect icon group length")
    seen = set()
    ids = set()
    for i in range(len(SIZES)):
        w, h, _colors, _reserved, _planes, depth, length, resource_id = unpack("<BBBBHHIH", group, 6 + 14 * i)
        width, height = w or 256, h or 256
        require(width == height and width in SIZES and width not in seen and depth == 32, "incorrect icon group image")
        image = resources.get((3, resource_id, language))
        require(image is not None and len(image) == length, f"missing/truncated RT_ICON {resource_id}")
        require(image == expected_icons[width], f"RT_ICON {width}px does not match the committed app icon")
        seen.add(width)
        ids.add(resource_id)
    require(seen == SIZES and len(ids) == 9, "missing/duplicate icon group frame")
    require(len([key for key in resources if key[0] == 3]) == 9, "expected nine RT_ICON resources")
    versions = [(key, value) for key, value in resources.items() if key[0] == 16]
    require(len(versions) == 1 and versions[0][0][1] == 1, "expected exactly one RT_VERSION, ID 1")
    key, fixed, blocks = version_node(versions[0][1])
    require(key == "VS_VERSION_INFO" and len(fixed) == 52, "invalid VS_FIXEDFILEINFO")
    numbers = unpack("<13I", fixed)
    parts = [int(n) for n in version.split(".")]
    require(len(parts) == 3 and all(0 <= n <= 65535 for n in parts), "expected version must be major.minor.patch")
    expected_version = (parts[0] << 16 | parts[1], parts[2] << 16)
    require(numbers[0] == 0xFEEF04BD and numbers[2:4] == expected_version and numbers[4:6] == expected_version,
            "fixed product/file version mismatch")
    strings = {}
    for block_key, _value, tables in blocks:
        if block_key == "StringFileInfo":
            for table_key, _table_value, entries in tables:
                require(table_key == "040904B0", "unexpected VERSIONINFO language/codepage")
                for field, value, _children in entries:
                    require(field not in strings, "duplicate VERSIONINFO string")
                    strings[field] = value.decode("utf-16-le").rstrip("\0")
    expected = {
        "ProductName": "gloamere-image", "FileDescription": IDENTITIES[filename],
        "OriginalFilename": filename, "InternalName": filename.removesuffix(".exe"),
        "FileVersion": version, "ProductVersion": version,
    }
    for field, value in expected.items():
        require(strings.get(field) == value, f"{field}: expected {value!r}, got {strings.get(field)!r}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--icon", type=Path, default=Path(__file__).resolve().parents[1] / "assets/gloamere-image.ico")
    parser.add_argument("--version", default="0.4.0")
    parser.add_argument("executables", nargs="+", type=Path)
    arguments = parser.parse_args()
    reference = arguments.icon.read_bytes()
    for executable in arguments.executables:
        try:
            resources = pe_resources(executable.read_bytes())
            check_resources(resources, reference, executable.name, arguments.version)
        except (ValueError, OSError, struct.error, UnicodeError) as error:
            raise SystemExit(f"FAIL {executable}: {error}") from error
        print(f"PASS {executable.name}: 9 exact icon frames, RT_GROUP_ICON, RT_VERSION, gloamere-image {arguments.version}")


if __name__ == "__main__":
    main()

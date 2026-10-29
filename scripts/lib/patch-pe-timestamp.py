#!/usr/bin/env python3
"""Sets a PE (Windows) binary's COFF header TimeDateStamp to
SOURCE_DATE_EPOCH, as the last step of making a Windows release binary build
reproducibly (see `scripts/release-build.sh` and docs/design/release.md).

The linker `cargo zigbuild` uses for the Windows target writes the build
clock into this field, and does not accept the usual option for leaving it
out (`--no-insert-timestamp`), so two builds of the same commit differ in
exactly these four bytes. Setting it to the release commit's own date keeps
the field meaningful and makes it depend on the commit alone. The optional
header's CheckSum is left at zero, as the linker wrote it; a nonzero one is
refused rather than left stale.

Usage: patch-pe-timestamp.py PATH EPOCH

Exits 1, with a reason, on anything that is not a PE image.
"""

import struct
import sys


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: patch-pe-timestamp.py PATH EPOCH", file=sys.stderr)
        return 2
    path, epoch_text = sys.argv[1], sys.argv[2]
    epoch = int(epoch_text)
    if not 0 <= epoch < 2**32:
        print(f"{path}: epoch {epoch} does not fit a PE timestamp", file=sys.stderr)
        return 1

    with open(path, "r+b") as f:
        data = bytearray(f.read())
        if data[:2] != b"MZ" or len(data) < 0x40:
            print(f"{path}: not a PE image (no MZ header)", file=sys.stderr)
            return 1
        pe = struct.unpack_from("<I", data, 0x3C)[0]
        if data[pe : pe + 4] != b"PE\0\0":
            print(f"{path}: not a PE image (no PE signature)", file=sys.stderr)
            return 1
        optional = pe + 24
        magic = struct.unpack_from("<H", data, optional)[0]
        if magic not in (0x10B, 0x20B):
            print(f"{path}: unknown optional header magic {magic:#x}", file=sys.stderr)
            return 1
        checksum = struct.unpack_from("<I", data, optional + 64)[0]
        if checksum != 0:
            print(f"{path}: has a nonzero CheckSum, which this script would leave stale", file=sys.stderr)
            return 1
        struct.pack_into("<I", data, pe + 8, epoch)
        f.seek(0)
        f.write(data)

    print(f"{path}: TimeDateStamp set to {epoch}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

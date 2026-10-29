#!/usr/bin/env python3
"""Rewrites a Mach-O binary's `LC_UUID` load command to a deterministic
value, as the last step of making a macOS release binary build
reproducibly from two different checkout paths (see
`scripts/release-build.sh` and docs/design/release.md).

ld64 writes `LC_UUID` at link time as a hash of its own input, which
includes each object file's on-disk name; that name is derived from a
Cargo-internal hash of the crate's absolute source path, which
`--remap-path-prefix` cannot reach (it rewrites paths rustc embeds, not
Cargo's own build-plan file naming). Two checkouts of the same commit can
therefore still disagree in this one load command even once every other
byte matches. Passing `-Wl,-no_uuid` to drop it instead was tried and
rejected: this machine's dyld refuses to run a binary with no `LC_UUID` at
all ("missing LC_UUID load command"), so the command has to stay, and
instead gets overwritten, after stripping, with a hash of the binary's own
now-checkout-independent bytes.

Usage: patch-macho-uuid.py PATH

Exits 0 and prints why on anything that is not a 64-bit little-endian
Mach-O, or has no `LC_UUID`, since not every target this script builds is
one (only the two macOS targets are). The caller re-signs the binary
afterwards (ad-hoc signing over the new bytes), because macOS refuses to
run a binary whose code signature no longer matches its content.
"""

import hashlib
import struct
import sys

MACHO64_MAGIC = 0xFEEDFACF
LC_UUID = 0x1B


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: patch-macho-uuid.py PATH", file=sys.stderr)
        return 2
    path = sys.argv[1]

    with open(path, "r+b") as f:
        data = bytearray(f.read())

        magic = struct.unpack_from("<I", data, 0)[0]
        if magic != MACHO64_MAGIC:
            print(f"{path}: not a 64-bit little-endian Mach-O (magic {magic:#x}); leaving it alone")
            return 0

        ncmds = struct.unpack_from("<I", data, 16)[0]
        offset = 32  # size of the Mach-O 64-bit header
        uuid_offset = None
        for _ in range(ncmds):
            cmd, cmdsize = struct.unpack_from("<II", data, offset)
            if cmd == LC_UUID:
                uuid_offset = offset + 8  # skip the load command's own cmd and cmdsize fields
                break
            offset += cmdsize

        if uuid_offset is None:
            print(f"{path}: no LC_UUID load command; leaving it alone")
            return 0

        # Zero the field being replaced before hashing, so the new value
        # is a function of everything else in the file and nothing more.
        data[uuid_offset : uuid_offset + 16] = b"\x00" * 16
        new_uuid = hashlib.sha256(bytes(data)).digest()[:16]
        data[uuid_offset : uuid_offset + 16] = new_uuid

        f.seek(0)
        f.write(data)

    print(f"{path}: LC_UUID set to {new_uuid.hex()}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

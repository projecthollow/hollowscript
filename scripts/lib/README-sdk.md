# HollowScript SDK

This archive is the HollowScript C library for one target, for embedding
HollowScript in a host program written in C, C++ or any language with a C
foreign function interface. The command line tool (`hollowscript`) and the
language server are in the separate CLI archive; the installers fetch only
that one.

## Contents

| Path | What |
|---|---|
| `include/hollowscript.h` | The C header: every function, type and status code, with its ownership and safety rules. |
| `lib/libhollowscript_capi.a` | The static library. |
| `lib/libhollowscript_capi.dylib` | The dynamic library (macOS only). |
| `lib/hollowscript_capi.dll`, `lib/libhollowscript_capi.dll.a` | The DLL and its import library (Windows only). |
| `LICENSE-APACHE`, `LICENSE-MIT`, `NOTICE` | HollowScript's own licence (Apache-2.0 OR MIT). |
| `THIRD-PARTY-LICENSES` | Every third-party component linked into these libraries, with its licence text. |

The Linux builds are for the static musl targets, which have no dynamic
library: link the static library into a program built for the same target.

## Using it

Call `hs_version()` first and compare it with the version of this archive,
to catch a host built against one release and run against another.

A static library built from Rust also needs the platform's own system
libraries at link time. Typical link lines:

- macOS: `cc host.c -Iinclude lib/libhollowscript_capi.a`
- Linux (musl): `musl-gcc -static host.c -Iinclude lib/libhollowscript_capi.a`
  (or `zig cc -target x86_64-linux-musl ...`)
- Windows (MinGW-w64): `gcc host.c -Iinclude lib/libhollowscript_capi.a -lws2_32 -luserenv -lntdll -lbcrypt`

If a link fails with undefined system symbols, the exact list for a target
is what `rustc --print native-static-libs` reports for a static library
built for it.

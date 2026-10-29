//! The bytecode container decoder and verifier on raw bytes, exactly as a
//! host that loads a container it did not itself produce would present it
//! (the WebAssembly and C ABIs both do this: see `capi.rs` and
//! `wasm_abi.rs`, which cover the same decode-and-verify step behind their
//! own boundary).
//!
//! The verifier is the one thing standing between untrusted bytes and the
//! VM (`docs/design/security.md`), so this target's property is the
//! strongest one in this workspace: a module `verify` accepts must never
//! make the VM panic or read out of bounds while running it. The container
//! reader already refuses every count that does not fit the remaining
//! buffer before allocating for it (`docs/design/bytecode.md`); this target
//! is what would catch a case that check missed.
//!
//! `data` is interpreted as a container directly, with its checksum fixed up
//! before it reaches the decoder (`hollowscript_bytecode::fix_checksum`):
//! the SHA-256 that guards a container's integrity is exactly as opaque to
//! a mutation fuzzer as a real attacker's forgery attempt should be, so
//! libFuzzer's coverage guidance essentially never mutates its way past
//! `ContainerError::ChecksumMismatch` on its own. Recomputing the digest
//! over whatever bytes the fuzzer produced keeps every other byte,
//! including the format version and every table that follows, exactly as
//! arbitrary as before, so this still exercises `read_header`'s other
//! checks and the whole of `decode` and `verify`, not only a narrower
//! slice of them.

#![no_main]

use hollowscript_bytecode::module::ExportKind;
use hollowscript_bytecode::verify::verify_bytes;
use hollowscript_core::{Control, Limits};
use hollowscript_vm::{CapabilityTable, Interpreter};
use libfuzzer_sys::fuzz_target;

fn limits() -> Limits {
    Limits {
        max_steps: 200_000,
        max_memory_bytes: 4_000_000,
        max_output_bytes: 100_000,
        max_call_depth: 64,
    }
}

/// The integers that break an implementation: the largest, and the ones
/// either side of a boundary.
const HOSTILE: [u32; 8] = [
    u32::MAX,
    u32::MAX - 1,
    0x7fff_ffff,
    0x8000_0000,
    0x00ff_ffff,
    0x1000_0000,
    0x0001_0000,
    0x0000_ffff,
];

/// Treats the last five bytes of `data` as an instruction, when the first of
/// them says so (its top bit), to overwrite four bytes of the rest, from a
/// position it gives, with one of [`HOSTILE`], and returns the rest so
/// changed. Byte-level mutation reaches `0xFFFF_FFFF` in a string id only by
/// chance, in four coordinated steps; this reaches it in one, which is how a
/// constant naming string `u32::MAX` (once enough to make the interpreter
/// size a cache to four billion entries) is found in seconds instead of not
/// at all. An input without the top bit is used exactly as given.
fn overwrite_with_a_hostile_integer(data: &[u8]) -> Vec<u8> {
    let Some(split) = data.len().checked_sub(5) else {
        return data.to_vec();
    };
    let (body, tail) = data.split_at(split);
    let (Some(&selector), Some(position)) = (tail.first(), tail.get(1..5)) else {
        return data.to_vec();
    };
    if selector & 0x80 == 0 || body.len() <= hollowscript_bytecode::container::HEADER_LEN {
        return data.to_vec();
    }
    let mut out = body.to_vec();
    let position = u32::from_le_bytes([position[0], position[1], position[2], position[3]]);
    let span = out.len() - hollowscript_bytecode::container::HEADER_LEN;
    let at = hollowscript_bytecode::container::HEADER_LEN + (position as usize) % span;
    let value = HOSTILE[usize::from(selector & 7)].to_le_bytes();
    for (offset, byte) in value.into_iter().enumerate() {
        if let Some(slot) = out.get_mut(at + offset) {
            *slot = byte;
        }
    }
    out
}

fuzz_target!(|data: &[u8]| {
    // Almost every random byte string fails the checksum long before it
    // reaches any of the verifier's own rules (see `fix_checksum`'s own
    // doc comment): fix it up so libFuzzer's coverage-guided search spends
    // its time on the format and the verifier, the actual boundary this
    // target exists to exercise, rather than on a 256-bit checksum it has
    // no way to land on by chance.
    let mut fixed = overwrite_with_a_hostile_integer(data);
    hollowscript_bytecode::fix_checksum(&mut fixed);
    let Ok(verified) = verify_bytes(&fixed) else {
        return;
    };
    // A module the verifier accepted must run safely, however it got here:
    // running its initialisers, and then every zero-argument function the
    // module exports, covers both the top-level statements every module has
    // and the functions a host would go on to call (section 12's `call`).
    // Every argument the verifier lets through is arbitrary too, so
    // zero-argument exports are what this target can call without also
    // having to fuzz a compatible argument list.
    let functions: Vec<u32> = verified
        .module()
        .exports
        .iter()
        .filter_map(|export| match &export.kind {
            ExportKind::Function {
                function,
                signature,
            } if signature.params.is_empty() => Some(*function),
            _ => None,
        })
        .collect();
    let mut interpreter = Interpreter::new(verified, CapabilityTable::new(Vec::new()));
    if interpreter
        .initialise(&limits(), &mut Control::none())
        .is_err()
    {
        return;
    }
    for function in functions {
        let _ = interpreter.call_export(function, &[], &limits(), &mut Control::none());
    }
});

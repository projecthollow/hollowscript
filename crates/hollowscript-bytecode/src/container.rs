//! The versioned container format.
//!
//! ```text
//! offset  size  field
//!      0     4  magic, the bytes HSBC
//!      4     2  format major, little endian
//!      6     2  format minor
//!      8     2  language major: the specification version the program targets
//!     10     2  language minor
//!     12     4  flags (bit 0: the module carries source positions)
//!     16     8  payload length in bytes
//!     24    32  SHA-256 of bytes 0 to 23 followed by the payload
//!     56     n  payload
//! ```
//!
//! Bytecode is not a stability promise. A reader accepts only the exact format
//! version it was built for and refuses every other, so the format can change
//! in any release. The payload holds the tables of a [`Module`] in a fixed
//! order, all integers little endian and every string prefixed by its length.
//! The reader never trusts a count: it checks it against the bytes that are
//! left and against absolute limits before it allocates.

use crate::module::{
    CapCall, Capability, CapabilityKind, CaptureSource, Constant, Export, ExportKind, Function,
    FunctionKind, Global, Module, ModuleEntry, NativeImport, Shape, Signature, SpanEntry, StringId,
    VariantDesc,
};
use crate::sha256::sha256;
use hollowscript_core::transfer::{TransferType, TypeRef, TypeTable, VariantDef};
use std::fmt;

/// The four bytes at the start of every container.
pub const MAGIC: [u8; 4] = *b"HSBC";

/// The format version this build reads and writes, as major and minor.
pub const FORMAT_VERSION: (u16, u16) = (0, 2);

/// The size of the header in bytes.
pub const HEADER_LEN: usize = 56;

// A count is bounded before it is used to allocate (`Reader::count` checks
// it against the bytes that are left, so a small file cannot claim a count
// that would overrun it), but a cheap-on-disk entry can still cost far more
// once decoded than it cost to write: an empty string is 4 bytes on disk
// (its zero length) but roughly 60 bytes as a `String` in a `Vec`, and a
// `TransferType::Int` is 1 byte on disk but pays for the size of the
// largest `TransferType` variant, roughly 56 bytes, once decoded. `MAX_TABLE`
// and `MAX_STRINGS` are the byte budget on the decoded table this buys back:
// chosen so that the worst case of either table (all-empty strings, all-Int
// types) stays within roughly the same envelope, tens of megabytes, as the
// other rather than letting the string table's higher cap make its own
// worst case four times larger for no reason tied to anything the format
// actually needs (measured: 16 MiB of empty strings peaked at 258 MiB
// before this budget; the same shape of container now peaks at a quarter
// of that).
const MAX_STRINGS: usize = 1 << 20;
const MAX_FUNCTIONS: usize = 1 << 20;
const MAX_CODE_WORDS: usize = 1 << 24;
const MAX_TABLE: usize = 1 << 20;

/// A container that cannot be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContainerError {
    /// The input ends before the structure it describes.
    Truncated,
    /// The first four bytes are not the magic.
    BadMagic,
    /// The format version is not the one this build reads.
    UnsupportedVersion {
        /// The version in the header.
        found: (u16, u16),
    },
    /// The payload length in the header is not the number of bytes that follow.
    LengthMismatch,
    /// The SHA-256 in the header is not that of the payload.
    ChecksumMismatch,
    /// A count or a size is beyond the absolute limits of the format.
    TooLarge(&'static str),
    /// A tag, a flag or a text is not valid.
    Malformed(&'static str),
    /// Bytes remain after the last table.
    TrailingBytes,
}

impl fmt::Display for ContainerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContainerError::Truncated => write!(f, "the bytecode is truncated"),
            ContainerError::BadMagic => write!(f, "this is not HollowScript bytecode"),
            ContainerError::UnsupportedVersion { found } => write!(
                f,
                "the bytecode format {}.{} is not supported; this build reads {}.{}",
                found.0, found.1, FORMAT_VERSION.0, FORMAT_VERSION.1
            ),
            ContainerError::LengthMismatch => {
                write!(f, "the payload length does not match the file")
            }
            ContainerError::ChecksumMismatch => {
                write!(f, "the bytecode is corrupt: the checksum differs")
            }
            ContainerError::TooLarge(what) => write!(f, "the bytecode is too large: {what}"),
            ContainerError::Malformed(what) => write!(f, "the bytecode is malformed: {what}"),
            ContainerError::TrailingBytes => {
                write!(f, "the bytecode has bytes after its last table")
            }
        }
    }
}

impl std::error::Error for ContainerError {}

/// The fields of the header of a container.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Header {
    /// The format version.
    pub format: (u16, u16),
    /// The specification version the program targets.
    pub language: (u16, u16),
    /// Whether the module carries source positions.
    pub has_debug: bool,
    /// The length of the payload.
    pub payload_len: u64,
    /// The SHA-256 of the first 24 bytes of the header and the payload.
    pub digest: [u8; 32],
}

// ---------------------------------------------------------------------------
// Writing

#[derive(Default)]
struct Writer {
    out: Vec<u8>,
}

impl Writer {
    fn u8(&mut self, v: u8) {
        self.out.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }
    fn len(&mut self, n: usize) {
        self.u32(u32::try_from(n).unwrap_or(u32::MAX));
    }
    fn i64(&mut self, v: i64) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.out.extend_from_slice(&v.to_bits().to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        self.len(s.len());
        self.out.extend_from_slice(s.as_bytes());
    }
    fn sig(&mut self, s: &Signature) {
        self.len(s.params.len());
        for p in &s.params {
            self.u32(p.0);
        }
        match s.ret {
            Some(r) => {
                self.u8(1);
                self.u32(r.0);
            }
            None => self.u8(0),
        }
    }
}

fn write_type(w: &mut Writer, ty: &TransferType) {
    match ty {
        TransferType::Int => w.u8(0),
        TransferType::Float => w.u8(1),
        TransferType::Bool => w.u8(2),
        TransferType::String => w.u8(3),
        TransferType::Filter => w.u8(4),
        TransferType::List(t) => {
            w.u8(5);
            w.u32(t.0);
        }
        TransferType::Map(k, v) => {
            w.u8(6);
            w.u32(k.0);
            w.u32(v.0);
        }
        TransferType::Set(t) => {
            w.u8(7);
            w.u32(t.0);
        }
        TransferType::Optional(t) => {
            w.u8(8);
            w.u32(t.0);
        }
        TransferType::Record(fields) => {
            w.u8(9);
            w.len(fields.len());
            for (name, t) in fields {
                w.str(name);
                w.u32(t.0);
            }
        }
        TransferType::Enum { id, name, variants } => {
            w.u8(10);
            w.u32(id.module);
            w.u32(id.index);
            w.str(name);
            w.len(variants.len());
            for v in variants {
                w.str(&v.name);
                w.len(v.payload.len());
                for t in &v.payload {
                    w.u32(t.0);
                }
            }
        }
    }
}

fn write_function(w: &mut Writer, f: &Function) {
    w.u32(f.name.0);
    w.u8(match f.kind {
        FunctionKind::Function => 0,
        FunctionKind::Closure => 1,
        FunctionKind::ModuleInit => 2,
    });
    w.u16(f.arity);
    w.u16(f.register_count);
    w.u8(u8::from(f.returns_value));
    w.len(f.captures.len());
    for c in &f.captures {
        match c {
            CaptureSource::Register(r) => {
                w.u8(0);
                w.u16(*r);
            }
            CaptureSource::Upvalue(u) => {
                w.u8(1);
                w.u16(*u);
            }
        }
    }
    w.len(f.constants.len());
    for c in &f.constants {
        match c {
            Constant::Int(v) => {
                w.u8(0);
                w.i64(*v);
            }
            Constant::Float(v) => {
                w.u8(1);
                w.f64(*v);
            }
            Constant::Str(s) => {
                w.u8(2);
                w.u32(s.0);
            }
            Constant::Filter(s) => {
                w.u8(3);
                w.u32(s.0);
            }
        }
    }
    w.len(f.code.len());
    for word in &f.code {
        w.u32(*word);
    }
    w.len(f.spans.len());
    for s in &f.spans {
        for v in [s.pc, s.module, s.start, s.end, s.line, s.column] {
            w.u32(v);
        }
    }
}

/// Encodes a module as a container.
#[must_use]
pub fn encode(module: &Module) -> Vec<u8> {
    let mut w = Writer::default();
    w.len(module.strings.len());
    for s in &module.strings {
        w.str(s);
    }
    w.len(module.types.len());
    for t in module.types.entries() {
        write_type(&mut w, t);
    }
    w.len(module.shapes.len());
    for s in &module.shapes {
        w.len(s.fields.len());
        for f in &s.fields {
            w.u32(f.0);
        }
    }
    w.len(module.variants.len());
    for v in &module.variants {
        w.u32(v.enum_name.0);
        w.u32(v.name.0);
        w.u16(v.index);
        w.u16(v.arity);
        w.u32(v.decl_module);
        w.u32(v.decl_index);
    }
    w.len(module.natives.len());
    for n in &module.natives {
        w.u32(n.name.0);
        w.u8(n.arity);
    }
    w.len(module.capabilities.len());
    for c in &module.capabilities {
        w.u32(c.name.0);
        match &c.kind {
            CapabilityKind::Function(sig) => {
                w.u8(0);
                w.sig(sig);
            }
            CapabilityKind::Namespace(functions) => {
                w.u8(1);
                w.len(functions.len());
                for (name, sig) in functions {
                    w.u32(name.0);
                    w.sig(sig);
                }
            }
        }
    }
    w.len(module.cap_calls.len());
    for c in &module.cap_calls {
        w.u32(c.capability);
        w.u32(c.function);
    }
    w.len(module.functions.len());
    for f in &module.functions {
        write_function(&mut w, f);
    }
    w.len(module.modules.len());
    for m in &module.modules {
        w.u32(m.path.0);
        w.u32(m.init);
    }
    w.len(module.globals.len());
    for g in &module.globals {
        w.u32(g.module);
        w.u32(g.name.0);
        w.u8(u8::from(g.mutable));
    }
    w.len(module.exports.len());
    for e in &module.exports {
        w.u32(e.name.0);
        match &e.kind {
            ExportKind::Function {
                function,
                signature,
            } => {
                w.u8(0);
                w.u32(*function);
                w.sig(signature);
            }
            ExportKind::Constant { global, ty } => {
                w.u8(1);
                w.u32(*global);
                w.u32(ty.0);
            }
        }
    }
    let payload = w.out;
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&MAGIC);
    for v in [
        FORMAT_VERSION.0,
        FORMAT_VERSION.1,
        module.language.0,
        module.language.1,
    ] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&u32::from(module.has_debug_info()).to_le_bytes());
    out.extend_from_slice(
        &u64::try_from(payload.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    out.extend_from_slice(&digest_of(&out, &payload));
    out.extend_from_slice(&payload);
    out
}

/// The digest of the header fields that precede it and of the payload, so
/// that a changed version or length is detected as well as a changed payload.
fn digest_of(prefix: &[u8], payload: &[u8]) -> [u8; 32] {
    let mut data = Vec::with_capacity(24 + payload.len());
    data.extend_from_slice(prefix.get(..24).unwrap_or(prefix));
    data.extend_from_slice(payload);
    sha256(&data)
}

/// Overwrites `bytes[24..56]` (the checksum field) with the digest that
/// `read_header` would actually demand for the rest of `bytes` as given, in
/// place. Does nothing if `bytes` is shorter than [`HEADER_LEN`]: too short
/// to have a checksum field at all, and `read_header` already reports that
/// as [`ContainerError::Truncated`] on its own.
///
/// For the bytecode fuzz targets only (`fuzz/fuzz_targets/bytecode_verify.rs`
/// and the C and WebAssembly ABI targets that decode raw containers the same
/// way): almost every random byte string libFuzzer tries fails the checksum
/// before reaching any of the verifier's own rules, since the checksum is
/// 256 bits and a mutation has no way to land on the right one by chance.
/// Fixing it up first lets the fuzzer's coverage-guided search spend its
/// time on the format and the verifier instead of on guessing a SHA-256
/// preimage.
pub fn fix_checksum(bytes: &mut [u8]) {
    if bytes.len() < HEADER_LEN {
        return;
    }
    let (prefix, rest) = bytes.split_at_mut(24);
    let (digest_field, payload) = rest.split_at_mut(32);
    digest_field.copy_from_slice(&digest_of(prefix, payload));
}

// ---------------------------------------------------------------------------
// Reading

struct Reader<'a> {
    data: &'a [u8],
}

type Result<T> = std::result::Result<T, ContainerError>;

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.data.len() {
            return Err(ContainerError::Truncated);
        }
        let (head, tail) = self.data.split_at(n);
        self.data = tail;
        Ok(head)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.take(N)?
            .try_into()
            .map_err(|_| ContainerError::Truncated)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.array()?))
    }
    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(u64::from_le_bytes(self.array()?)))
    }
    /// A count of elements that each take at least `min_size` bytes, checked
    /// against the bytes that are left and against `limit` before any allocation.
    fn count(&mut self, min_size: usize, limit: usize, what: &'static str) -> Result<usize> {
        let n = self.u32()? as usize;
        if n > limit {
            return Err(ContainerError::TooLarge(what));
        }
        if n.checked_mul(min_size)
            .is_none_or(|bytes| bytes > self.data.len())
        {
            return Err(ContainerError::Truncated);
        }
        Ok(n)
    }
    fn string(&mut self) -> Result<String> {
        let n = self.count(1, usize::MAX >> 1, "string")?;
        let bytes = self.take(n)?;
        String::from_utf8(bytes.to_vec())
            .map_err(|_| ContainerError::Malformed("a string is not UTF-8"))
    }
    fn refs(&mut self) -> Result<Vec<TypeRef>> {
        let n = self.count(4, MAX_TABLE, "type list")?;
        (0..n).map(|_| self.u32().map(TypeRef)).collect()
    }
    fn sig(&mut self) -> Result<Signature> {
        let params = self.refs()?;
        let ret = match self.u8()? {
            0 => None,
            1 => Some(TypeRef(self.u32()?)),
            _ => return Err(ContainerError::Malformed("a result flag")),
        };
        Ok(Signature { params, ret })
    }
}

fn read_type(r: &mut Reader<'_>) -> Result<TransferType> {
    Ok(match r.u8()? {
        0 => TransferType::Int,
        1 => TransferType::Float,
        2 => TransferType::Bool,
        3 => TransferType::String,
        4 => TransferType::Filter,
        5 => TransferType::List(TypeRef(r.u32()?)),
        6 => TransferType::Map(TypeRef(r.u32()?), TypeRef(r.u32()?)),
        7 => TransferType::Set(TypeRef(r.u32()?)),
        8 => TransferType::Optional(TypeRef(r.u32()?)),
        9 => {
            let n = r.count(8, MAX_TABLE, "record fields")?;
            let mut fields = Vec::with_capacity(n);
            for _ in 0..n {
                fields.push((r.string()?, TypeRef(r.u32()?)));
            }
            TransferType::Record(fields)
        }
        10 => {
            let id = hollowscript_core::transfer::EnumId {
                module: r.u32()?,
                index: r.u32()?,
            };
            let name = r.string()?;
            let n = r.count(8, MAX_TABLE, "variants")?;
            let mut variants = Vec::with_capacity(n);
            for _ in 0..n {
                let name = r.string()?;
                variants.push(VariantDef {
                    name,
                    payload: r.refs()?,
                });
            }
            TransferType::Enum { id, name, variants }
        }
        _ => return Err(ContainerError::Malformed("a type tag")),
    })
}

fn read_function(r: &mut Reader<'_>) -> Result<Function> {
    let name = StringId(r.u32()?);
    let kind = match r.u8()? {
        0 => FunctionKind::Function,
        1 => FunctionKind::Closure,
        2 => FunctionKind::ModuleInit,
        _ => return Err(ContainerError::Malformed("a function kind")),
    };
    let arity = r.u16()?;
    let register_count = r.u16()?;
    let returns_value = match r.u8()? {
        0 => false,
        1 => true,
        _ => return Err(ContainerError::Malformed("a flag")),
    };
    let n = r.count(3, MAX_TABLE, "captures")?;
    let mut captures = Vec::with_capacity(n);
    for _ in 0..n {
        let tag = r.u8()?;
        let index = r.u16()?;
        captures.push(match tag {
            0 => CaptureSource::Register(index),
            1 => CaptureSource::Upvalue(index),
            _ => return Err(ContainerError::Malformed("a capture tag")),
        });
    }
    let n = r.count(5, MAX_TABLE, "constants")?;
    let mut constants = Vec::with_capacity(n);
    for _ in 0..n {
        constants.push(match r.u8()? {
            0 => Constant::Int(r.i64()?),
            1 => {
                let v = r.f64()?;
                if !v.is_finite() || (v == 0.0 && v.is_sign_negative()) {
                    return Err(ContainerError::Malformed("a Float constant"));
                }
                Constant::Float(v)
            }
            2 => Constant::Str(StringId(r.u32()?)),
            3 => Constant::Filter(StringId(r.u32()?)),
            _ => return Err(ContainerError::Malformed("a constant tag")),
        });
    }
    let n = r.count(4, MAX_CODE_WORDS, "code")?;
    let mut code = Vec::with_capacity(n);
    for _ in 0..n {
        code.push(r.u32()?);
    }
    let n = r.count(24, MAX_CODE_WORDS, "spans")?;
    let mut spans = Vec::with_capacity(n);
    for _ in 0..n {
        spans.push(SpanEntry {
            pc: r.u32()?,
            module: r.u32()?,
            start: r.u32()?,
            end: r.u32()?,
            line: r.u32()?,
            column: r.u32()?,
        });
    }
    Ok(Function {
        name,
        kind,
        arity,
        register_count,
        returns_value,
        captures,
        constants,
        code,
        spans,
    })
}

/// Reads and checks the header of a container.
///
/// # Errors
///
/// Returns the first problem: truncation, the magic, the version, the length
/// or the checksum.
pub fn read_header(bytes: &[u8]) -> Result<(Header, &[u8])> {
    let mut r = Reader { data: bytes };
    let prefix = bytes.get(..24).unwrap_or(bytes);
    if r.array::<4>()? != MAGIC {
        return Err(ContainerError::BadMagic);
    }
    let format = (r.u16()?, r.u16()?);
    if format != FORMAT_VERSION {
        return Err(ContainerError::UnsupportedVersion { found: format });
    }
    let language = (r.u16()?, r.u16()?);
    let flags = r.u32()?;
    if flags > 1 {
        return Err(ContainerError::Malformed("unknown flags"));
    }
    let payload_len = u64::from_le_bytes(r.array()?);
    let digest: [u8; 32] = r.array()?;
    if u64::try_from(r.data.len()).ok() != Some(payload_len) {
        return Err(ContainerError::LengthMismatch);
    }
    if digest_of(prefix, r.data) != digest {
        return Err(ContainerError::ChecksumMismatch);
    }
    Ok((
        Header {
            format,
            language,
            has_debug: flags == 1,
            payload_len,
            digest,
        },
        r.data,
    ))
}

/// Decodes a container into a module. The module is well formed as a byte
/// structure but has not been verified: pass it to [`crate::verify::verify`].
///
/// # Errors
///
/// Returns the first problem found.
pub fn decode(bytes: &[u8]) -> Result<Module> {
    let (header, payload) = read_header(bytes)?;
    let mut r = Reader { data: payload };
    let mut module = Module {
        language: header.language,
        ..Module::default()
    };
    let n = r.count(4, MAX_STRINGS, "strings")?;
    for _ in 0..n {
        module.strings.push(r.string()?);
    }
    let n = r.count(1, MAX_TABLE, "types")?;
    let mut types = TypeTable::new();
    for _ in 0..n {
        types.add(read_type(&mut r)?);
    }
    module.types = types;
    let n = r.count(4, MAX_TABLE, "shapes")?;
    for _ in 0..n {
        let m = r.count(4, MAX_TABLE, "shape fields")?;
        let fields = (0..m)
            .map(|_| r.u32().map(StringId))
            .collect::<Result<_>>()?;
        module.shapes.push(Shape { fields });
    }
    let n = r.count(20, MAX_TABLE, "variants")?;
    for _ in 0..n {
        module.variants.push(VariantDesc {
            enum_name: StringId(r.u32()?),
            name: StringId(r.u32()?),
            index: r.u16()?,
            arity: r.u16()?,
            decl_module: r.u32()?,
            decl_index: r.u32()?,
        });
    }
    let n = r.count(5, MAX_TABLE, "natives")?;
    for _ in 0..n {
        module.natives.push(NativeImport {
            name: StringId(r.u32()?),
            arity: r.u8()?,
        });
    }
    let n = r.count(5, MAX_TABLE, "capabilities")?;
    for _ in 0..n {
        let name = StringId(r.u32()?);
        let kind = match r.u8()? {
            0 => CapabilityKind::Function(r.sig()?),
            1 => {
                let m = r.count(9, MAX_TABLE, "capability functions")?;
                let mut functions = Vec::with_capacity(m);
                for _ in 0..m {
                    functions.push((StringId(r.u32()?), r.sig()?));
                }
                CapabilityKind::Namespace(functions)
            }
            _ => return Err(ContainerError::Malformed("a capability kind")),
        };
        module.capabilities.push(Capability { name, kind });
    }
    let n = r.count(8, MAX_TABLE, "capability calls")?;
    for _ in 0..n {
        module.cap_calls.push(CapCall {
            capability: r.u32()?,
            function: r.u32()?,
        });
    }
    let n = r.count(20, MAX_FUNCTIONS, "functions")?;
    for _ in 0..n {
        module.functions.push(read_function(&mut r)?);
    }
    let n = r.count(8, MAX_TABLE, "modules")?;
    for _ in 0..n {
        module.modules.push(ModuleEntry {
            path: StringId(r.u32()?),
            init: r.u32()?,
        });
    }
    let n = r.count(9, MAX_TABLE, "globals")?;
    for _ in 0..n {
        let module_index = r.u32()?;
        let name = StringId(r.u32()?);
        let mutable = match r.u8()? {
            0 => false,
            1 => true,
            _ => return Err(ContainerError::Malformed("a flag")),
        };
        module.globals.push(Global {
            module: module_index,
            name,
            mutable,
        });
    }
    let n = r.count(9, MAX_TABLE, "exports")?;
    for _ in 0..n {
        let name = StringId(r.u32()?);
        let kind = match r.u8()? {
            0 => ExportKind::Function {
                function: r.u32()?,
                signature: r.sig()?,
            },
            1 => ExportKind::Constant {
                global: r.u32()?,
                ty: TypeRef(r.u32()?),
            },
            _ => return Err(ContainerError::Malformed("an export kind")),
        };
        module.exports.push(Export { name, kind });
    }
    if !r.data.is_empty() {
        return Err(ContainerError::TrailingBytes);
    }
    Ok(module)
}

#[cfg(test)]
mod tests {
    use super::{ContainerError, FORMAT_VERSION, HEADER_LEN, decode, encode, read_header};
    use crate::module::{
        CapCall, Capability, CapabilityKind, CaptureSource, Constant, Export, ExportKind, Function,
        FunctionKind, Global, Module, ModuleEntry, NativeImport, Shape, Signature, SpanEntry,
        VariantDesc,
    };
    use crate::opcode::{Instruction, Op};
    use hollowscript_core::transfer::{TransferType, TypeRef, VariantDef};

    /// A module that uses every table.
    pub(crate) fn sample() -> Module {
        let mut m = Module::new();
        let main = m.add_string("main.hws").unwrap();
        let name = m.add_string("update").unwrap();
        let cap = m.add_string("print").unwrap();
        let field = m.add_string("x").unwrap();
        let native = m.add_string("string.length").unwrap();
        let text = m.add_string("hello").unwrap();
        let e = m.add_string("Shape").unwrap();
        let v = m.add_string("Circle").unwrap();
        let int = m.types.add(TransferType::Int);
        let float = m.types.add(TransferType::Float);
        let string = m.types.add(TransferType::String);
        m.types.add(TransferType::Optional(int));
        m.types.add(TransferType::Record(vec![("x".into(), int)]));
        m.types.add_enum(
            "Shape",
            vec![VariantDef {
                name: "Circle".into(),
                payload: vec![float],
            }],
        );
        m.shapes.push(Shape {
            fields: vec![field],
        });
        m.variants.push(VariantDesc {
            enum_name: e,
            name: v,
            index: 0,
            arity: 1,
            decl_module: hollowscript_core::transfer::HOST_MODULE,
            decl_index: 2,
        });
        m.natives.push(NativeImport {
            name: native,
            arity: 1,
        });
        m.capabilities.push(Capability {
            name: cap,
            kind: CapabilityKind::Function(Signature {
                params: vec![string],
                ret: None,
            }),
        });
        m.cap_calls.push(CapCall {
            capability: 0,
            function: 0,
        });
        let code = vec![
            Instruction::abx(Op::LoadK, 0, 0).encode(),
            Instruction::abx(Op::LoadK, 1, 1).encode(),
            Instruction::abc(Op::ReturnNone, 0, 0, 0).encode(),
        ];
        let entry = |init| ModuleEntry { path: main, init };
        for (i, kind) in [FunctionKind::ModuleInit, FunctionKind::Function]
            .into_iter()
            .enumerate()
        {
            m.functions.push(Function {
                name,
                kind,
                arity: 0,
                register_count: 2,
                returns_value: false,
                captures: if i == 1 {
                    vec![CaptureSource::Register(0), CaptureSource::Upvalue(1)]
                } else {
                    vec![]
                },
                constants: vec![
                    Constant::Int(-7),
                    Constant::Float(2.5),
                    Constant::Str(text),
                    Constant::Filter(text),
                ],
                code: code.clone(),
                spans: vec![SpanEntry {
                    pc: 0,
                    module: 0,
                    start: 4,
                    end: 9,
                    line: 2,
                    column: 3,
                }],
            });
        }
        m.modules.push(entry(0));
        m.globals.push(Global {
            module: 0,
            name,
            mutable: true,
        });
        m.exports.push(Export {
            name,
            kind: ExportKind::Function {
                function: 1,
                signature: Signature {
                    params: vec![float],
                    ret: None,
                },
            },
        });
        m.exports.push(Export {
            name: field,
            kind: ExportKind::Constant {
                global: 0,
                ty: TypeRef(0),
            },
        });
        m
    }

    #[test]
    fn a_module_survives_a_round_trip() {
        let m = sample();
        let bytes = encode(&m);
        assert_eq!(&bytes[..4], b"HSBC");
        assert_eq!(decode(&bytes), Ok(m.clone()));
        let (h, _) = read_header(&bytes).unwrap();
        assert_eq!(h.format, FORMAT_VERSION);
        assert!(h.has_debug);
        assert_eq!(
            usize::try_from(h.payload_len).unwrap(),
            bytes.len() - HEADER_LEN
        );
        assert!(!read_header(&encode(&m.stripped())).unwrap().0.has_debug);
    }

    #[test]
    fn corruption_is_detected() {
        let bytes = encode(&sample());
        for i in [0, 4, 8, 20, 30, HEADER_LEN, bytes.len() - 1] {
            let mut bad = bytes.clone();
            bad[i] ^= 0x55;
            assert!(decode(&bad).is_err(), "flipping byte {i} must be refused");
        }
        assert_eq!(
            decode(&bytes[..bytes.len() - 1]),
            Err(ContainerError::LengthMismatch)
        );
        assert_eq!(decode(&bytes[..10]), Err(ContainerError::Truncated));
        let mut extra = bytes.clone();
        extra.push(0);
        assert_eq!(decode(&extra), Err(ContainerError::LengthMismatch));
        assert_eq!(decode(b"NOPE"), Err(ContainerError::BadMagic));
        assert_eq!(decode(b"NO"), Err(ContainerError::Truncated));
    }

    #[test]
    fn another_format_version_is_refused() {
        let mut bytes = encode(&sample());
        bytes[4] = 9;
        assert_eq!(
            decode(&bytes),
            Err(ContainerError::UnsupportedVersion {
                found: (9, FORMAT_VERSION.1)
            })
        );
    }

    /// A cheap-on-disk entry (an empty string costs 4 bytes; a
    /// `TransferType::Int` costs 1) can cost far more once decoded, so the
    /// count caps are themselves the byte budget on the decoded table. 16
    /// MiB of empty strings measured a peak of 258 MiB before `MAX_STRINGS`
    /// was tightened to match `MAX_TABLE`'s envelope; this checks that the
    /// tightened cap, not merely some cap, is what is enforced.
    #[test]
    fn the_strings_table_budget_matches_the_types_table() {
        assert_eq!(
            super::MAX_STRINGS,
            super::MAX_TABLE,
            "a string costs roughly as much to decode as a type entry does, \
             so their worst-case decoded sizes should stay in the same envelope"
        );
    }

    #[test]
    fn hostile_counts_do_not_allocate() {
        // A valid header around a payload that claims four billion strings.
        let payload = u32::MAX.to_le_bytes().to_vec();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"HSBC");
        for v in [FORMAT_VERSION.0, FORMAT_VERSION.1, 1, 0] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        let digest = super::digest_of(&bytes, &payload);
        bytes.extend_from_slice(&digest);
        bytes.extend_from_slice(&payload);
        assert_eq!(decode(&bytes), Err(ContainerError::TooLarge("strings")));
    }
}

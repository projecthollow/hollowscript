//! The verifier.
//!
//! The VM runs only a [`VerifiedModule`], and the only way to make one is
//! [`verify`], so bytecode that did not pass cannot reach the interpreter,
//! whether it came from the compiler in the same process or from a file. The
//! browser VM contains this code and no compiler.
//!
//! The VM is written in safe Rust, so bytecode can never make it unsafe in the
//! memory sense whatever it contains. The verifier therefore does not re-check
//! the type system, which the checker did before the compiler ran. Values carry
//! their kind at run time and an instruction that meets the wrong kind faults
//! with HS1099. What the verifier does guarantee is what the VM relies on to
//! keep its promises about resources:
//!
//! 1. every table index, register, constant, jump target and run of registers
//!    is in range, so the interpreter never has to handle a missing operand;
//! 2. execution cannot fall off the end of a function;
//! 3. every cycle of the control flow passes through an instruction that
//!    charges steps, so at most 1,024 steps of work happen between two
//!    safepoints and a run can always be cancelled (section 11.7);
//! 4. closures capture only what exists, calls name functions of the right
//!    kind and arity, and exports and capabilities have transferable types.
//!
//! **What this deliberately does not cover.** `GetField`, `GetPayload`,
//! `ListGet` and `ListRest`'s immediate `c` operand indexes into a record's
//! field vector, an enum's payload vector or a list's element vector at run
//! time, a length this verifier cannot know statically (it depends on the
//! record's shape, the enum's variant or the list's own length, none of
//! which are visible without running the program). A well formed module, one
//! a correct compiler produced, never emits one of these without a preceding
//! check that `c` is in bounds for the value it will meet, but that is an
//! invariant of the compiler, not one this container format or `verify`
//! enforce or could enforce. The interpreter, once written, carries the
//! obligation this verifier cannot: it must range-check `c` against the
//! live value's actual length or arity every time it executes one of these
//! four opcodes, and fault (HS1099) rather than index directly.
//!
//! `Call` is the same kind of gap: it calls whatever closure is in a
//! register, so the callee, and therefore its arity, is only known at run
//! time. The verifier checks that the argument registers exist in the
//! caller's frame, not that their count matches the callee. A module with a
//! mismatch leaves the missing parameters as `none`, and the first
//! instruction that needs one of them faults (HS1099) on the value it finds.
//! A correct compiler never emits such a call, because the checker has
//! already typed the callee.

use crate::container::{ContainerError, decode};
use crate::module::{
    CapabilityKind, CaptureSource, Constant, ExportKind, FunctionKind, MAX_REGISTERS, Module,
};
use crate::opcode::{Format, Instruction, Op, Operand, Wide};
use hollowscript_core::transfer::{TransferType, TypeRef, TypeTable};
use std::collections::BTreeSet;
use std::fmt;

/// A module that passed the verifier.
#[derive(Clone, Debug, PartialEq)]
pub struct VerifiedModule {
    module: Module,
}

impl VerifiedModule {
    /// The module.
    #[must_use]
    pub fn module(&self) -> &Module {
        &self.module
    }

    /// The module, giving up the guarantee.
    #[must_use]
    pub fn into_module(self) -> Module {
        self.module
    }
}

/// A rule that a module breaks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifyError {
    /// The function, when the problem is inside one.
    pub function: Option<usize>,
    /// The instruction, when the problem is at one.
    pub pc: Option<usize>,
    /// The rule, in words.
    pub rule: &'static str,
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bytecode verification failed: {}", self.rule)?;
        if let Some(function) = self.function {
            write!(f, " (function {function}")?;
            if let Some(pc) = self.pc {
                write!(f, ", instruction {pc}")?;
            }
            write!(f, ")")?;
        }
        Ok(())
    }
}

impl std::error::Error for VerifyError {}

/// Bytes that are not acceptable bytecode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// The container is damaged, truncated or of another version.
    Container(ContainerError),
    /// The container is intact and the program breaks a rule.
    Verify(VerifyError),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Container(e) => e.fmt(f),
            LoadError::Verify(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for LoadError {}

/// Decodes and verifies a container.
///
/// # Errors
///
/// Returns the container error or the first rule that is broken.
pub fn verify_bytes(bytes: &[u8]) -> Result<VerifiedModule, LoadError> {
    let module = decode(bytes).map_err(LoadError::Container)?;
    verify(module).map_err(LoadError::Verify)
}

fn fail<T>(
    function: Option<usize>,
    pc: Option<usize>,
    rule: &'static str,
) -> Result<T, VerifyError> {
    Err(VerifyError { function, pc, rule })
}

/// The direct children of one type table entry: what `is_transferable`'s
/// walk (however it is driven) descends into. Shared by
/// [`transferability_table`]'s two passes so the structural match lives in
/// exactly one place.
fn type_children(types: &TypeTable, r: TypeRef) -> Vec<TypeRef> {
    match types.get(r) {
        None
        | Some(
            TransferType::Filter
            | TransferType::Int
            | TransferType::Float
            | TransferType::Bool
            | TransferType::String,
        ) => Vec::new(),
        Some(TransferType::List(t) | TransferType::Set(t) | TransferType::Optional(t)) => {
            vec![*t]
        }
        Some(TransferType::Map(k, v)) => vec![*k, *v],
        Some(TransferType::Record(fields)) => fields.iter().map(|(_, t)| *t).collect(),
        Some(TransferType::Enum { variants, .. }) => variants
            .iter()
            .flat_map(|v| v.payload.iter().copied())
            .collect(),
    }
}

/// Whether every type table entry can cross the host boundary as a result
/// or field: no `Filter`, and no dangling reference, reachable from it. An
/// enum may be recursive, so this is a whole-table reachability question
/// (does this entry's type graph reach a `Filter` or a missing entry
/// anywhere, however many other entries loop back through it first) rather
/// than a tree walk from one root, and it is answered once for the whole
/// table here rather than once per export or capability signature: a walk
/// from scratch for each of a module's exports, discarding what it had
/// already learned about entries it shares with every other export, made
/// verifying a 2.7 MB container with 2,000 exports over a long type chain
/// take 47 seconds. Computed by propagating "cannot transfer"
/// backwards from every `Filter` or dangling entry along reverse edges,
/// which handles a cycle exactly as correctly as a fresh per-root walk did
/// (revisiting an entry already known bad adds nothing new, so the
/// propagation queue only ever grows by the number of distinct entries,
/// never by the number of paths to them) while computing every entry's
/// answer in one pass over the whole table.
fn transferability_table(types: &TypeTable) -> Vec<bool> {
    let len = types.len();
    let mut predecessors: Vec<Vec<u32>> = vec![Vec::new(); len];
    for i in 0..len {
        for child in type_children(types, TypeRef(u32::try_from(i).unwrap_or(u32::MAX))) {
            if let Some(preds) = predecessors.get_mut(child.0 as usize) {
                preds.push(u32::try_from(i).unwrap_or(u32::MAX));
            }
        }
    }
    let mut cannot_transfer = vec![false; len];
    let mut queue: Vec<u32> = Vec::new();
    for (i, slot) in cannot_transfer.iter_mut().enumerate() {
        let r = TypeRef(u32::try_from(i).unwrap_or(u32::MAX));
        if matches!(types.get(r), None | Some(TransferType::Filter)) {
            *slot = true;
            queue.push(u32::try_from(i).unwrap_or(u32::MAX));
        }
    }
    while let Some(i) = queue.pop() {
        let Some(preds) = predecessors.get(i as usize) else {
            continue;
        };
        for &p in preds {
            if let Some(slot) = cannot_transfer.get_mut(p as usize) {
                if !*slot {
                    *slot = true;
                    queue.push(p);
                }
            }
        }
    }
    cannot_transfer.into_iter().map(|bad| !bad).collect()
}

/// Whether `r` can cross the host boundary, from a table
/// [`transferability_table`] already computed for the whole module. A
/// reference outside the table (which cannot happen for a `TypeRef` the
/// type table itself produced, but can for one a hostile container claims,
/// since nothing has range-checked it yet here) is not transferable, the
/// same answer a fresh walk would give it.
fn is_transferable(table: &[bool], r: TypeRef) -> bool {
    table.get(r.0 as usize).copied().unwrap_or(false)
}

/// Whether `text` is an identifier of the language's lexical grammar
/// (section 1.5, `identifier = ( letter | "_" ) { letter | digit | "_" }`, with
/// the letters ASCII): what a capability, a capability's function and an export
/// are called, because each is written as an identifier in a program (`uses
/// clock`, `clock.now()`, an exported `fn`). A name outside that grammar cannot
/// be reached from source, and one with a quote or a backslash in it breaks any
/// host that writes names into JSON or joins a namespace to a function with a
/// dot.
///
/// The keywords are not excluded here: the verifier does not know them (they
/// belong to the front end), and a keyword cannot be written where a name is
/// expected, so no program reaches one.
fn is_identifier(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Verifies a module.
///
/// # Errors
///
/// Returns the first rule that the module breaks.
pub fn verify(module: Module) -> Result<VerifiedModule, VerifyError> {
    verify_tables(&module)?;
    for (index, function) in module.functions.iter().enumerate() {
        verify_function(&module, index, function)?;
    }
    Ok(VerifiedModule { module })
}

fn verify_tables(m: &Module) -> Result<(), VerifyError> {
    let strings = m.strings.len();
    let string_ok = |id: crate::module::StringId| (id.0 as usize) < strings;
    if m.language.0 != 1 {
        return fail(
            None,
            None,
            "the program targets another major version of the language",
        );
    }
    if m.types.validate().is_err() {
        return fail(None, None, "the type table is malformed");
    }
    let refs_ok = |sig: &crate::module::Signature| {
        sig.params
            .iter()
            .chain(sig.ret.iter())
            .all(|t| (t.0 as usize) < m.types.len())
    };
    for shape in &m.shapes {
        let names: Vec<&str> = shape.fields.iter().map(|f| m.string(*f)).collect();
        if shape.fields.is_empty() || shape.fields.len() > MAX_REGISTERS - 1 {
            return fail(None, None, "a record shape has no fields or too many");
        }
        if !shape.fields.iter().all(|f| string_ok(*f))
            || names.windows(2).any(|w| matches!(w, [a, b] if a >= b))
        {
            return fail(
                None,
                None,
                "record shape fields must be strings, sorted and distinct",
            );
        }
    }
    if m.variants
        .iter()
        .any(|v| !string_ok(v.enum_name) || !string_ok(v.name))
    {
        return fail(None, None, "a variant names a string that does not exist");
    }
    // Every name a function carries, and every string a constant holds, is an
    // index into the string table, like every other name above. The VM sizes
    // nothing from one, but it reads them all, so an id the table does not
    // reach is a malformed module here and not a surprise there.
    if m.functions.iter().any(|f| !string_ok(f.name)) {
        return fail(None, None, "a function names a string that does not exist");
    }
    let mut native_names = BTreeSet::new();
    for n in &m.natives {
        if !string_ok(n.name) || !native_names.insert(m.string(n.name)) {
            return fail(None, None, "native imports must name distinct strings");
        }
    }
    let mut capability_names = BTreeSet::new();
    for c in &m.capabilities {
        if !string_ok(c.name)
            || !is_identifier(m.string(c.name))
            || !capability_names.insert(m.string(c.name))
        {
            return fail(None, None, "capability names must be distinct identifiers");
        }
        let sigs_ok = match &c.kind {
            CapabilityKind::Function(sig) => refs_ok(sig),
            CapabilityKind::Namespace(functions) => {
                let mut function_names = BTreeSet::new();
                !functions.is_empty()
                    && functions.iter().all(|(n, sig)| {
                        string_ok(*n)
                            && is_identifier(m.string(*n))
                            && function_names.insert(m.string(*n))
                            && refs_ok(sig)
                    })
            }
        };
        if !sigs_ok {
            return fail(None, None, "a capability interface is malformed");
        }
    }
    for call in &m.cap_calls {
        let ok = m
            .capabilities
            .get(call.capability as usize)
            .is_some_and(|c| match &c.kind {
                CapabilityKind::Function(_) => call.function == 0,
                CapabilityKind::Namespace(functions) => (call.function as usize) < functions.len(),
            });
        if !ok {
            return fail(
                None,
                None,
                "a capability call names a function that does not exist",
            );
        }
    }
    if m.modules.is_empty() {
        return fail(None, None, "a program has at least one module");
    }
    let mut paths = BTreeSet::new();
    for module in &m.modules {
        let init_ok = m
            .functions
            .get(module.init as usize)
            .is_some_and(|f| f.kind == FunctionKind::ModuleInit && f.arity == 0);
        if !string_ok(module.path) || !paths.insert(m.string(module.path)) || !init_ok {
            return fail(
                None,
                None,
                "a module needs a distinct path and an initialiser of no parameters",
            );
        }
    }
    if m.globals
        .iter()
        .any(|g| (g.module as usize) >= m.modules.len() || !string_ok(g.name))
    {
        return fail(
            None,
            None,
            "a global belongs to a module that does not exist",
        );
    }
    let entry = m.modules.len() - 1;
    // Computed once for the whole table, not once per export: see
    // `transferability_table`'s doc comment.
    let transferable = transferability_table(&m.types);
    let mut export_names = BTreeSet::new();
    for e in &m.exports {
        if !string_ok(e.name)
            || !is_identifier(m.string(e.name))
            || !export_names.insert(m.string(e.name))
        {
            return fail(None, None, "export names must be distinct identifiers");
        }
        match &e.kind {
            ExportKind::Function {
                function,
                signature,
            } => {
                let ok = m.functions.get(*function as usize).is_some_and(|f| {
                    f.kind == FunctionKind::Function
                        && usize::from(f.arity) == signature.params.len()
                        && f.returns_value == signature.ret.is_some()
                });
                let types_ok = signature
                    .params
                    .iter()
                    .chain(signature.ret.iter())
                    .all(|t| is_transferable(&transferable, *t));
                if !ok || !types_ok {
                    return fail(
                        None,
                        None,
                        "an exported function does not match its signature or is not transferable",
                    );
                }
            }
            ExportKind::Constant { global, ty } => {
                let ok = m
                    .globals
                    .get(*global as usize)
                    .is_some_and(|g| g.module as usize == entry && !g.mutable);
                if !ok || !is_transferable(&transferable, *ty) {
                    return fail(
                        None,
                        None,
                        "an exported constant is not a constant of the entry module or is not transferable",
                    );
                }
            }
        }
    }
    Ok(())
}

/// One instruction of a function's code: its [`Op::Ext`] prefix, if any, and
/// the word it widens, taken together.
#[derive(Copy, Clone, Debug)]
struct Unit {
    /// The pc of the unit's first word: the prefix when there is one. This
    /// is the only pc of the unit a jump may land on.
    start: usize,
    /// The pc just after the unit, which is also what a jump offset of the
    /// unit is relative to.
    next: usize,
    /// The instruction, widened.
    wide: Wide,
}

/// Splits a function's words into [`Unit`]s: every word is either a unit on
/// its own or the second half of an [`Op::Ext`] pair. An `Ext` must be
/// followed by an instruction it can widen, never by another `Ext`, by an
/// `Ax` instruction (whose one operand has no room for more), or by nothing;
/// and it may widen only the operands an `AsBx` word has room for (`a`, not
/// the signed offset, which a longer jump widens by being a `JumpLong`).
fn decode_units(at: Option<usize>, code: &[u32]) -> Result<Vec<Unit>, VerifyError> {
    let mut units = Vec::with_capacity(code.len());
    let mut pc = 0;
    while let Some(word) = code.get(pc) {
        let Ok(first) = Instruction::decode(*word) else {
            return fail(at, Some(pc), "an unknown opcode");
        };
        if first.op != Op::Ext {
            units.push(Unit {
                start: pc,
                next: pc + 1,
                wide: Wide::new(None, first),
            });
            pc += 1;
            continue;
        }
        let Some(Ok(main)) = code.get(pc + 1).map(|w| Instruction::decode(*w)) else {
            return fail(
                at,
                Some(pc),
                "an operand prefix must be followed by an instruction",
            );
        };
        let fits = match main.op.format() {
            _ if main.op == Op::Ext => false,
            Format::Abc | Format::Abx => true,
            Format::AsBx => first.b == 0 && first.c == 0,
            Format::Ax => false,
        };
        if !fits {
            return fail(
                at,
                Some(pc),
                "an operand prefix must widen an instruction with room for it",
            );
        }
        units.push(Unit {
            start: pc,
            next: pc + 2,
            wide: Wide::new(Some(first), main),
        });
        pc += 2;
    }
    Ok(units)
}

fn verify_function(
    m: &Module,
    index: usize,
    f: &crate::module::Function,
) -> Result<(), VerifyError> {
    let at = Some(index);
    let registers = usize::from(f.register_count);
    if registers == 0 || registers > MAX_REGISTERS || usize::from(f.arity) > registers {
        return fail(
            at,
            None,
            "the frame must have from 1 to 65535 registers and hold the parameters",
        );
    }
    if (f.kind != FunctionKind::Closure && !f.captures.is_empty())
        || f.captures.len() > MAX_REGISTERS - 1
    {
        return fail(at, None, "only a closure has upvalues, and at most 65534");
    }
    if f.code.is_empty() {
        return fail(at, None, "a function has code");
    }
    for constant in &f.constants {
        match constant {
            Constant::Str(id) | Constant::Filter(id) => {
                if (id.0 as usize) >= m.strings.len() {
                    return fail(
                        at,
                        None,
                        "a string or filter constant names a string that does not exist",
                    );
                }
            }
            // Section 3.4: a constant `Float` is finite, and negative zero is
            // not one.
            Constant::Float(x) => {
                if !x.is_finite() || (*x == 0.0 && x.is_sign_negative()) {
                    return fail(
                        at,
                        None,
                        "a Float constant must be finite and not negative zero",
                    );
                }
            }
            Constant::Int(_) => {}
        }
    }
    let units = decode_units(at, &f.code)?;
    // `unit_at[pc]` is the unit that starts at `pc`, or `None` for a pc that
    // is the second word of a prefixed pair: somewhere no jump may land.
    let mut unit_at: Vec<Option<usize>> = vec![None; f.code.len()];
    for (u, unit) in units.iter().enumerate() {
        if let Some(slot) = unit_at.get_mut(unit.start) {
            *slot = Some(u);
        }
    }
    for unit in &units {
        verify_instruction(m, index, f, &unit_at, *unit)?;
    }
    if units.last().is_none_or(|u| u.wide.op.falls_through()) {
        return fail(
            at,
            Some(units.last().map_or(0, |u| u.start)),
            "a function must end with a return, a trap or a jump",
        );
    }
    if f.spans
        .windows(2)
        .any(|w| matches!(w, [a, b] if a.pc > b.pc))
        || f.spans
            .iter()
            .any(|s| s.pc as usize >= f.code.len() || (s.module as usize) >= m.modules.len())
    {
        return fail(
            at,
            None,
            "source positions must be ordered and inside the function",
        );
    }
    check_cycles(index, &units, &unit_at)
}

/// Where an instruction jumps to, as a pc, or `None` for one that does not
/// jump. `next` is the pc just after the instruction, which its offset is
/// relative to.
///
/// The opcode decides, never the format: `Op::LoadInt` is `AsBx` too, and
/// its `sbx` is the value it loads. Reading that as an offset made
/// `LoadInt r, -1` look like a jump to itself, and
/// [`check_cycles`] refused every function holding one where no charge
/// intervened, which is a correct program the verifier rejected.
fn jump_target(next: usize, i: Wide) -> Option<i64> {
    if !i.op.is_jump() {
        return None;
    }
    let offset = match i.op.format() {
        Format::AsBx => i64::from(i.sbx()),
        Format::Ax => i64::from(i.ax()),
        Format::Abc | Format::Abx => return None,
    };
    i64::try_from(next).ok()?.checked_add(offset)
}

/// The unit a jump of `unit` lands on, if it lands on the start of one.
fn jump_unit(unit_at: &[Option<usize>], unit: Unit) -> Option<usize> {
    jump_target(unit.next, unit.wide)
        .and_then(|t| usize::try_from(t).ok())
        .and_then(|t| unit_at.get(t).copied().flatten())
}

#[allow(
    clippy::many_single_char_names,
    reason = "a, b and c are the operand names of the instruction formats"
)]
fn verify_instruction(
    m: &Module,
    findex: usize,
    f: &crate::module::Function,
    unit_at: &[Option<usize>],
    unit: Unit,
) -> Result<(), VerifyError> {
    let at = Some(findex);
    let here = Some(unit.start);
    let i = unit.wide;
    let registers = usize::from(f.register_count);
    let raw = match i.op.format() {
        Format::Abc => [usize::from(i.a), usize::from(i.b), usize::from(i.c)],
        Format::Abx => [usize::from(i.a), i.bx() as usize, 0],
        Format::AsBx => [usize::from(i.a), 0, 0],
        Format::Ax => [0, 0, 0],
    };
    for (slot, kind) in i.op.operands().into_iter().enumerate() {
        let value = raw.get(slot).copied().unwrap_or(0);
        let ok = match kind {
            Operand::None => value == 0,
            Operand::Reg => value < registers,
            Operand::Const => value < f.constants.len(),
            Operand::Imm | Operand::Jump => true,
            Operand::Func => value < m.functions.len(),
            Operand::Native => value < m.natives.len(),
            Operand::Cap => value < m.cap_calls.len(),
            Operand::Shape => value < m.shapes.len(),
            Operand::Variant => value < m.variants.len(),
            Operand::Global => value < m.globals.len(),
            Operand::Upval => value < f.captures.len(),
        };
        if !ok {
            return fail(at, here, "an operand is out of range");
        }
    }
    if i.op.is_jump() && jump_unit(unit_at, unit).is_none() {
        return fail(
            at,
            here,
            "a jump leaves the function or lands inside an instruction",
        );
    }
    // Runs of registers: the values that follow `a`, or that start at `b`.
    let a = usize::from(i.a);
    let (b, c, bx) = (usize::from(i.b), usize::from(i.c), i.bx() as usize);
    let run_ok = |end: usize| end <= registers;
    let ok = match i.op {
        Op::NewList => run_ok(a + 1 + bx),
        Op::ListAppend | Op::Interp => run_ok(b + c),
        Op::NewMap => run_ok(a + 1 + 2 * bx),
        Op::MapPut => run_ok(b + 2 * c),
        Op::NewRecord => m
            .shapes
            .get(bx)
            .is_some_and(|s| run_ok(a + 1 + s.fields.len())),
        Op::NewEnum => m
            .variants
            .get(bx)
            .is_some_and(|v| run_ok(a + 1 + usize::from(v.arity))),
        Op::Call => run_ok(a + 1 + b),
        Op::CallFn => m.functions.get(bx).is_some_and(|g| {
            g.kind == FunctionKind::Function && run_ok(a + 1 + usize::from(g.arity))
        }),
        Op::CallNative => m
            .natives
            .get(bx)
            .is_some_and(|n| run_ok(a + 1 + usize::from(n.arity))),
        Op::CallCap => cap_arity(m, bx).is_some_and(|n| run_ok(a + 1 + n)),
        Op::IterInit | Op::IterNext => run_ok(a + 3),
        Op::Charge => (1..=1024).contains(&bx),
        Op::ListCheck => c <= 1,
        Op::Closure => m.functions.get(bx).is_some_and(|g| {
            g.kind == FunctionKind::Closure
                && g.captures.iter().all(|s| match s {
                    CaptureSource::Register(r) => usize::from(*r) < registers,
                    CaptureSource::Upvalue(u) => usize::from(*u) < f.captures.len(),
                })
        }),
        Op::Return | Op::Try => f.returns_value || f.kind == FunctionKind::ModuleInit,
        Op::ReturnNone => !f.returns_value || f.kind == FunctionKind::ModuleInit,
        _ => true,
    };
    if !ok {
        return fail(
            at,
            here,
            "a run of registers, a call or a closure does not fit its operands",
        );
    }
    // A module's own init "function" has no declared return type (there is
    // nothing to declare one on), but section 4.7 allows a top-level `?`
    // exactly as it allows a top-level `return e`: on `Err`, it ends
    // initialisation with that value, dynamically typed. `returns_value` is
    // therefore not the right question for it, only for an ordinary
    // function or closure, which is what the check just above already
    // knows (it is the same exception `Op::Return` gets there).
    if i.op == Op::Try && !f.returns_value && f.kind != FunctionKind::ModuleInit {
        return fail(
            at,
            here,
            "the operator ? needs a function that returns a value",
        );
    }
    // `SetCell a, a`: whatever `R[a]` held is both the cell being mutated
    // and the value it is set to, so the cell is made to hold a handle to
    // itself directly. A retained-size or comparison walk over such a value
    // only terminates because `Heap::tree_size` is itself cycle-safe
    // (section 11.4); refusing the one case a syntactic check can see for
    // certain, without running anything, is cheap defence in depth ahead of
    // that walk. No correct compiler ever needs a cell to hold its own
    // handle, so this never rejects a program the checker would accept.
    if i.op == Op::SetCell && a == b {
        return fail(at, here, "a cell cannot hold itself");
    }
    Ok(())
}

/// The number of arguments of a capability call.
fn cap_arity(m: &Module, call: usize) -> Option<usize> {
    let c = m.cap_calls.get(call)?;
    match &m.capabilities.get(c.capability as usize)?.kind {
        CapabilityKind::Function(sig) => Some(sig.params.len()),
        CapabilityKind::Namespace(functions) => functions
            .get(c.function as usize)
            .map(|(_, s)| s.params.len()),
    }
}

/// Every cycle of the control flow must pass through an instruction that
/// charges steps. The instructions that do not charge are searched depth first
/// with an explicit stack, and a back edge among them is a cycle without a charge.
///
/// This is a search over *edges*, not merely over instructions, because one
/// instruction, `IterNext`, charges on only one of its two outgoing edges:
/// the fall-through edge taken when there is another element to visit (the
/// charge interp.rs makes before it hands back the element), never the jump
/// edge taken once the collection is exhausted (section 11.3: finding that
/// there is no next iteration is not one, so there is nothing to charge
/// for). Treating arrival at an `IterNext` as proof a charge already
/// happened, whichever edge led there, missed a cycle that loops entirely
/// through its uncharged exit edge; `edge_charges` below is asked about the
/// edge actually being taken, not about the instruction at its far end, so
/// that case is walked like any other uncharged instruction and a bare
/// cycle through it is caught.
fn check_cycles(
    function: usize,
    code: &[Unit],
    unit_at: &[Option<usize>],
) -> Result<(), VerifyError> {
    const WHITE: u8 = 0;
    const GREY: u8 = 1;
    const BLACK: u8 = 2;
    // Indexes here are units, not pcs: a prefixed pair is one node.
    let successors = |u: usize| -> [Option<usize>; 2] {
        let Some(unit) = code.get(u) else {
            return [None, None];
        };
        let next = (unit.wide.op.falls_through() && u + 1 < code.len()).then_some(u + 1);
        [next, jump_unit(unit_at, *unit)]
    };
    // Whether taking edge `edge` (0 = fall-through, 1 = jump) out of `pc`
    // is guaranteed to charge a step. For every charging opcode but
    // `IterNext`, charging happens unconditionally before the single edge
    // they have (they are none of them jumps), so which edge index is
    // irrelevant; `IterNext` charges on edge 0 (there is another element)
    // and never on edge 1 (the collection is exhausted).
    let edge_charges = |pc: usize, edge: usize| {
        code.get(pc).is_some_and(|u| {
            if u.wide.op == Op::IterNext {
                edge == 0
            } else {
                u.wide.op.charges()
            }
        })
    };
    let starts_charged = |pc: usize| {
        code.get(pc)
            .is_some_and(|u| u.wide.op.charges() && u.wide.op != Op::IterNext)
    };
    let mut colour = vec![WHITE; code.len()];
    for start in 0..code.len() {
        if starts_charged(start) || colour.get(start) != Some(&WHITE) {
            continue;
        }
        let mut stack = vec![(start, 0usize)];
        if let Some(c) = colour.get_mut(start) {
            *c = GREY;
        }
        while let Some(&(pc, edge)) = stack.last() {
            let succ = successors(pc).get(edge).copied().flatten();
            if edge >= 2 {
                if let Some(c) = colour.get_mut(pc) {
                    *c = BLACK;
                }
                stack.pop();
                continue;
            }
            let charged_edge = edge_charges(pc, edge);
            if let Some(top) = stack.last_mut() {
                top.1 += 1;
            }
            let Some(next) = succ else { continue };
            if charged_edge {
                continue;
            }
            match colour.get(next) {
                Some(&GREY) => {
                    return fail(
                        Some(function),
                        code.get(next).map(|u| u.start),
                        "a cycle of the control flow charges no steps",
                    );
                }
                Some(&WHITE) => {
                    if let Some(c) = colour.get_mut(next) {
                        *c = GREY;
                    }
                    stack.push((next, 0));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{LoadError, VerifyError, verify, verify_bytes};
    use crate::container::{ContainerError, encode};
    use crate::module::{
        CaptureSource, Constant, Export, ExportKind, Function, FunctionKind, Global, Module,
        ModuleEntry, Signature, StringId,
    };
    use crate::opcode::{Instruction, Op};
    use hollowscript_core::transfer::TransferType;

    fn function(m: &mut Module, kind: FunctionKind, code: Vec<Instruction>) -> usize {
        let name = m.add_string("f").unwrap();
        m.functions.push(Function {
            name,
            kind,
            arity: 0,
            register_count: 4,
            returns_value: false,
            captures: vec![],
            constants: vec![Constant::Int(1)],
            code: code.into_iter().map(Instruction::encode).collect(),
            spans: vec![],
        });
        m.functions.len() - 1
    }

    /// A module with one empty entry module.
    fn base(body: Vec<Instruction>) -> Module {
        let mut m = Module::new();
        let path = m.add_string("main.hws").unwrap();
        let init = function(&mut m, FunctionKind::ModuleInit, body);
        m.modules.push(ModuleEntry {
            path,
            init: u32::try_from(init).unwrap(),
        });
        m
    }

    fn rule(m: Module) -> &'static str {
        verify(m).unwrap_err().rule
    }

    const RET: Instruction = Instruction::abc(Op::ReturnNone, 0, 0, 0);
    const LEAVES: &str = "a jump leaves the function or lands inside an instruction";
    const FILLER: Instruction = Instruction::abc(Op::Move, 0, 1, 0);

    fn long(offset: i32) -> Instruction {
        Instruction::try_ax(Op::JumpLong, offset).unwrap()
    }

    /// `head`, then `count` uncharged fillers, then `tail`.
    fn padded(head: Vec<Instruction>, count: usize, tail: Vec<Instruction>) -> Module {
        let mut code = head;
        code.extend(std::iter::repeat_n(FILLER, count));
        code.extend(tail);
        base(code)
    }

    /// A jump may reach exactly as far as its encoding says and no further:
    /// the short form 32,767 instructions forward, the long form beyond it,
    /// to exactly the last instruction and not one past it.
    #[test]
    fn long_jumps_reach_past_the_short_encoding() {
        let short = padded(
            vec![Instruction::asbx(Op::Jump, 0, i16::MAX)],
            32_767,
            vec![RET],
        );
        assert!(verify(short).is_ok());
        for n in [32_768, 70_000] {
            let offset = i32::try_from(n).unwrap();
            assert!(verify(padded(vec![long(offset)], n, vec![RET])).is_ok());
            assert_eq!(rule(padded(vec![long(offset + 1)], n, vec![RET])), LEAVES);
        }
        // A conditional jump that reaches as far: inverted, over a `JumpLong`.
        let conditional = padded(
            vec![
                Instruction::abc(Op::LoadTrue, 0, 0, 0),
                Instruction::asbx(Op::JumpIf, 0, 1),
                long(40_000),
            ],
            40_000,
            vec![RET],
        );
        assert!(verify(conditional).is_ok());
        // Backwards to the first instruction, and one before it.
        let mut back = padded(vec![Instruction::abx(Op::Charge, 0, 1)], 40_000, vec![]);
        back.functions[0]
            .code
            .extend([long(-40_002), RET].map(Instruction::encode));
        assert!(verify(back.clone()).is_ok());
        back.functions[0].code[40_001] = long(-40_003).encode();
        assert_eq!(rule(back), LEAVES);
    }

    /// A long jump is still a jump to the cycle check: a loop closed by one
    /// needs a charge exactly as a loop closed by a short jump does.
    #[test]
    fn a_long_loop_needs_a_charge() {
        let mut spin = padded(vec![], 40_000, vec![]);
        spin.functions[0]
            .code
            .extend([long(-40_001), RET].map(Instruction::encode));
        assert_eq!(rule(spin), "a cycle of the control flow charges no steps");
        let mut paid = padded(vec![Instruction::abx(Op::Charge, 0, 1)], 40_000, vec![]);
        paid.functions[0]
            .code
            .extend([long(-40_002), RET].map(Instruction::encode));
        assert!(verify(paid).is_ok());
    }

    fn ext(a: u8, b: u8, c: u8) -> Instruction {
        Instruction::abc(Op::Ext, a, b, c)
    }

    /// A prefix widens the operands of the instruction after it, which are
    /// then checked against the frame like any other.
    #[test]
    fn a_prefix_widens_the_next_instruction() {
        let mut wide = base(vec![
            ext(1, 1, 0),
            Instruction::abc(Op::Move, 44, 0, 0),
            RET,
        ]);
        // `Move 300 256 0`: in range only once the frame is that large.
        assert_eq!(rule(wide.clone()), "an operand is out of range");
        wide.functions[0].register_count = 301;
        assert!(verify(wide.clone()).is_ok());
        wide.functions[0].register_count = 300;
        assert_eq!(rule(wide), "an operand is out of range");
        // A run that starts past the first 256 registers: `NewList 300 255`.
        let mut run = base(vec![
            ext(1, 0, 0),
            Instruction::abx(Op::NewList, 44, 255),
            RET,
        ]);
        run.functions[0].register_count = 556;
        assert!(verify(run.clone()).is_ok());
        run.functions[0].code[1] = Instruction::abx(Op::NewList, 44, 256).encode();
        assert_eq!(
            rule(run),
            "a run of registers, a call or a closure does not fit its operands"
        );
        // An `Abx` constant index widened past 16 bits.
        assert_eq!(
            rule(base(vec![
                ext(0, 1, 0),
                Instruction::abx(Op::LoadK, 0, 0),
                RET
            ])),
            "an operand is out of range"
        );
    }

    #[test]
    fn a_prefix_must_widen_something_it_fits() {
        const MISFIT: &str = "an operand prefix must widen an instruction with room for it";
        assert_eq!(
            rule(base(vec![RET, ext(0, 0, 0)])),
            "an operand prefix must be followed by an instruction"
        );
        assert_eq!(rule(base(vec![ext(0, 0, 0), ext(0, 0, 0), RET])), MISFIT);
        assert_eq!(rule(base(vec![ext(0, 0, 0), long(0), RET])), MISFIT);
        // An `AsBx` word has room for a wider `a`, not a wider offset.
        let mut test = base(vec![
            ext(1, 0, 0),
            Instruction::asbx(Op::JumpIfNot, 0, 0),
            RET,
        ]);
        test.functions[0].register_count = 257;
        assert!(verify(test.clone()).is_ok());
        test.functions[0].code[0] = ext(1, 1, 0).encode();
        assert_eq!(rule(test), MISFIT);
        let mut unknown = base(vec![ext(0, 0, 0), RET]);
        unknown.functions[0].code[1] = 0x0000_00EE;
        assert_eq!(
            rule(unknown),
            "an operand prefix must be followed by an instruction"
        );
    }

    /// A prefixed pair is one instruction: a jump lands on its prefix, and a
    /// jump that would land on the word after it is refused, as is a pair
    /// that ends the function without ending its control flow.
    #[test]
    fn a_jump_lands_on_a_prefix_and_never_inside_a_pair() {
        let mut to_pair = base(vec![
            Instruction::asbx(Op::Jump, 0, 0),
            ext(1, 0, 0),
            Instruction::abc(Op::Move, 0, 1, 0),
            RET,
        ]);
        to_pair.functions[0].register_count = 257;
        assert!(verify(to_pair.clone()).is_ok());
        to_pair.functions[0].code[0] = Instruction::asbx(Op::Jump, 0, 1).encode();
        assert_eq!(rule(to_pair), LEAVES);
        let mut back = base(vec![
            Instruction::abx(Op::Charge, 0, 1),
            ext(1, 0, 0),
            Instruction::asbx(Op::JumpIf, 0, -3),
            RET,
        ]);
        back.functions[0].register_count = 257;
        assert!(verify(back.clone()).is_ok());
        back.functions[0].code[2] = Instruction::asbx(Op::JumpIf, 0, -1).encode();
        assert_eq!(rule(back), LEAVES);
        let mut last = base(vec![ext(1, 0, 0), Instruction::abc(Op::Return, 0, 0, 0)]);
        last.functions[0].register_count = 257;
        last.functions[0].returns_value = true;
        assert!(verify(last.clone()).is_ok());
        last.functions[0].code[1] = Instruction::abc(Op::Move, 0, 0, 0).encode();
        assert_eq!(
            rule(last),
            "a function must end with a return, a trap or a jump"
        );
    }

    #[test]
    fn a_small_program_passes() {
        let m = base(vec![
            Instruction::abx(Op::Charge, 0, 1),
            Instruction::abx(Op::LoadK, 0, 0),
            RET,
        ]);
        assert!(verify(m.clone()).is_ok());
        assert_eq!(verify_bytes(&encode(&m)).unwrap().module(), &m);
    }

    #[test]
    fn operands_must_be_in_range() {
        let bad = |i| rule(base(vec![i, RET]));
        assert_eq!(
            bad(Instruction::abc(Op::Move, 0, 9, 0)),
            "an operand is out of range"
        );
        assert_eq!(
            bad(Instruction::abx(Op::LoadK, 0, 5)),
            "an operand is out of range"
        );
        assert_eq!(
            bad(Instruction::abx(Op::GetGlobal, 0, 0)),
            "an operand is out of range"
        );
        assert_eq!(
            bad(Instruction::abc(Op::Move, 0, 1, 7)),
            "an operand is out of range"
        );
        assert_eq!(
            bad(Instruction::abx(Op::NewList, 3, 2)),
            "a run of registers, a call or a closure does not fit its operands"
        );
        assert_eq!(
            bad(Instruction::abx(Op::Charge, 0, 0)),
            "a run of registers, a call or a closure does not fit its operands"
        );
        assert_eq!(
            bad(Instruction::abx(Op::Charge, 0, 1025)),
            "a run of registers, a call or a closure does not fit its operands"
        );
    }

    #[test]
    fn opcodes_and_endings_are_checked() {
        let mut m = base(vec![RET]);
        m.functions[0].code = vec![0x0000_00EE];
        assert_eq!(rule(m), "an unknown opcode");
        assert_eq!(
            rule(base(vec![Instruction::abx(Op::LoadK, 0, 0)])),
            "a function must end with a return, a trap or a jump"
        );
        assert_eq!(rule(base(vec![])), "a function has code");
    }

    #[test]
    fn jumps_stay_inside_the_function() {
        assert_eq!(
            rule(base(vec![Instruction::asbx(Op::Jump, 0, 5), RET])),
            LEAVES
        );
        assert_eq!(
            rule(base(vec![Instruction::asbx(Op::Jump, 0, -5), RET])),
            LEAVES
        );
        assert!(verify(base(vec![Instruction::asbx(Op::Jump, 0, 0), RET])).is_ok());
    }

    #[test]
    fn a_loop_needs_a_charge() {
        let spin = vec![
            Instruction::abc(Op::Move, 0, 1, 0),
            Instruction::asbx(Op::Jump, 0, -2),
            RET,
        ];
        assert_eq!(
            rule(base(spin)),
            "a cycle of the control flow charges no steps"
        );
        let paid = vec![
            Instruction::abx(Op::Charge, 0, 1),
            Instruction::asbx(Op::Jump, 0, -2),
            RET,
        ];
        assert!(verify(base(paid)).is_ok());
        let conditional = vec![
            Instruction::abc(Op::LoadTrue, 0, 0, 0),
            Instruction::asbx(Op::JumpIf, 0, -2),
            RET,
        ];
        assert_eq!(
            rule(base(conditional)),
            "a cycle of the control flow charges no steps"
        );
        let iterating = vec![
            Instruction::abc(Op::IterInit, 0, 3, 0),
            Instruction::asbx(Op::IterNext, 0, 1),
            Instruction::asbx(Op::Jump, 0, -2),
            RET,
        ];
        assert!(verify(base(iterating)).is_ok());
    }

    #[test]
    fn closures_capture_only_what_exists() {
        let mut m = base(vec![Instruction::abx(Op::Closure, 0, 1), RET]);
        let inner = function(&mut m, FunctionKind::Closure, vec![RET]);
        m.functions[inner].captures = vec![CaptureSource::Register(9)];
        assert_eq!(
            rule(m.clone()),
            "a run of registers, a call or a closure does not fit its operands"
        );
        m.functions[inner].captures = vec![CaptureSource::Upvalue(0)];
        assert_eq!(
            rule(m.clone()),
            "a run of registers, a call or a closure does not fit its operands"
        );
        m.functions[inner].captures = vec![CaptureSource::Register(2)];
        assert!(verify(m).is_ok());
    }

    /// Suspected item from the outside review: the type table allows a
    /// cycle that does not pass through an `Enum` at all (a `List`
    /// referring to itself directly), which `is_transferable`'s old
    /// per-root walk tolerated (its own `seen` set stopped it revisiting
    /// the entry), but which a naive memoisation scheme could get wrong or
    /// hang on. `transferability_table` must terminate and answer `true`:
    /// nothing in a self-referential `List(List(List(...)))` is a `Filter`.
    #[test]
    fn a_list_cycle_with_no_enum_is_transferable_and_terminates() {
        use hollowscript_core::transfer::TypeTable;
        let mut types = TypeTable::new();
        let self_ref = super::TypeRef(u32::try_from(types.len()).unwrap());
        let made = types.add(TransferType::List(self_ref));
        assert_eq!(made, self_ref, "the type refers to the slot it was put in");
        let table = super::transferability_table(&types);
        assert!(table[self_ref.0 as usize]);
    }

    #[test]
    fn exports_must_match_and_be_transferable() {
        let mut m = base(vec![RET]);
        let f = function(&mut m, FunctionKind::Function, vec![RET]);
        let name = m.add_string("update").unwrap();
        let float = m.types.add(TransferType::Float);
        let filter = m.types.add(TransferType::Filter);
        let export = |params| Export {
            name,
            kind: ExportKind::Function {
                function: u32::try_from(f).unwrap(),
                signature: Signature { params, ret: None },
            },
        };
        m.exports.push(export(vec![float]));
        assert_eq!(
            rule(m.clone()),
            "an exported function does not match its signature or is not transferable"
        );
        m.functions[f].arity = 1;
        assert!(verify(m.clone()).is_ok());
        m.exports[0] = export(vec![filter]);
        assert_eq!(
            rule(m.clone()),
            "an exported function does not match its signature or is not transferable"
        );
        m.exports[0] = Export {
            name,
            kind: ExportKind::Constant {
                global: 0,
                ty: float,
            },
        };
        m.globals.push(Global {
            module: 0,
            name,
            mutable: true,
        });
        assert_eq!(
            rule(m.clone()),
            "an exported constant is not a constant of the entry module or is not transferable"
        );
        m.globals[0].mutable = false;
        assert!(verify(m).is_ok());
    }

    /// A constant that names a string the table does not hold must be
    /// refused: the interpreter once resized a cache to one more than the id,
    /// so 316 bytes of container could ask for a table of 2^32 entries.
    #[test]
    fn a_string_or_filter_constant_must_name_a_string_that_exists() {
        const RULE: &str = "a string or filter constant names a string that does not exist";
        for huge in [u32::MAX, u32::MAX - 1, 0x7fff_ffff, 1 << 24] {
            let mut m = base(vec![RET]);
            m.functions[0].constants = vec![Constant::Str(StringId(huge))];
            assert_eq!(rule(m.clone()), RULE);
            // And through the bytes a host would hand over, with the id in
            // the container itself.
            let err = verify_bytes(&encode(&m)).unwrap_err();
            assert!(matches!(
                err,
                LoadError::Verify(VerifyError { rule: RULE, .. })
            ));
            let mut m = base(vec![RET]);
            m.functions[0].constants = vec![Constant::Filter(StringId(huge))];
            assert_eq!(rule(m), RULE);
        }
        // The first id past the table is refused and the last one inside it
        // is not.
        let mut m = base(vec![RET]);
        let past = u32::try_from(m.strings.len()).unwrap();
        m.functions[0].constants = vec![Constant::Str(StringId(past))];
        assert_eq!(rule(m.clone()), RULE);
        m.functions[0].constants = vec![Constant::Str(StringId(past - 1))];
        assert!(verify(m).is_ok());
    }

    #[test]
    fn a_function_name_must_name_a_string_that_exists() {
        let mut m = base(vec![RET]);
        m.functions[0].name = StringId(u32::MAX);
        assert_eq!(rule(m), "a function names a string that does not exist");
    }

    #[test]
    fn a_float_constant_must_be_finite_and_not_negative_zero() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.0] {
            let mut m = base(vec![RET]);
            m.functions[0].constants = vec![Constant::Float(bad)];
            assert_eq!(
                rule(m),
                "a Float constant must be finite and not negative zero"
            );
        }
        let mut m = base(vec![RET]);
        m.functions[0].constants = vec![Constant::Float(0.0), Constant::Float(-1.5)];
        assert!(verify(m).is_ok());
    }

    /// What a capability, a capability's function and an export are called is
    /// an identifier of the lexical grammar, because each is written as one in
    /// a program; a name outside it cannot be reached from source and breaks
    /// any host that writes names into JSON or joins them with a dot.
    #[test]
    fn capability_and_export_names_must_be_identifiers() {
        use crate::module::{Capability, CapabilityKind};
        let capability = |m: &mut Module, name: &str| {
            let name = m.add_string(name).unwrap();
            m.capabilities.push(Capability {
                name,
                kind: CapabilityKind::Function(Signature {
                    params: vec![],
                    ret: None,
                }),
            });
        };
        for bad in [
            "print\",\"clock.now",
            "a\\b",
            "line\nfeed",
            "tab\t",
            "a b",
            "a.b",
            "",
            "1abc",
            "-x",
            "caf\u{e9}",
            "\u{0}",
        ] {
            let mut m = base(vec![RET]);
            capability(&mut m, bad);
            assert_eq!(
                rule(m),
                "capability names must be distinct identifiers",
                "{bad:?}"
            );
        }
        for good in ["print", "_", "_x9", "Clock", "a1_B2"] {
            let mut m = base(vec![RET]);
            capability(&mut m, good);
            assert!(verify(m).is_ok(), "{good:?}");
        }
        // Two capabilities of one name.
        let mut m = base(vec![RET]);
        capability(&mut m, "print");
        capability(&mut m, "print");
        assert_eq!(rule(m), "capability names must be distinct identifiers");
        // A namespace's functions are identifiers and distinct.
        for (names, ok) in [
            (vec!["now", "today"], true),
            (vec!["now", "now"], false),
            (vec!["now", "to day"], false),
            (vec!["now", "a.b"], false),
            (vec!["now", ""], false),
            (vec!["now", "x\"y"], false),
        ] {
            let mut m = base(vec![RET]);
            let name = m.add_string("clock").unwrap();
            let functions = names
                .iter()
                .map(|n| {
                    (
                        m.add_string(*n).unwrap(),
                        Signature {
                            params: vec![],
                            ret: None,
                        },
                    )
                })
                .collect();
            m.capabilities.push(Capability {
                name,
                kind: CapabilityKind::Namespace(functions),
            });
            assert_eq!(verify(m).is_ok(), ok, "{names:?}");
        }
        // An export.
        let mut m = base(vec![RET]);
        let int = m.types.add(TransferType::Int);
        let name = m.add_string("bad\"name").unwrap();
        m.globals.push(Global {
            module: 0,
            name,
            mutable: false,
        });
        m.exports.push(Export {
            name,
            kind: ExportKind::Constant { global: 0, ty: int },
        });
        assert_eq!(rule(m), "export names must be distinct identifiers");
    }

    #[test]
    fn bytes_are_decoded_before_they_are_verified() {
        assert_eq!(
            verify_bytes(b"HSBC").unwrap_err(),
            LoadError::Container(ContainerError::Truncated)
        );
        let mut m = base(vec![RET]);
        m.language = (2, 0);
        let err = verify_bytes(&encode(&m)).unwrap_err();
        assert!(matches!(
            err,
            LoadError::Verify(VerifyError { function: None, .. })
        ));
        assert!(err.to_string().contains("another major version"));
    }
}

//! Lowers a checked [`Program`] to a [`Module`].
//!
//! # Gaps found while writing this compiler
//!
//! The interfaces of `hollowscript-check` and `hollowscript-bytecode` are
//! fixed by their own design documents and are not redesigned here, but real
//! gaps blocked a literal reading of them and each is worked around narrowly,
//! in one place, with the workaround documented at its use site.
//! (Two more were found the same way and have since been closed in
//! `hollowscript-check` itself. The first: a narrowed path with field
//! accesses could not be compiled, because `hir::Path` named its fields by
//! `Symbol` alone and no field position is recoverable from a `TypeId` out
//! here; `hir::PathField` now carries the same position `ExprKind::Field`
//! does, which the checker already had in hand while building the path, so
//! `ExprKind::NarrowedPath` compiles for a path of any length. The second, a
//! record's field order: [`TypeInterner::record`]
//! (`hollowscript_check::types`) now sorts a record's fields by their
//! *resolved string*, the same order this compiler always built `Shape` and
//! `NewRecord` in, so `ExprKind::Field::index`, which that order defines,
//! agrees with this compiler's own field positions by construction, for a
//! record nested at any depth. It needed an interner reference
//! `TypeInterner` did not previously keep, which `record` now takes as a
//! parameter.)
//!
//! 1. **No interner reaches `compile`.** See the crate's top-level
//!    documentation; `compile` now takes `symbols: &Interner`.
//!
//! Both are closed. `VariantDesc` carries the enum's and the variant's real
//! declared names, and `Module::exports` is built by [`crate::transfer`],
//! which converts a checker `TypeId` into the transferable type an export's
//! signature needs.
//!
//! # Deliberate simplifications, not gaps
//!
//! - Every compiled function is `FunctionKind::Closure` (an empty capture
//!   list for a plain top-level `fn`). Every call compiles to the generic,
//!   register-based `Op::Call`; `Op::CallFn` is never emitted.
//! - A capability's parameter count is taken from the arity actually used at
//!   each call site, not from a declared signature: neither the HIR nor
//!   `hollowscript_check::CapabilityInterface` carries a capability's
//!   parameter or return types (only a flat `TypeTable` per capability name,
//!   which cannot express a function signature at all, only value types), so
//!   there is nothing to read one from. The verifier does not check a
//!   capability signature's types, only that a call's argument count matches
//!   its own declared arity; this compiler enforces the one further
//!   condition that keeps that sound (one fixed arity per capability, see
//!   [`ModuleCtx::cap_call_of`]) itself, rather than leave it to a
//!   not-yet-written HIR-level check in the checker.
//! - Step charges are one `Op::Charge` per statement executed and per loop
//!   iteration, which is section 11.3's schedule for statements and loops;
//!   every other charge (calls, comparisons, library functions) is made by
//!   the instruction that does the work, in the interpreter.
//!
//! # Widths
//!
//! Section 1.10 bounds how many elements, arguments, fields or statements
//! one construct has only by the size of a source file, so nothing here
//! stops at what one operand byte reaches:
//!
//! - Temporaries are allocated like a stack and freed as soon as the
//!   instruction that reads them is emitted (see [`FnBuilder`]), so a chain
//!   or a long function needs no more registers than its deepest expression.
//! - A list or map literal is built a chunk at a time ([`LIST_CHUNK`],
//!   [`MAP_CHUNK`]): `NewList`/`NewMap` of the first chunk, then
//!   `ListAppend`/`MapPut`s that extend the same value in place, so a
//!   literal of any length needs one chunk of registers and is charged as
//!   one literal.
//! - A register, a count or an index past what an operand byte holds is
//!   widened with an `Op::Ext` prefix, so a frame holds up to
//!   `MAX_REGISTERS` (65,535): a function of hundreds of locals, a call of
//!   hundreds of arguments or a record of hundreds of fields each compiles
//!   directly.
//! - A jump is laid out once the whole function is known ([`layout`]), in
//!   its short form where it reaches and as `Op::JumpLong` where it does not.
//!
//! What is left is a single function that needs more than 65,535 registers
//! at once (an interpolation of more than 32,766 values, a call of that
//! many arguments, or as many locals), or code too long for a `JumpLong` to
//! cross. These are this implementation's own limits, which section 1.10
//! allows: each is reported as HS0901 located at the construct that went
//! over (`CompileError::limit`), never as an internal error.

use hollowscript_bytecode::module::{
    CapCall, Capability, CapabilityKind, CaptureSource, Constant, Export, ExportKind, Function,
    FunctionKind, Global, MAX_REGISTERS, Module, ModuleEntry, Shape, Signature, SpanEntry,
    StringId, VariantDesc,
};
use hollowscript_bytecode::opcode::{Instruction, Op};
use hollowscript_check::hir::NodeId;
use hollowscript_check::hir::{
    self, Block, Capture, Constant as HirConstant, Expr, ExprKind, LocalId, Match, PathRoot,
    Pattern, PatternKind, Program, Stmt, StmtKind,
};
use hollowscript_check::spans::NodeSpan;
use hollowscript_check::types::{DeclId, DeclShape, TypeView};
use hollowscript_core::native::ROSTER;
use hollowscript_core::{Interner, Symbol};
use std::collections::BTreeMap;
use std::collections::BTreeSet;

use crate::{CompileError, Options};

/// A register of a frame. An instruction names the first 256 with its own
/// operand bytes and the rest through an `Op::Ext` prefix
/// (`hollowscript_bytecode::opcode`), so a register is 16 bits here, below
/// [`MAX_REGISTERS`].
type Reg = u16;

/// The one limit left on how wide a function may be: every register it
/// needs at once has to fit in one frame.
fn too_many_registers() -> CompileError {
    CompileError::limit(format!(
        "one function holds at most {MAX_REGISTERS} values at once, and this needs more"
    ))
}

/// `value` as a register, or [`too_many_registers`].
fn reg_at(value: usize) -> Result<Reg, CompileError> {
    if value < MAX_REGISTERS {
        u16::try_from(value).map_err(|_| too_many_registers())
    } else {
        Err(too_many_registers())
    }
}

/// `value` as a 16 bit immediate operand (a position, a count, an index an
/// `Abc` instruction carries in `b` or `c`), widened by a prefix when it
/// needs to be.
fn imm16(value: usize) -> Result<u16, CompileError> {
    u16::try_from(value).map_err(|_| {
        CompileError::limit(format!(
            "one instruction counts at most {} of anything, and this needs more",
            u16::MAX
        ))
    })
}

fn u32_index(value: usize) -> Result<u32, CompileError> {
    u32::try_from(value)
        .map_err(|_| CompileError::unsupported("a table grew past u32::MAX entries"))
}

fn register_of(local: LocalId) -> Result<Reg, CompileError> {
    reg_at(local.0 as usize)
}

/// Rejects a declared local count that already exceeds [`MAX_REGISTERS`],
/// the same way every actual register allocation is checked ([`reg_at`]),
/// so an oversized function is refused with this crate's own diagnostic
/// regardless of whether any instruction happens to touch the unused high
/// registers.
fn check_local_count(local_count: usize) -> Result<(), CompileError> {
    if local_count > MAX_REGISTERS {
        return Err(too_many_registers());
    }
    Ok(())
}

/// `slice[index]`, but reported rather than a possible panic: every call site
/// re-reads a position it computed itself a moment earlier (a sort of the
/// slice's own indices), so this can never actually miss for a well formed
/// program, but nothing here relies on that by indexing directly.
fn at<T>(slice: &[T], index: usize) -> Result<&T, CompileError> {
    slice
        .get(index)
        .ok_or_else(|| CompileError::unsupported("an internal index fell out of range"))
}

/// The original positions of `names`, reordered so the names they name are
/// alphabetical: `order[rank]` is the position, among the input, of the
/// field that belongs at alphabetical rank `rank`. This is the order this
/// compiler always builds a `Shape`/`NewRecord`/record-pattern `GetField` in,
/// which is also the order `hollowscript_check::types::TypeInterner::record`
/// sorts by, so a real checker's own `ExprKind::Field::index` names the same
/// position in that `Shape`.
fn sorted_order(names: Vec<String>) -> Vec<usize> {
    let mut indexed: Vec<(usize, String)> = names.into_iter().enumerate().collect();
    indexed.sort_by(|a, b| a.1.cmp(&b.1));
    indexed.into_iter().map(|(original, _)| original).collect()
}

/// Tables shared by every function and module-init body of one compilation.
struct ModuleCtx<'a> {
    out: Module,
    program: &'a Program,
    symbols: &'a Interner,
    string_cache: BTreeMap<u32, StringId>,
    shape_cache: BTreeMap<Vec<String>, u32>,
    variant_cache: BTreeMap<(u32, u32, u32), u32>,
    native_cache: BTreeMap<u32, u32>,
    capability_cache: BTreeMap<u32, u32>,
    cap_call_cache: BTreeMap<(u32, usize), u32>,
    /// The call index of one member of a namespace capability, by
    /// `(capability, member position)`.
    cap_member_cache: BTreeMap<(u32, u32), u32>,
    global_slot: BTreeMap<(u32, u32), u32>,
}

impl ModuleCtx<'_> {
    fn string(&mut self, symbol: Symbol) -> Result<StringId, CompileError> {
        if let Some(&id) = self.string_cache.get(&symbol.index()) {
            return Ok(id);
        }
        let text = self.symbols.resolve(symbol);
        let id = self
            .out
            .add_string(text)
            .map_err(|e| CompileError::unsupported(e.to_string()))?;
        self.string_cache.insert(symbol.index(), id);
        Ok(id)
    }

    fn literal_string(&mut self, text: &str) -> Result<StringId, CompileError> {
        self.out
            .add_string(text)
            .map_err(|e| CompileError::unsupported(e.to_string()))
    }

    /// The module-level shape index for a record whose fields (already
    /// resolved to text) are given in source order; sorted by that resolved
    /// text, the same order `TypeInterner::record` computes a real checker's
    /// `ExprKind::Field::index` from, rather than by re-deriving a checker's
    /// order some other way.
    fn shape_of(&mut self, field_names: &[String]) -> Result<(u32, Vec<usize>), CompileError> {
        let order = sorted_order(field_names.to_vec());
        let mut sorted = Vec::with_capacity(order.len());
        for &original in &order {
            sorted.push(at(field_names, original)?.clone());
        }
        if let Some(&idx) = self.shape_cache.get(&sorted) {
            return Ok((idx, order));
        }
        let mut ids = Vec::with_capacity(sorted.len());
        for name in &sorted {
            ids.push(self.literal_string(name)?);
        }
        let idx = u32_index(self.out.shapes.len())?;
        self.out.shapes.push(Shape { fields: ids });
        self.shape_cache.insert(sorted, idx);
        Ok((idx, order))
    }

    /// The module-level variant table index for one variant of an enum.
    ///
    /// The names come from the program's own declarations, through
    /// `TypeInterner::decl_name` and `DeclShape::Enum`. A declaration this
    /// compiler cannot find a name for is a defect in the checker's own
    /// tables rather than a property of the program, so it falls back to a
    /// name built from the `DeclId` and the variant position and keeps
    /// compiling; the verifier checks only that the string ids are in range.
    /// How many payload values the variant at `index` of `decl` declares,
    /// or `None` when this build cannot see the declaration.
    fn declared_variant_arity(&self, decl: DeclId, index: u32) -> Option<u16> {
        match self.program.types.decl_shape(decl) {
            Some(DeclShape::Enum { variants, .. }) => variants
                .get(index as usize)
                .and_then(|v| u16::try_from(v.payload.len()).ok()),
            _ => None,
        }
    }

    fn variant_of(&mut self, decl: DeclId, index: u32, arity: u16) -> Result<u32, CompileError> {
        // The declaration's own payload count, not the call site's argument
        // count: a variant used as a value (`constant make = Shape.Circle`)
        // has no call site to take one from, and a table entry built from
        // one would say the constructor takes nothing.
        let arity = self.declared_variant_arity(decl, index).unwrap_or(arity);
        let key = (decl.module, decl.index, index);
        if let Some(&idx) = self.variant_cache.get(&key) {
            if let Some(existing) = self.out.variants.get_mut(idx as usize) {
                if existing.arity < arity {
                    existing.arity = arity;
                }
            }
            return Ok(idx);
        }
        let declared_enum = self
            .program
            .types
            .decl_name(decl)
            .map(|s| self.symbols.resolve(s).to_owned());
        let declared_variant = match self.program.types.decl_shape(decl) {
            Some(DeclShape::Enum { variants, .. }) => variants
                .get(index as usize)
                .map(|v| self.symbols.resolve(v.name).to_owned()),
            _ => None,
        };
        let enum_name = self.literal_string(
            &declared_enum.unwrap_or_else(|| format!("enum#{}.{}", decl.module, decl.index)),
        )?;
        let name =
            self.literal_string(&declared_variant.unwrap_or_else(|| format!("variant#{index}")))?;
        let idx = u32_index(self.out.variants.len())?;
        self.out.variants.push(VariantDesc {
            enum_name,
            name,
            index: u16::try_from(index)
                .map_err(|_| CompileError::limit("an enum has at most 65535 variants"))?,
            arity,
            // `decl` identifies the declaration itself (section 10.4):
            // two enums spelled the same, declared in different
            // modules, or one declared by a program and one declared by a
            // host, are different types and must never share a
            // `module.variants` entry.
            decl_module: decl.module,
            decl_index: decl.index,
        });
        self.variant_cache.insert(key, idx);
        Ok(idx)
    }

    /// The module-level native import index for one entry of the standard
    /// library roster.
    fn native_of(&mut self, roster_index: u32) -> Result<u32, CompileError> {
        if let Some(&idx) = self.native_cache.get(&roster_index) {
            return Ok(idx);
        }
        let info = ROSTER.get(roster_index as usize).ok_or_else(|| {
            CompileError::unsupported("a native call names an unknown roster entry")
        })?;
        let name = self.literal_string(info.name)?;
        let idx = u32_index(self.out.natives.len())?;
        self.out
            .natives
            .push(hollowscript_bytecode::module::NativeImport {
                name,
                arity: info.arity,
            });
        self.native_cache.insert(roster_index, idx);
        Ok(idx)
    }

    /// Writes one capability's declared interface (section 9.6) into the
    /// module, once, from the checker type the host's grant produced. The
    /// module carries the *declaration*, not a shape guessed from call
    /// sites: that is what lets the VM hold a host to what it promised
    /// (HS1013), and what stops a call site's argument count from standing
    /// in for a signature it never saw.
    fn declare_capability(
        &mut self,
        transfer: &mut crate::transfer::Transfer<'_>,
        id: hir::CapabilityId,
        name: Symbol,
    ) -> Result<u32, CompileError> {
        if let Some(&idx) = self.capability_cache.get(&id.0) {
            return Ok(idx);
        }
        let name_id = self.string(name)?;
        let Some(&declared) = self.program.capability_types.get(id.0 as usize) else {
            return Err(CompileError::unsupported(
                "a capability has no declared type",
            ));
        };
        let kind = match self.program.types.view(declared) {
            TypeView::Function { params, ret, .. } => {
                let params = params.to_vec();
                CapabilityKind::Function(self.declared_signature(transfer, &params, ret)?)
            }
            TypeView::Record(fields) => {
                let fields = fields.to_vec();
                let mut members = Vec::with_capacity(fields.len());
                for (field, ty) in fields {
                    let TypeView::Function { params, ret, .. } = self.program.types.view(ty) else {
                        return Err(CompileError::unsupported(
                            "a capability namespace holds something that is not a function",
                        ));
                    };
                    let params = params.to_vec();
                    let signature = self.declared_signature(transfer, &params, ret)?;
                    members.push((self.string(field)?, signature));
                }
                CapabilityKind::Namespace(members)
            }
            _ => {
                return Err(CompileError::unsupported(
                    "a capability is neither a function nor a namespace",
                ));
            }
        };
        let idx = u32_index(self.out.capabilities.len())?;
        self.out.capabilities.push(Capability {
            name: name_id,
            kind,
        });
        self.capability_cache.insert(id.0, idx);
        Ok(idx)
    }

    /// One capability function's declared types, as entries of the module's
    /// own type table. Every one of them came from a transferable type in
    /// the host's grant, so a type that will not convert back is a defect
    /// rather than a program the compiler should accept quietly.
    fn declared_signature(
        &mut self,
        transfer: &mut crate::transfer::Transfer<'_>,
        params: &[hollowscript_check::types::TypeId],
        ret: Option<hollowscript_check::types::TypeId>,
    ) -> Result<Signature, CompileError> {
        let mut converted = Vec::with_capacity(params.len());
        for &param in params {
            let Some(r) = transfer.convert(&mut self.out, param) else {
                return Err(CompileError::unsupported(
                    "a capability parameter is not a transferable type",
                ));
            };
            converted.push(r);
        }
        let ret = match ret {
            None => None,
            Some(ty) => match transfer.convert(&mut self.out, ty) {
                Some(r) => Some(r),
                None => {
                    return Err(CompileError::unsupported(
                        "a capability result is not a transferable type",
                    ));
                }
            },
        };
        Ok(Signature {
            params: converted,
            ret,
        })
    }

    /// The declared parameter count of a capability used as a value, or of
    /// one member of a namespace capability. A value is not a call site, so
    /// there is no argument count to take it from: the declaration is the
    /// only source, which is why the module carries it.
    fn cap_declared_arity(
        &self,
        cap_index: u32,
        member: Option<StringId>,
    ) -> Result<usize, CompileError> {
        let capability = self
            .out
            .capabilities
            .get(cap_index as usize)
            .ok_or_else(|| CompileError::unsupported("a capability index is out of range"))?;
        match (&capability.kind, member) {
            (CapabilityKind::Function(signature), None) => Ok(signature.params.len()),
            (CapabilityKind::Namespace(functions), Some(name)) => functions
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, signature)| signature.params.len())
                .ok_or_else(|| {
                    CompileError::unsupported("a capability namespace has no such function")
                }),
            _ => Err(CompileError::unsupported(
                "a capability is used as the wrong shape",
            )),
        }
    }

    /// The index of a capability the program declared with `uses`, which
    /// [`declare_capabilities`] has already written into the module.
    fn capability_of(&mut self, id: hir::CapabilityId) -> Result<u32, CompileError> {
        self.capability_cache
            .get(&id.0)
            .copied()
            .ok_or_else(|| CompileError::unsupported("a capability was used without `uses`"))
    }

    /// The module-level capability-call index for calling `id` with `arity`
    /// arguments. The arity is the declaration's, not the call site's: a
    /// call site that disagrees is refused here rather than silently
    /// widening the shared signature, which would let a smaller call
    /// populate only its own registers while the verifier's register-run
    /// check still accepted the wider frame the larger call site needed,
    /// handing the host capability extra argument slots holding whatever
    /// value earlier computation left there.
    fn cap_call_of(
        &mut self,
        id: hir::CapabilityId,
        member: Option<(Symbol, u32)>,
        arity: usize,
    ) -> Result<u32, CompileError> {
        let cap_index = self.capability_of(id)?;
        if let Some((member_name, _)) = member {
            return self.namespace_call_of(cap_index, member_name, arity);
        }
        let capability = self
            .out
            .capabilities
            .get(cap_index as usize)
            .ok_or_else(|| CompileError::unsupported("a capability index is out of range"))?;
        let CapabilityKind::Function(signature) = &capability.kind else {
            return Err(CompileError::unsupported(
                "a namespace capability is called as a function",
            ));
        };
        if signature.params.len() != arity {
            return Err(CompileError::unsupported(
                "a capability is called with an argument count its declaration does not have",
            ));
        }
        let key = (cap_index, arity);
        if let Some(&idx) = self.cap_call_cache.get(&key) {
            return Ok(idx);
        }
        let idx = u32_index(self.out.cap_calls.len())?;
        self.out.cap_calls.push(CapCall {
            capability: cap_index,
            function: 0,
        });
        self.cap_call_cache.insert(key, idx);
        Ok(idx)
    }

    /// The call index for one member of a namespace capability
    /// (`clock.now`), found by name in the declared interface, so that
    /// `CapCall::function` names the same position the host bound.
    fn namespace_call_of(
        &mut self,
        cap_index: u32,
        member: Symbol,
        arity: usize,
    ) -> Result<u32, CompileError> {
        let member_id = self.string(member)?;
        let capability = self
            .out
            .capabilities
            .get(cap_index as usize)
            .ok_or_else(|| CompileError::unsupported("a capability index is out of range"))?;
        let CapabilityKind::Namespace(functions) = &capability.kind else {
            return Err(CompileError::unsupported(
                "a member is read from a capability that is one function",
            ));
        };
        let Some(position) = functions.iter().position(|(n, _)| *n == member_id) else {
            return Err(CompileError::unsupported(
                "a capability namespace has no such function",
            ));
        };
        let Some((_, signature)) = functions.get(position) else {
            return Err(CompileError::unsupported("a member index is out of range"));
        };
        if signature.params.len() != arity {
            return Err(CompileError::unsupported(
                "a capability function is called with an argument count its declaration \
                 does not have",
            ));
        }
        let function = u32_index(position)?;
        if let Some(&idx) = self.cap_member_cache.get(&(cap_index, function)) {
            return Ok(idx);
        }
        let idx = u32_index(self.out.cap_calls.len())?;
        self.out.cap_calls.push(CapCall {
            capability: cap_index,
            function,
        });
        self.cap_member_cache.insert((cap_index, function), idx);
        Ok(idx)
    }
}

/// Writes the declared interface of every capability the program used with
/// `uses` into the module, before any body is compiled, so that a call site
/// only ever looks one up.
fn declare_capabilities(ctx: &mut ModuleCtx<'_>, program: &Program) -> Result<(), CompileError> {
    let mut transfer = crate::transfer::Transfer::new(&program.types, ctx.symbols);
    let used: Vec<(Symbol, hir::CapabilityId)> = program
        .modules
        .iter()
        .flat_map(|module| module.capabilities.iter().copied())
        .collect();
    for (name, id) in used {
        ctx.declare_capability(&mut transfer, id, name)?;
    }
    Ok(())
}

/// Compiles a whole program.
pub(crate) fn compile_program(
    program: &Program,
    symbols: &Interner,
    options: Options,
) -> Result<Module, CompileError> {
    let mut ctx = ModuleCtx {
        out: Module::new(),
        program,
        symbols,
        string_cache: BTreeMap::new(),
        shape_cache: BTreeMap::new(),
        variant_cache: BTreeMap::new(),
        native_cache: BTreeMap::new(),
        capability_cache: BTreeMap::new(),
        cap_call_cache: BTreeMap::new(),
        cap_member_cache: BTreeMap::new(),
        global_slot: BTreeMap::new(),
    };

    declare_capabilities(&mut ctx, program)?;

    // Global slots, assigned before any body is compiled so every function
    // can resolve a `GlobalId` it reads or writes.
    for (module_index, module) in program.modules.iter().enumerate() {
        for global in &module.globals {
            let slot = u32_index(ctx.out.globals.len())?;
            ctx.global_slot
                .insert((global.id.module, global.id.index), slot);
            let name = ctx.string(global.name)?;
            ctx.out.globals.push(Global {
                module: u32_index(module_index)?,
                name,
                mutable: global.mutable,
            });
        }
    }

    // Which module each function belongs to, so that its instructions can
    // be located in the right source. `hir::Module::function_ids` names
    // every function and closure of the module, not only its top-level
    // named ones, so this covers every `FunctionId` of the program.
    let mut owner: BTreeMap<u32, u32> = BTreeMap::new();
    for (index, module) in program.modules.iter().enumerate() {
        let index = u32_index(index)?;
        for id in &module.function_ids {
            owner.insert(id.0, index);
        }
    }

    // Every function of the program, `FunctionId(i)` at `out.functions[i]`.
    for (index, function) in program.functions.iter().enumerate() {
        check_block_depth(&function.body)?;
        let boxed = collect_boxed_locals(program, &function.body);
        let index = u32_index(index)?;
        // A function reaching here that `owner` does not name (a defect in
        // the checker, not something a valid program can cause) is located
        // nowhere, which costs only the position of a fault inside it.
        let source = owner.get(&index).and_then(|module_index| {
            program
                .modules
                .get(*module_index as usize)
                .map(|m| (*module_index, &m.node_spans))
        });
        let compiled = compile_function(&mut ctx, function, &boxed, options, source)?;
        ctx.out.functions.push(compiled);
    }

    // Each module's initialiser: its globals' own initialisers interleaved
    // with its free statements by source position (their shared `NodeId`
    // order), then the statements that are not a declaration.
    for (module_index, module) in program.modules.iter().enumerate() {
        let module_index = u32_index(module_index)?;
        let init_index = u32_index(ctx.out.functions.len())?;
        let compiled = compile_module_init(&mut ctx, module, options, module_index)?;
        ctx.out.functions.push(compiled);
        let path = ctx.literal_string(&module.path)?;
        ctx.out.modules.push(ModuleEntry {
            path,
            init: init_index,
        });
    }

    register_data_types(&mut ctx, program)?;
    build_exports(&mut ctx, program)?;

    Ok(ctx.out)
}

/// Puts the prelude types of chapter 15 into the module's tables when the
/// program calls one of that chapter's functions.
///
/// `json.parse` builds a `Json` value of whichever variant the text turns
/// out to hold, and `time.parts` builds a `TimeParts` record, but the
/// program may never write either itself: it might only `match` an `Object`
/// or read one field. The compiler records only what the program writes, so
/// a value the library builds needs its variant and its shape put there on
/// the library's behalf, and by name, so that a variant the program *does*
/// write shares the one entry rather than making a second that a `match`
/// would treat as a different variant.
fn register_data_types(ctx: &mut ModuleCtx<'_>, program: &Program) -> Result<(), CompileError> {
    let uses_json = ctx
        .out
        .natives
        .iter()
        .any(|n| ctx.out.string(n.name).starts_with("json."));
    let uses_time = ctx
        .out
        .natives
        .iter()
        .any(|n| ctx.out.string(n.name).starts_with("time."));
    if uses_json {
        // `json_decl` is the checker's own declaration, so `variant_of`
        // finds each variant's real name and its position within the enum.
        let decl = program.types.json_decl_if_declared().ok_or_else(|| {
            CompileError::unsupported("a json call reached the compiler with no Json declaration")
        })?;
        let Some(DeclShape::Enum { variants, .. }) = program.types.decl_shape(decl) else {
            return Err(CompileError::unsupported("Json is not an enum"));
        };
        let arities: Vec<(u32, u16)> = variants
            .iter()
            .enumerate()
            .map(|(index, v)| {
                (
                    u32::try_from(index).unwrap_or(0),
                    u16::try_from(v.payload.len()).unwrap_or(0),
                )
            })
            .collect();
        for (index, arity) in arities {
            ctx.variant_of(decl, index, arity)?;
        }
        ctx.shape_of(&["message".to_owned(), "offset".to_owned()])?;
    }
    // `json.parse`, `time.parse`, `time.parseDate`, `time.fromParts`,
    // `parseInt` and `parseFloat` each return a `Result`, which they build,
    // so its two variants are needed even when the program only asks
    // `isOk()`.
    let uses_parse = ctx
        .out
        .natives
        .iter()
        .any(|n| matches!(ctx.out.string(n.name), "parseInt" | "parseFloat"));
    if uses_json || uses_time || uses_parse {
        let result = program.types.result_decl_if_declared().ok_or_else(|| {
            CompileError::unsupported("a chapter 15 call reached the compiler with no Result")
        })?;
        ctx.variant_of(result, 0, 1)?;
        ctx.variant_of(result, 1, 1)?;
    }
    if uses_time {
        ctx.shape_of(&[
            "year".to_owned(),
            "month".to_owned(),
            "day".to_owned(),
            "hour".to_owned(),
            "minute".to_owned(),
            "second".to_owned(),
            "millisecond".to_owned(),
            "weekday".to_owned(),
        ])?;
    }
    Ok(())
}

/// Builds the entry module's export table (section 12.4), which is what a
/// host reads to know what it may `call` and `read`.
///
/// Only the entry module's exports are listed: section 10.4's `export` also
/// serves other *modules*, which the checker has already resolved, and
/// chapter 12 gives a host access to the entry module alone. A name whose
/// type is not transferable (a function value, a `Filter` outside a
/// capability) is left out rather than reported, because a module may
/// legitimately export it for another module to import.
fn build_exports(ctx: &mut ModuleCtx<'_>, program: &Program) -> Result<(), CompileError> {
    let Some(entry) = program.modules.last() else {
        return Ok(());
    };
    let entry_index = u32_index(program.modules.len().saturating_sub(1))?;
    let mut transfer = crate::transfer::Transfer::new(&program.types, ctx.symbols);
    for (name, export) in &entry.exports {
        let kind = match export {
            hir::Export::Type(_) => continue,
            hir::Export::Function(id) => {
                let Some(function) = program.functions.get(id.0 as usize) else {
                    continue;
                };
                let mut params = Vec::with_capacity(function.params.len());
                for param in &function.params {
                    if let Some(r) = transfer.convert(&mut ctx.out, param.ty) {
                        params.push(r);
                    } else {
                        break;
                    }
                }
                if params.len() != function.params.len() {
                    continue;
                }
                let ret = match function.ret {
                    None => None,
                    Some(ty) => match transfer.convert(&mut ctx.out, ty) {
                        Some(r) => Some(r),
                        None => continue,
                    },
                };
                let entry_point =
                    compile_export_entry(ctx, *name, id.0, params.len(), ret.is_some())?;
                ExportKind::Function {
                    function: entry_point,
                    signature: Signature { params, ret },
                }
            }
            hir::Export::Constant(global) => {
                if global.module != entry_index {
                    continue;
                }
                let Some(slot) = ctx.global_slot.get(&(global.module, global.index)).copied()
                else {
                    continue;
                };
                let Some(declared) = entry
                    .globals
                    .iter()
                    .find(|g| g.id.index == global.index)
                    .map(|g| g.ty)
                else {
                    continue;
                };
                let Some(ty) = transfer.convert(&mut ctx.out, declared) else {
                    continue;
                };
                ExportKind::Constant { global: slot, ty }
            }
        };
        let name = ctx.string(*name)?;
        ctx.out.exports.push(Export { name, kind });
    }
    Ok(())
}

/// An exported function's entry point.
///
/// The verifier reserves [`FunctionKind::Function`] for a function a host
/// enters, and this compiler makes every function of the program a
/// [`FunctionKind::Closure`], because that is what `Op::Closure` needs to
/// build a function value and every call within a program goes through one.
/// A function cannot be both, so an export gets a small entry point of its
/// own that builds the real function's closure and calls it with the
/// arguments the host passed. The extra frame is one call per host
/// call, and it keeps the two kinds meaning exactly what the verifier says
/// they mean.
fn compile_export_entry(
    ctx: &mut ModuleCtx<'_>,
    name: Symbol,
    target: u32,
    arity: usize,
    returns_value: bool,
) -> Result<u32, CompileError> {
    let arity16 = imm16(arity)?;
    // The host's arguments arrive in registers 0 to `arity - 1`, and a call
    // wants the callee first and its arguments after it: moving each
    // argument up one, from the last down, frees register 0 for the callee
    // without a second copy of every argument, so the entry needs one
    // register more than the function it calls, never twice as many.
    let registers = reg_at(arity).map(|top| top + 1)?;
    let mut code = Vec::with_capacity(arity + 3);
    let mut push = |(ext, main): (Option<Instruction>, Instruction)| {
        code.extend(ext.map(Instruction::encode));
        code.push(main.encode());
    };
    for i in (0..arity).rev() {
        push(Instruction::wide_abc(
            Op::Move,
            reg_at(i + 1)?,
            reg_at(i)?,
            0,
        ));
    }
    push(Instruction::wide_abx(Op::Closure, 0, target));
    push(Instruction::wide_abc(Op::Call, 0, arity16, 0));
    push(if returns_value {
        Instruction::wide_abc(Op::Return, 0, 0, 0)
    } else {
        Instruction::wide_abc(Op::ReturnNone, 0, 0, 0)
    });
    let index = u32_index(ctx.out.functions.len())?;
    let name = ctx.string(name)?;
    ctx.out.functions.push(Function {
        name,
        kind: FunctionKind::Function,
        arity: arity16,
        register_count: registers,
        returns_value,
        captures: Vec::new(),
        constants: Vec::new(),
        code,
        spans: Vec::new(),
    });
    Ok(index)
}

/// The deepest an expression, statement or pattern tree may nest before this
/// compiler refuses to compile it rather than risk the host's own call
/// stack: [`compile_expr`], [`compile_stmt`], [`compile_pattern`], the
/// closure-capture walk and [`max_local_id`] all recurse directly on nested
/// HIR nodes with no depth limit of their own, so a tree deeper than this is
/// rejected here, *before* any of them ever runs, by [`check_block_depth`]
/// and [`check_expr_depth`]: an explicit work stack that cannot itself
/// overflow, the same technique `hollowscript_vm::heap::Heap::tree_size` and
/// `hollowscript_bytecode::verify::check_cycles` already use for the same
/// reason. A hand-built HIR expression of 200,000 nested `Binary` nodes is
/// exactly the input this closes: without this check it aborts the process
/// with a stack overflow instead of returning a [`CompileError`], which is
/// the one class of "no panics, never crashes" gap (spec section 19.1) this
/// crate had. Chosen conservatively low enough that even a debug build's
/// larger stack frames cannot overflow a small host thread stack before this
/// count is reached, and high enough that no real program's own nesting
/// (bounded, at the source level, by `MAX_CHAIN_LINKS` and
/// `MAX_NESTING_DEPTH` of section 1.10, both far smaller) is ever refused by it.
const MAX_COMPILE_NESTING_DEPTH: u32 = 2_000;

fn too_deeply_nested() -> CompileError {
    CompileError::unsupported(
        "an expression, statement or pattern is nested too deeply for this compiler",
    )
}

/// One pending node of the iterative depth walk, tagged with the depth it
/// was reached at.
enum DepthItem<'a> {
    Block(&'a Block, u32),
    Stmt(&'a Stmt, u32),
    Expr(&'a Expr, u32),
    Pattern(&'a Pattern, u32),
}

/// Rejects a function body or module-init block whose expression, statement
/// or pattern tree nests deeper than [`MAX_COMPILE_NESTING_DEPTH`]. See
/// [`MAX_COMPILE_NESTING_DEPTH`] for why this exists and why it is safe to
/// call this on completely untrusted, adversarially deep input.
fn check_block_depth(block: &Block) -> Result<(), CompileError> {
    walk_depth(vec![DepthItem::Block(block, 0)])
}

/// As [`check_block_depth`], for a single expression (a global's initialiser).
fn check_expr_depth(expr: &Expr) -> Result<(), CompileError> {
    walk_depth(vec![DepthItem::Expr(expr, 0)])
}

fn walk_depth(mut stack: Vec<DepthItem<'_>>) -> Result<(), CompileError> {
    while let Some(item) = stack.pop() {
        let depth = match &item {
            DepthItem::Block(_, d)
            | DepthItem::Stmt(_, d)
            | DepthItem::Expr(_, d)
            | DepthItem::Pattern(_, d) => *d,
        };
        if depth > MAX_COMPILE_NESTING_DEPTH {
            return Err(too_deeply_nested());
        }
        let next = depth + 1;
        match item {
            DepthItem::Block(block, _) => {
                stack.extend(block.stmts.iter().map(|s| DepthItem::Stmt(s, next)));
            }
            DepthItem::Stmt(stmt, _) => push_stmt_children(stmt, next, &mut stack),
            DepthItem::Expr(expr, _) => push_expr_children(expr, next, &mut stack),
            DepthItem::Pattern(pattern, _) => push_pattern_children(pattern, next, &mut stack),
        }
    }
    Ok(())
}

fn push_stmt_children<'a>(stmt: &'a Stmt, depth: u32, stack: &mut Vec<DepthItem<'a>>) {
    match &stmt.kind {
        StmtKind::Let { pattern, init, .. } => {
            stack.push(DepthItem::Expr(init, depth));
            if let Some(pattern) = pattern {
                stack.push(DepthItem::Pattern(pattern, depth));
            }
        }
        StmtKind::Assign { value, .. }
        | StmtKind::AssignUpvalue { value, .. }
        | StmtKind::AssignGlobal { value, .. } => {
            stack.push(DepthItem::Expr(value, depth));
        }
        StmtKind::If {
            condition,
            then,
            otherwise,
            ..
        } => {
            stack.push(DepthItem::Expr(condition, depth));
            stack.push(DepthItem::Block(then, depth));
            if let Some(otherwise) = otherwise {
                stack.push(DepthItem::Block(otherwise, depth));
            }
        }
        StmtKind::Match(m) => {
            stack.push(DepthItem::Expr(&m.scrutinee, depth));
            for arm in &m.arms {
                for pattern in &arm.patterns {
                    stack.push(DepthItem::Pattern(pattern, depth));
                }
                stack.push(DepthItem::Block(&arm.body, depth));
            }
        }
        StmtKind::For {
            pattern,
            iterable,
            body,
            ..
        } => {
            stack.push(DepthItem::Expr(iterable, depth));
            stack.push(DepthItem::Block(body, depth));
            if let Some(pattern) = pattern {
                stack.push(DepthItem::Pattern(pattern, depth));
            }
        }
        StmtKind::While {
            condition, body, ..
        } => {
            stack.push(DepthItem::Expr(condition, depth));
            stack.push(DepthItem::Block(body, depth));
        }
        StmtKind::Return(Some(expr)) | StmtKind::Call(expr) => {
            stack.push(DepthItem::Expr(expr, depth));
        }
        StmtKind::Return(None) | StmtKind::Break { .. } | StmtKind::Continue { .. } => {}
    }
}

fn push_expr_children<'a>(expr: &'a Expr, depth: u32, stack: &mut Vec<DepthItem<'a>>) {
    match &expr.kind {
        ExprKind::List(items)
        | ExprKind::NativeCall { args: items, .. }
        | ExprKind::VariantConstruct { args: items, .. } => {
            stack.extend(items.iter().map(|e| DepthItem::Expr(e, depth)));
        }
        ExprKind::Map(pairs) => {
            for (k, v) in pairs {
                stack.push(DepthItem::Expr(k, depth));
                stack.push(DepthItem::Expr(v, depth));
            }
        }
        ExprKind::Record(fields) => {
            stack.extend(fields.iter().map(|(_, v)| DepthItem::Expr(v, depth)));
        }
        ExprKind::Unary { operand, .. }
        | ExprKind::Not(operand)
        | ExprKind::IntToFloat(operand)
        | ExprKind::Try { operand, .. } => stack.push(DepthItem::Expr(operand, depth)),
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Logical { lhs, rhs, .. } => {
            stack.push(DepthItem::Expr(lhs, depth));
            stack.push(DepthItem::Expr(rhs, depth));
        }
        ExprKind::Call { callee, args } | ExprKind::EffectCall { callee, args } => {
            stack.push(DepthItem::Expr(callee, depth));
            stack.extend(args.iter().map(|e| DepthItem::Expr(e, depth)));
        }
        ExprKind::Index { target, index } => {
            stack.push(DepthItem::Expr(target, depth));
            stack.push(DepthItem::Expr(index, depth));
        }
        ExprKind::Field { target, .. } => stack.push(DepthItem::Expr(target, depth)),
        ExprKind::Interpolate { exprs, .. } => {
            stack.extend(exprs.iter().map(|e| DepthItem::Expr(e, depth)));
        }
        // A closure's own body is compiled independently, as its own
        // top-level function (see `walk_expr_for_closures`), so it is
        // checked by its own `check_block_depth` call in `compile_program`,
        // not descended into from here.
        ExprKind::Closure(_)
        | ExprKind::CapabilityMember { .. }
        | ExprKind::Literal(_)
        | ExprKind::Filter(_)
        | ExprKind::Local(_)
        | ExprKind::Upvalue(_)
        | ExprKind::Global(_)
        | ExprKind::NarrowedPath(_)
        | ExprKind::Capability(_)
        | ExprKind::FunctionValue(_)
        | ExprKind::VariantValue { .. }
        | ExprKind::NativeValue(_)
        | ExprKind::Error => {}
    }
}

fn push_pattern_children<'a>(pattern: &'a Pattern, depth: u32, stack: &mut Vec<DepthItem<'a>>) {
    match &pattern.kind {
        PatternKind::Variant { args, .. } => {
            stack.extend(args.iter().map(|p| DepthItem::Pattern(p, depth)));
        }
        PatternKind::Record(fields) => {
            stack.extend(fields.iter().map(|f| DepthItem::Pattern(&f.pattern, depth)));
        }
        PatternKind::List { elements, .. } => {
            stack.extend(elements.iter().map(|p| DepthItem::Pattern(p, depth)));
        }
        PatternKind::Binding(_) | PatternKind::Literal(_) | PatternKind::None => {}
    }
}

/// Which locals of one function's own scope a nested closure literal
/// captures by value (`hir::Capture::Local`), found by walking the body
/// without descending into a nested closure's own body: that closure's
/// captures already say, by [`LocalId`], which of *this* function's locals
/// it reaches, and those locals must live in a cell from their declaration
/// so the closure can still see later assignments.
fn collect_boxed_locals(program: &Program, body: &Block) -> BTreeSet<u32> {
    let mut boxed = BTreeSet::new();
    walk_block_for_closures(program, body, &mut boxed);
    boxed
}

fn walk_block_for_closures(program: &Program, block: &Block, boxed: &mut BTreeSet<u32>) {
    for stmt in &block.stmts {
        walk_stmt_for_closures(program, stmt, boxed);
    }
}

fn walk_stmt_for_closures(program: &Program, stmt: &Stmt, boxed: &mut BTreeSet<u32>) {
    match &stmt.kind {
        StmtKind::Let { init, .. }
        | StmtKind::Assign { value: init, .. }
        | StmtKind::AssignUpvalue { value: init, .. }
        | StmtKind::AssignGlobal { value: init, .. } => {
            walk_expr_for_closures(program, init, boxed);
        }
        StmtKind::If {
            condition,
            then,
            otherwise,
            ..
        } => {
            walk_expr_for_closures(program, condition, boxed);
            walk_block_for_closures(program, then, boxed);
            if let Some(otherwise) = otherwise {
                walk_block_for_closures(program, otherwise, boxed);
            }
        }
        StmtKind::Match(m) => {
            walk_expr_for_closures(program, &m.scrutinee, boxed);
            for arm in &m.arms {
                walk_block_for_closures(program, &arm.body, boxed);
            }
        }
        StmtKind::For { iterable, body, .. } => {
            walk_expr_for_closures(program, iterable, boxed);
            walk_block_for_closures(program, body, boxed);
        }
        StmtKind::While {
            condition, body, ..
        } => {
            walk_expr_for_closures(program, condition, boxed);
            walk_block_for_closures(program, body, boxed);
        }
        StmtKind::Return(Some(expr)) | StmtKind::Call(expr) => {
            walk_expr_for_closures(program, expr, boxed);
        }
        StmtKind::Return(None) | StmtKind::Break { .. } | StmtKind::Continue { .. } => {}
    }
}

fn walk_expr_for_closures(program: &Program, expr: &Expr, boxed: &mut BTreeSet<u32>) {
    match &expr.kind {
        ExprKind::Closure(fid) => {
            if let Some(target) = program.functions.get(fid.0 as usize) {
                for capture in &target.captures {
                    if let Capture::Local(local) = capture {
                        boxed.insert(local.0);
                    }
                }
            }
            // Deliberately not descending into the closure's own body: its
            // captures already name every local of *this* function it reaches.
        }
        ExprKind::List(items) => {
            for item in items {
                walk_expr_for_closures(program, item, boxed);
            }
        }
        ExprKind::Map(pairs) => {
            for (k, v) in pairs {
                walk_expr_for_closures(program, k, boxed);
                walk_expr_for_closures(program, v, boxed);
            }
        }
        ExprKind::Record(fields) => {
            for (_, v) in fields {
                walk_expr_for_closures(program, v, boxed);
            }
        }
        ExprKind::Unary { operand, .. }
        | ExprKind::Not(operand)
        | ExprKind::IntToFloat(operand)
        | ExprKind::Try { operand, .. } => {
            walk_expr_for_closures(program, operand, boxed);
        }
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Logical { lhs, rhs, .. } => {
            walk_expr_for_closures(program, lhs, boxed);
            walk_expr_for_closures(program, rhs, boxed);
        }
        ExprKind::Call { callee, args } | ExprKind::EffectCall { callee, args } => {
            walk_expr_for_closures(program, callee, boxed);
            for arg in args {
                walk_expr_for_closures(program, arg, boxed);
            }
        }
        ExprKind::NativeCall { args, .. } | ExprKind::VariantConstruct { args, .. } => {
            for arg in args {
                walk_expr_for_closures(program, arg, boxed);
            }
        }
        ExprKind::Index { target, index } => {
            walk_expr_for_closures(program, target, boxed);
            walk_expr_for_closures(program, index, boxed);
        }
        ExprKind::Field { target, .. } => {
            walk_expr_for_closures(program, target, boxed);
        }
        ExprKind::Interpolate { exprs, .. } => {
            for e in exprs {
                walk_expr_for_closures(program, e, boxed);
            }
        }
        ExprKind::Literal(_)
        | ExprKind::Filter(_)
        | ExprKind::Local(_)
        | ExprKind::Upvalue(_)
        | ExprKind::Global(_)
        | ExprKind::NarrowedPath(_)
        | ExprKind::Capability(_)
        | ExprKind::CapabilityMember { .. }
        | ExprKind::FunctionValue(_)
        | ExprKind::VariantValue { .. }
        | ExprKind::NativeValue(_)
        | ExprKind::Error => {}
    }
}

struct LoopCtx {
    break_patches: Vec<usize>,
    continue_pc: usize,
}

/// One instruction of a function being built, in the order it runs. A jump
/// names its target as the index of another item, not as an offset, because
/// an offset depends on how many words every item between them turns out to
/// take, which [`layout`] only knows once the whole function is built.
#[derive(Copy, Clone, Debug)]
enum Item {
    /// Encoded already: one word, or an `Op::Ext` prefix and a word.
    Fixed(Option<Instruction>, Instruction),
    /// A jump by `op` (testing register `a`, for a conditional one) to the
    /// item at `target`, which is [`UNPATCHED`] until it is patched.
    Jump { op: Op, a: Reg, target: usize },
}

/// The target of a jump that has not been patched yet.
const UNPATCHED: usize = usize::MAX;

/// How many elements of a list literal go into one instruction: the first
/// run into `NewList`, every further run into an `Op::ListAppend` that
/// extends the same list in place, so a literal of any length needs only
/// this many registers for its elements at once, and is charged exactly as
/// one list of all of them (section 11.4).
const LIST_CHUNK: usize = 64;

/// As [`LIST_CHUNK`], in key and value pairs, for a map literal's `NewMap`
/// and `Op::MapPut`. Larger than a list's, because a later chunk that sorts
/// before entries already in the map moves them and is charged steps for it
/// (see `docs/design/bytecode.md`): a map literal of up to this many pairs is
/// always one `NewMap`, and charged no steps whatever order its keys are in.
const MAP_CHUNK: usize = 128;

/// The per-function build state: registers, constants and code. The tables
/// shared by the whole module (`ModuleCtx`) are borrowed for the duration of
/// one function's compilation.
///
/// # Registers
///
/// Named locals live in fixed registers below `local_count` for the whole
/// function. Every temporary above them is allocated like a stack: an
/// expression's value lands in the first free register (`next_temp` when
/// [`compile_expr`] starts), and everything its subexpressions used above
/// that is free again once it is done. So a binary chain of any length needs
/// two temporaries, and a literal, a call or an interpolation needs its own
/// run of values plus whatever its widest element needs, not the sum over
/// every element: the frame grows with how deeply an expression nests and
/// how wide one run is, never with how long a function or an expression is.
struct FnBuilder<'m, 'a> {
    ctx: &'m mut ModuleCtx<'a>,
    boxed: BTreeSet<u32>,
    /// The register that [`FnBuilder::reset_temps`] rewinds to: the first
    /// register above the named locals, raised inside a `for` loop over its
    /// iterator's registers, which have to survive every statement of the
    /// body (see [`compile_for`]).
    temp_floor: u32,
    /// The first free register.
    next_temp: u32,
    /// The most registers in use at once so far, which is the frame's size.
    high_water: u32,
    constants: Vec<Constant>,
    const_int: BTreeMap<i64, u32>,
    const_float: BTreeMap<u64, u32>,
    const_str: BTreeMap<String, u32>,
    items: Vec<Item>,
    /// Where each of this module's nodes is, for the span table.
    node_spans: BTreeMap<NodeId, NodeSpan>,
    /// The index into `Module::modules` of the module being compiled.
    module_index: u32,
    /// Its canonical path, which a located [`CompileError`] names.
    module_path: std::sync::Arc<str>,
    /// The span entries built so far, each at the index of the item it
    /// starts at (made a word pc by [`FnBuilder::finish`]), and the last line
    /// one was written for, so that a run of instructions on one line costs
    /// one entry.
    spans: Vec<SpanEntry>,
    last_line: Option<u32>,
    loops: Vec<LoopCtx>,
    returns_value: bool,
    is_module_init: bool,
}

impl<'m, 'a> FnBuilder<'m, 'a> {
    fn new(
        ctx: &'m mut ModuleCtx<'a>,
        local_count: u16,
        boxed: BTreeSet<u32>,
        returns_value: bool,
        is_module_init: bool,
        source: Option<(u32, &BTreeMap<NodeId, NodeSpan>)>,
    ) -> FnBuilder<'m, 'a> {
        let module_index = source.map_or(0, |(index, _)| index);
        let module_path = ctx.program.modules.get(module_index as usize).map_or_else(
            || std::sync::Arc::from(""),
            |m| std::sync::Arc::from(m.path.as_str()),
        );
        FnBuilder {
            ctx,
            boxed,
            temp_floor: u32::from(local_count),
            next_temp: u32::from(local_count),
            high_water: u32::from(local_count),
            constants: Vec::new(),
            const_int: BTreeMap::new(),
            const_float: BTreeMap::new(),
            const_str: BTreeMap::new(),
            items: Vec::new(),
            node_spans: source.map(|(_, spans)| spans.clone()).unwrap_or_default(),
            module_index,
            module_path,
            spans: Vec::new(),
            last_line: None,
            loops: Vec::new(),
            returns_value,
            is_module_init,
        }
    }

    /// Where `node` is, for a located [`CompileError`].
    fn location(&self, node: NodeId) -> Option<hollowscript_core::Location> {
        let span = self.node_spans.get(&node)?;
        Some(hollowscript_core::Location {
            file: self.module_path.clone(),
            span: hollowscript_core::Span {
                start: span.start,
                end: span.end,
            },
            line: span.line,
            column: span.column,
        })
    }

    fn alloc_temp(&mut self) -> Result<Reg, CompileError> {
        self.alloc_block(1)
    }

    fn alloc_block(&mut self, count: usize) -> Result<Reg, CompileError> {
        let start = self.next_temp;
        let next = u32::try_from(count)
            .ok()
            .and_then(|count| start.checked_add(count))
            .filter(|&next| next as usize <= MAX_REGISTERS)
            .ok_or_else(too_many_registers)?;
        self.next_temp = next;
        self.high_water = self.high_water.max(next);
        reg_at(start as usize)
    }

    /// Frees every temporary allocated since the top of the current
    /// statement: named locals live below `local_count` and are never freed
    /// this way, and neither is anything below `temp_floor`, which a `for`
    /// loop raises over its iterator's registers.
    fn reset_temps(&mut self) {
        self.next_temp = self.temp_floor;
    }

    fn is_boxed(&self, local: LocalId) -> bool {
        self.boxed.contains(&local.0)
    }

    /// Records that the instructions emitted from here on come from `node`,
    /// if its position is known and differs from the last one recorded.
    /// Section 19.3 locates a fault at the operation that raised it, and
    /// this is the only place that position survives to.
    fn at(&mut self, node: NodeId) {
        let Some(span) = self.node_spans.get(&node).copied() else {
            return;
        };
        if self.last_line == Some(span.line) {
            return;
        }
        self.last_line = Some(span.line);
        let Ok(pc) = u32::try_from(self.items.len()) else {
            return;
        };
        self.spans.push(SpanEntry {
            pc,
            module: self.module_index,
            start: span.start,
            end: span.end,
            line: span.line,
            column: span.column,
        });
    }

    /// The index the next item will have: a jump target, or a loop's start.
    fn here(&self) -> usize {
        self.items.len()
    }

    fn push(&mut self, (ext, main): (Option<Instruction>, Instruction)) -> usize {
        self.items.push(Item::Fixed(ext, main));
        self.items.len() - 1
    }

    fn abc(&mut self, op: Op, a: Reg, b: u16, c: u16) -> usize {
        self.push(Instruction::wide_abc(op, a, b, c))
    }

    fn abx(&mut self, op: Op, a: Reg, bx: u32) -> usize {
        self.push(Instruction::wide_abx(op, a, bx))
    }

    fn asbx(&mut self, op: Op, a: Reg, sbx: i16) -> usize {
        self.push(Instruction::wide_asbx(op, a, sbx))
    }

    fn mov(&mut self, to: Reg, from: Reg) {
        self.abc(Op::Move, to, from, 0);
    }

    /// Emits a jump whose target [`FnBuilder::patch_jump`] gives later.
    fn jump(&mut self, op: Op, a: Reg) -> usize {
        self.items.push(Item::Jump {
            op,
            a,
            target: UNPATCHED,
        });
        self.items.len() - 1
    }

    fn patch_jump(&mut self, item: usize, target: usize) -> Result<(), CompileError> {
        match self.items.get_mut(item) {
            Some(Item::Jump { target: slot, .. }) => {
                *slot = target;
                Ok(())
            }
            _ => Err(CompileError::unsupported(
                "patched an instruction that is not a jump",
            )),
        }
    }

    fn constant_id(&self) -> Result<u32, CompileError> {
        u32::try_from(self.constants.len())
            .map_err(|_| CompileError::unsupported("a function has too many constants"))
    }

    fn const_int(&mut self, value: i64) -> Result<u32, CompileError> {
        if let Some(&id) = self.const_int.get(&value) {
            return Ok(id);
        }
        let id = self.constant_id()?;
        self.constants.push(Constant::Int(value));
        self.const_int.insert(value, id);
        Ok(id)
    }

    fn const_float(&mut self, value: f64) -> Result<u32, CompileError> {
        let key = value.to_bits();
        if let Some(&id) = self.const_float.get(&key) {
            return Ok(id);
        }
        let id = self.constant_id()?;
        self.constants.push(Constant::Float(value));
        self.const_float.insert(key, id);
        Ok(id)
    }

    fn const_str(&mut self, value: &str) -> Result<u32, CompileError> {
        if let Some(&id) = self.const_str.get(value) {
            return Ok(id);
        }
        let string_id = self.ctx.literal_string(value)?;
        let id = self.constant_id()?;
        self.constants.push(Constant::Str(string_id));
        self.const_str.insert(value.to_owned(), id);
        Ok(id)
    }

    fn const_filter(&mut self, value: &str) -> Result<u32, CompileError> {
        let string_id = self.ctx.literal_string(value)?;
        let id = self.constant_id()?;
        self.constants.push(Constant::Filter(string_id));
        Ok(id)
    }

    /// Charges `amount` steps (section 11.3's charge for a statement or a
    /// loop iteration; see the module documentation).
    fn charge(&mut self, amount: u16) {
        let amount = amount.clamp(1, 1024);
        self.abx(Op::Charge, 0, u32::from(amount));
    }

    /// Ends the function and lays it out: the frame size, the code words,
    /// the constants and the span table with its entries moved from item
    /// indexes to word pcs. `at` locates a function too long for a jump to
    /// cross, the one error laying out can raise.
    fn finish(mut self, at: Option<hollowscript_core::Location>) -> Result<Finished, CompileError> {
        // Always append a trailing instruction, even when the body's last
        // instruction already ends control flow (a `Return`, `Trap` or
        // unconditional `Jump`): a `match`, `if` or loop that is the last
        // thing in a block patches its "end" jumps to whatever pc comes
        // next, which is only a valid target once something is actually
        // there. Appending unconditionally guarantees a landing pad exists;
        // when the body already terminated, this instruction is simply
        // unreachable, which the verifier does not mind.
        if self.returns_value && !self.is_module_init {
            self.abx(Op::Trap, 0, 0);
        } else {
            self.abc(Op::ReturnNone, 0, 0, 0);
        }
        let (code, starts) = layout(&self.items).map_err(|e| e.located(|| at))?;
        let mut spans = self.spans;
        for span in &mut spans {
            let start = starts.get(span.pc as usize).copied().unwrap_or(0);
            span.pc = u32::try_from(start)
                .map_err(|_| CompileError::unsupported("a function has too many instructions"))?;
        }
        // The verifier requires every frame to hold at least one register,
        // and a function with no parameters, no locals and no temporary
        // (`fn nothing() { return }`) would otherwise ask for none.
        let register_count =
            u16::try_from(self.high_water.max(1)).map_err(|_| too_many_registers())?;
        Ok(Finished {
            register_count,
            code,
            constants: self.constants,
            spans,
        })
    }
}

/// A function's body, laid out: what [`FnBuilder::finish`] hands back.
struct Finished {
    register_count: u16,
    code: Vec<u32>,
    constants: Vec<Constant>,
    spans: Vec<SpanEntry>,
}

/// How many words `item` takes: one, or two with an `Op::Ext` prefix, for a
/// fixed instruction; for a jump, its short form, or when `long` its long
/// form (see [`layout`]).
fn item_words(item: &Item, long: bool) -> usize {
    match item {
        Item::Fixed(ext, _) => 1 + usize::from(ext.is_some()),
        Item::Jump { op, a, .. } => {
            let prefix = usize::from(*a > 0xFF);
            match (long, op) {
                (false, _) => prefix + 1,
                (true, Op::Jump) => 1,
                (true, Op::IterNext) => prefix + 3,
                (true, _) => prefix + 2,
            }
        }
    }
}

/// The word pc each item starts at, and one more entry for the end.
fn item_starts(items: &[Item], long: &[bool]) -> Vec<usize> {
    let mut starts = Vec::with_capacity(items.len() + 1);
    let mut pc = 0usize;
    starts.push(pc);
    for (item, &long) in items.iter().zip(long) {
        pc = pc.saturating_add(item_words(item, long));
        starts.push(pc);
    }
    starts
}

/// The conditional jump that tests the opposite of `op`.
fn inverted(op: Op) -> Option<Op> {
    match op {
        Op::JumpIf => Some(Op::JumpIfNot),
        Op::JumpIfNot => Some(Op::JumpIf),
        Op::JumpIfNone => Some(Op::JumpIfSome),
        Op::JumpIfSome => Some(Op::JumpIfNone),
        _ => None,
    }
}

/// Lays a function's items out as words, choosing for each jump the short
/// form when its offset fits the 16 bits of `AsBx` and the long form
/// otherwise:
///
/// - `Jump` becomes one `JumpLong`;
/// - a conditional jump becomes the opposite test jumping over the next
///   word, then a `JumpLong` (`bytecode.md`'s own recipe);
/// - `IterNext`, which jumps when the collection is exhausted, becomes an
///   `IterNext` over the next word, a `Jump` over the word after it (taken
///   when there is another element), and a `JumpLong` to the loop's end.
///
/// Making a jump long moves everything after it, which can push another
/// jump out of reach, so the choice is repeated until nothing changes. A jump
/// only ever goes from short to long, so this ends, after one pass for
/// almost every function and a handful for a pathological one.
fn layout(items: &[Item]) -> Result<(Vec<u32>, Vec<usize>), CompileError> {
    let mut long = vec![false; items.len()];
    let starts = loop {
        let starts = item_starts(items, &long);
        let mut changed = false;
        for (index, item) in items.iter().enumerate() {
            let Item::Jump { target, .. } = item else {
                continue;
            };
            if long.get(index).copied().unwrap_or(true) {
                continue;
            }
            let offset = jump_offset(&starts, index, *target)?;
            if i16::try_from(offset).is_err() {
                if let Some(slot) = long.get_mut(index) {
                    *slot = true;
                }
                changed = true;
            }
        }
        if !changed {
            break starts;
        }
    };
    let total = starts.last().copied().unwrap_or(0);
    let mut code = Vec::with_capacity(total);
    for (index, item) in items.iter().enumerate() {
        match *item {
            Item::Fixed(ext, main) => {
                code.extend(ext.map(Instruction::encode));
                code.push(main.encode());
            }
            Item::Jump { op, a, target } => {
                // Relative to the word after the item, whichever form it
                // takes: the word that jumps is the item's last either way.
                let offset = jump_offset(&starts, index, target)?;
                if !long.get(index).copied().unwrap_or(false) {
                    let short = i16::try_from(offset)
                        .map_err(|_| CompileError::unsupported("a short jump did not fit"))?;
                    let (ext, main) = Instruction::wide_asbx(op, a, short);
                    code.extend(ext.map(Instruction::encode));
                    code.push(main.encode());
                    continue;
                }
                let far = i32::try_from(offset)
                    .ok()
                    .and_then(|offset| Instruction::try_ax(Op::JumpLong, offset))
                    .ok_or_else(|| {
                        CompileError::limit(
                            "the code of one function spans at most 8388607 instructions",
                        )
                    })?;
                let skip = match op {
                    Op::Jump => None,
                    Op::IterNext => Some(Op::IterNext),
                    _ => Some(inverted(op).ok_or_else(|| {
                        CompileError::unsupported("a jump the compiler cannot lengthen")
                    })?),
                };
                if let Some(skip) = skip {
                    let (ext, main) = Instruction::wide_asbx(skip, a, 1);
                    code.extend(ext.map(Instruction::encode));
                    code.push(main.encode());
                }
                if op == Op::IterNext {
                    code.push(Instruction::asbx(Op::Jump, 0, 1).encode());
                }
                code.push(far.encode());
            }
        }
    }
    Ok((code, starts))
}

/// The offset of the jump at item `index` to item `target`, relative to the
/// word after it.
fn jump_offset(starts: &[usize], index: usize, target: usize) -> Result<i64, CompileError> {
    let to = starts
        .get(target)
        .ok_or_else(|| CompileError::unsupported("a jump was never given a target"))?;
    let from = starts
        .get(index + 1)
        .ok_or_else(|| CompileError::unsupported("a jump is outside its function"))?;
    let to = i64::try_from(*to).map_err(|_| CompileError::unsupported("a jump overflowed"))?;
    let from = i64::try_from(*from).map_err(|_| CompileError::unsupported("a jump overflowed"))?;
    Ok(to - from)
}

fn compile_function(
    ctx: &mut ModuleCtx<'_>,
    function: &hir::Function,
    boxed: &BTreeSet<u32>,
    options: Options,
    source: Option<(u32, &BTreeMap<NodeId, NodeSpan>)>,
) -> Result<Function, CompileError> {
    // A function too wide to have a frame at all is located at its own
    // declaration, which is all there is to point at before any code exists.
    let declared_at = |ctx: &ModuleCtx<'_>| {
        source.and_then(|(index, spans)| {
            let span = spans.get(&function.node)?;
            let path = ctx.program.modules.get(index as usize)?.path.as_str();
            Some(hollowscript_core::Location {
                file: std::sync::Arc::from(path),
                span: hollowscript_core::Span {
                    start: span.start,
                    end: span.end,
                },
                line: span.line,
                column: span.column,
            })
        })
    };
    let local_count = (function.local_count as usize).max(function.params.len());
    if let Err(e) = check_local_count(local_count) {
        return Err(e.located(|| declared_at(ctx)));
    }
    let arity = imm16(function.params.len())?;
    let local_count = u16::try_from(local_count).map_err(|_| too_many_registers())?;
    let mut fb = FnBuilder::new(
        ctx,
        local_count,
        boxed.clone(),
        function.ret.is_some(),
        false,
        source,
    );

    // Box every captured parameter in place, at entry, before the body runs.
    for param in &function.params {
        if fb.is_boxed(param.local) {
            let reg = register_of(param.local)?;
            fb.abc(Op::NewCell, reg, reg, 0);
        }
    }

    compile_block(&mut fb, &function.body)?;
    let captures = translate_captures(&function.captures)?;
    let at = fb.location(function.node);
    let Finished {
        register_count,
        code,
        constants,
        spans,
    } = fb.finish(at)?;
    let name = ctx.string(function.name)?;
    let _ = options;
    Ok(Function {
        name,
        kind: FunctionKind::Closure,
        arity,
        register_count,
        returns_value: function.ret.is_some(),
        captures,
        constants,
        code,
        spans,
    })
}

fn translate_captures(captures: &[Capture]) -> Result<Vec<CaptureSource>, CompileError> {
    captures
        .iter()
        .map(|capture| match capture {
            Capture::Local(local) => Ok(CaptureSource::Register(register_of(*local)?)),
            Capture::Upvalue(index) => Ok(CaptureSource::Upvalue(imm16(*index as usize)?)),
        })
        .collect()
}

/// The order in which a module's globals and free statements actually run:
/// merged by the `NodeId` each carries, which is assigned in source order.
enum InitStep<'a> {
    Global(&'a hir::Global),
    Stmt(&'a Stmt),
}

fn module_init_order(module: &hir::Module) -> Vec<InitStep<'_>> {
    let mut steps: Vec<InitStep<'_>> =
        Vec::with_capacity(module.globals.len() + module.init.stmts.len());
    steps.extend(module.globals.iter().map(InitStep::Global));
    steps.extend(module.init.stmts.iter().map(InitStep::Stmt));
    steps.sort_by_key(|step| match step {
        InitStep::Global(g) => g.node.0,
        InitStep::Stmt(s) => s.node.0,
    });
    steps
}

fn compile_module_init(
    ctx: &mut ModuleCtx<'_>,
    module: &hir::Module,
    options: Options,
    module_index: u32,
) -> Result<Function, CompileError> {
    for global in &module.globals {
        if let Some(init) = &global.init {
            check_expr_depth(init)?;
        }
    }
    check_block_depth(&module.init)?;
    let mut boxed = BTreeSet::new();
    for global in &module.globals {
        if let Some(init) = &global.init {
            walk_expr_for_closures(ctx.program, init, &mut boxed);
        }
    }
    boxed.extend(collect_boxed_locals(ctx.program, &module.init));
    let local_count = max_local_id(&module.init).map_or(0, |m| m as usize + 1);
    check_local_count(local_count)?;
    let local_count = u16::try_from(local_count).map_err(|_| too_many_registers())?;
    let mut fb = FnBuilder::new(
        ctx,
        local_count,
        boxed,
        false,
        true,
        Some((module_index, &module.node_spans)),
    );

    for step in module_init_order(module) {
        match step {
            // A global a destructuring declared has no initialiser of its
            // own: the `Let` that follows evaluates one and the
            // `AssignGlobal`s after it write each name.
            InitStep::Global(global) if global.init.is_none() => {}
            InitStep::Global(global) => {
                fb.at(global.node);
                // Section 11.3: "Executing any statement: a declaration ...
                // charged 1", and a top-level `constant`/`variable` is a
                // declaration exactly as a local one is. `compile_stmt`
                // charges every statement it compiles this way already;
                // this is the one other place a statement is compiled,
                // for the module's own top-level bindings, which needs the
                // same charge or a program's step count depends on whether
                // a declaration happens to be local or top-level.
                fb.charge(1);
                let Some(init) = &global.init else {
                    continue;
                };
                let reg = compile_expr(&mut fb, init)?;
                let slot = *fb
                    .ctx
                    .global_slot
                    .get(&(global.id.module, global.id.index))
                    .ok_or_else(|| CompileError::unsupported("a global was not assigned a slot"))?;
                fb.abx(Op::SetGlobal, reg, slot);
                fb.reset_temps();
            }
            InitStep::Stmt(stmt) => {
                compile_stmt(&mut fb, stmt)?;
                fb.reset_temps();
            }
        }
    }

    // Module code too long for a jump to cross is located at the start of
    // the module, which is all of it.
    let at = Some(hollowscript_core::Location {
        file: fb.module_path.clone(),
        span: hollowscript_core::Span { start: 0, end: 0 },
        line: 1,
        column: 1,
    });
    let Finished {
        register_count,
        code,
        constants,
        spans,
    } = fb.finish(at)?;
    let name = ctx.literal_string("<module init>")?;
    let _ = options;
    Ok(Function {
        name,
        kind: FunctionKind::ModuleInit,
        arity: 0,
        register_count,
        returns_value: false,
        captures: Vec::new(),
        constants,
        code,
        spans,
    })
}

fn max_local_id(block: &Block) -> Option<u32> {
    fn bump(max: &mut Option<u32>, id: u32) {
        *max = Some(max.map_or(id, |m: u32| m.max(id)));
    }
    fn walk_block(block: &Block, max: &mut Option<u32>) {
        for stmt in &block.stmts {
            walk_stmt(stmt, max);
        }
    }
    fn walk_pattern(pattern: &Pattern, max: &mut Option<u32>) {
        match &pattern.kind {
            PatternKind::Binding(Some(local)) => bump(max, local.0),
            PatternKind::Variant { args, .. } => {
                for arg in args {
                    walk_pattern(arg, max);
                }
            }
            PatternKind::Record(fields) => {
                for field in fields {
                    walk_pattern(&field.pattern, max);
                }
            }
            PatternKind::List { elements, rest } => {
                for element in elements {
                    walk_pattern(element, max);
                }
                if let Some(Some(local)) = rest {
                    bump(max, local.0);
                }
            }
            PatternKind::Binding(None) | PatternKind::Literal(_) | PatternKind::None => {}
        }
    }
    fn walk_stmt(stmt: &Stmt, max: &mut Option<u32>) {
        match &stmt.kind {
            StmtKind::Let {
                bindings, pattern, ..
            } => {
                for binding in bindings {
                    bump(max, binding.local.0);
                }
                if let Some(pattern) = pattern {
                    walk_pattern(pattern, max);
                }
            }
            StmtKind::If {
                then, otherwise, ..
            } => {
                walk_block(then, max);
                if let Some(otherwise) = otherwise {
                    walk_block(otherwise, max);
                }
            }
            StmtKind::Match(m) => {
                for arm in &m.arms {
                    for binding in &arm.bindings {
                        bump(max, binding.local.0);
                    }
                    for pattern in &arm.patterns {
                        walk_pattern(pattern, max);
                    }
                    walk_block(&arm.body, max);
                }
            }
            StmtKind::For { bindings, body, .. } => {
                for binding in bindings {
                    bump(max, binding.local.0);
                }
                walk_block(body, max);
            }
            StmtKind::While { body, .. } => walk_block(body, max),
            _ => {}
        }
    }
    let mut max = None;
    walk_block(block, &mut max);
    max
}

fn compile_block(fb: &mut FnBuilder<'_, '_>, block: &Block) -> Result<(), CompileError> {
    for stmt in &block.stmts {
        compile_stmt(fb, stmt)?;
        fb.reset_temps();
    }
    Ok(())
}

/// Compiles one statement, locating any error it raises at the statement
/// unless something inside it already located it more precisely.
fn compile_stmt(fb: &mut FnBuilder<'_, '_>, stmt: &Stmt) -> Result<(), CompileError> {
    compile_stmt_inner(fb, stmt).map_err(|e| e.located(|| fb.location(stmt.node)))
}

fn store_local(fb: &mut FnBuilder<'_, '_>, local: LocalId, value: Reg) -> Result<(), CompileError> {
    let reg = register_of(local)?;
    if fb.is_boxed(local) {
        fb.abc(Op::SetCell, reg, value, 0);
    } else if reg != value {
        fb.mov(reg, value);
    }
    // `value` is a compiler temporary from `compile_expr`; clear it after
    // storing so a dead list handle cannot look like a live alias to a later
    // copy-on-write optimisation in this frame.
    if reg != value {
        fb.abc(Op::LoadNone, value, 0, 0);
    }
    Ok(())
}

fn bind_local_from_declaration(
    fb: &mut FnBuilder<'_, '_>,
    local: LocalId,
    value: Reg,
) -> Result<(), CompileError> {
    let reg = register_of(local)?;
    if fb.is_boxed(local) {
        fb.abc(Op::NewCell, reg, value, 0);
    } else if reg != value {
        fb.mov(reg, value);
    }
    Ok(())
}

fn compile_stmt_inner(fb: &mut FnBuilder<'_, '_>, stmt: &Stmt) -> Result<(), CompileError> {
    fb.at(stmt.node);
    fb.charge(1);
    match &stmt.kind {
        StmtKind::Let {
            bindings,
            pattern,
            init,
        } => {
            let value = compile_expr(fb, init)?;
            if let Some(pattern) = pattern {
                let mut fail = Vec::new();
                compile_pattern(fb, pattern, value, Refutability::Binding, &mut fail)?;
                // A `let` pattern is exhaustive by construction (checked
                // elsewhere): there is nowhere to jump on failure, but the
                // pattern compiler always leaves a well-formed program even
                // if one were produced, by falling through here regardless.
                let here = fb.here();
                for pc in fail {
                    fb.patch_jump(pc, here)?;
                }
            } else if let [binding] = bindings.as_slice() {
                bind_local_from_declaration(fb, binding.local, value)?;
            }
            Ok(())
        }
        StmtKind::Assign { local, value } => {
            let value = compile_expr(fb, value)?;
            store_local(fb, *local, value)
        }
        StmtKind::AssignUpvalue { index, value } => {
            let value = compile_expr(fb, value)?;
            fb.abc(Op::SetUpval, value, imm16(*index as usize)?, 0);
            Ok(())
        }
        StmtKind::AssignGlobal { global, value } => {
            let value = compile_expr(fb, value)?;
            let slot = *fb
                .ctx
                .global_slot
                .get(&(global.module, global.index))
                .ok_or_else(|| {
                    CompileError::unsupported("a global assignment names an unknown slot")
                })?;
            fb.abx(Op::SetGlobal, value, slot);
            // The expression result is a dead temporary after the global is
            // assigned. Clearing it lets the VM distinguish the one binding
            // being replaced from another live alias of the same list.
            fb.abc(Op::LoadNone, value, 0, 0);
            Ok(())
        }
        StmtKind::If {
            condition,
            then,
            otherwise,
            ..
        } => {
            let cond = compile_expr(fb, condition)?;
            let jump_to_else = fb.jump(Op::JumpIfNot, cond);
            fb.reset_temps();
            compile_block(fb, then)?;
            if let Some(otherwise) = otherwise {
                let jump_to_end = fb.jump(Op::Jump, 0);
                let else_start = fb.here();
                fb.patch_jump(jump_to_else, else_start)?;
                compile_block(fb, otherwise)?;
                let end = fb.here();
                fb.patch_jump(jump_to_end, end)?;
            } else {
                let end = fb.here();
                fb.patch_jump(jump_to_else, end)?;
            }
            Ok(())
        }
        StmtKind::Match(m) => compile_match(fb, m),
        StmtKind::For {
            bindings,
            pattern,
            iterable,
            body,
            ..
        } => compile_for(fb, bindings, pattern.as_ref(), iterable, body),
        StmtKind::While {
            condition, body, ..
        } => {
            let loop_start = fb.here();
            fb.charge(1);
            let cond = compile_expr(fb, condition)?;
            let jump_to_end = fb.jump(Op::JumpIfNot, cond);
            fb.reset_temps();
            fb.loops.push(LoopCtx {
                break_patches: Vec::new(),
                continue_pc: loop_start,
            });
            compile_block(fb, body)?;
            let back = fb.jump(Op::Jump, 0);
            fb.patch_jump(back, loop_start)?;
            let end = fb.here();
            fb.patch_jump(jump_to_end, end)?;
            let ctx = fb
                .loops
                .pop()
                .ok_or_else(|| CompileError::unsupported("loop stack underflow"))?;
            for pc in ctx.break_patches {
                fb.patch_jump(pc, end)?;
            }
            Ok(())
        }
        StmtKind::Break { depth } => {
            let index = loop_index(fb, *depth)?;
            let pc = fb.jump(Op::Jump, 0);
            fb.loops
                .get_mut(index)
                .ok_or_else(|| {
                    CompileError::unsupported("break names a loop that does not enclose it")
                })?
                .break_patches
                .push(pc);
            Ok(())
        }
        StmtKind::Continue { depth } => {
            let index = loop_index(fb, *depth)?;
            let target = fb
                .loops
                .get(index)
                .ok_or_else(|| {
                    CompileError::unsupported("continue names a loop that does not enclose it")
                })?
                .continue_pc;
            let pc = fb.jump(Op::Jump, 0);
            fb.patch_jump(pc, target)
        }
        StmtKind::Return(Some(expr)) => {
            let value = compile_expr(fb, expr)?;
            fb.abc(Op::Return, value, 0, 0);
            Ok(())
        }
        StmtKind::Return(None) => {
            fb.abc(Op::ReturnNone, 0, 0, 0);
            Ok(())
        }
        StmtKind::Call(expr) => {
            compile_expr(fb, expr)?;
            Ok(())
        }
    }
}

fn loop_index(fb: &FnBuilder<'_, '_>, depth: u32) -> Result<usize, CompileError> {
    let len = fb.loops.len();
    let depth = depth as usize;
    len.checked_sub(1)
        .and_then(|last| last.checked_sub(depth))
        .ok_or_else(|| {
            CompileError::unsupported("break or continue names a loop that does not enclose it")
        })
}

fn compile_for(
    fb: &mut FnBuilder<'_, '_>,
    bindings: &[hir::LocalBinding],
    pattern: Option<&Pattern>,
    iterable: &Expr,
    body: &Block,
) -> Result<(), CompileError> {
    let collection = compile_expr(fb, iterable)?;
    let iter_base = fb.alloc_block(3)?;
    // `IterNext` keeps the loop's state in these three registers across
    // every iteration, so they must outlive the body's statements; without
    // raising the floor, the first temporary of the body would reuse them
    // and the second iteration would read whatever the body left there.
    let saved_floor = fb.temp_floor;
    fb.temp_floor = u32::from(iter_base) + 3;
    fb.abc(Op::IterInit, iter_base, collection, 0);
    let loop_start = fb.here();
    let jump_to_end = fb.jump(Op::IterNext, iter_base);
    let element = iter_base
        .checked_add(2)
        .ok_or_else(|| CompileError::unsupported("iterator registers overflowed"))?;
    fb.reset_temps();
    if let Some(pattern) = pattern {
        let mut fail = Vec::new();
        compile_pattern(fb, pattern, element, Refutability::Binding, &mut fail)?;
        let here = fb.here();
        for pc in fail {
            fb.patch_jump(pc, here)?;
        }
    } else if let [binding] = bindings {
        bind_local_from_declaration(fb, binding.local, element)?;
    }
    fb.loops.push(LoopCtx {
        break_patches: Vec::new(),
        continue_pc: loop_start,
    });
    compile_block(fb, body)?;
    let back = fb.jump(Op::Jump, 0);
    fb.patch_jump(back, loop_start)?;
    let end = fb.here();
    fb.patch_jump(jump_to_end, end)?;
    let ctx = fb
        .loops
        .pop()
        .ok_or_else(|| CompileError::unsupported("loop stack underflow"))?;
    for pc in ctx.break_patches {
        fb.patch_jump(pc, end)?;
    }
    fb.temp_floor = saved_floor;
    fb.reset_temps();
    Ok(())
}

fn compile_match(fb: &mut FnBuilder<'_, '_>, m: &Match) -> Result<(), CompileError> {
    let scrutinee = compile_expr(fb, &m.scrutinee)?;
    // Every arm's pattern test reads this register, and an arm's body frees
    // the temporaries allocated since the top of its statement. Without
    // raising the floor over the scrutinee, the second arm's first temporary
    // reuses it: `VariantIs` writes its answer where the value being matched
    // was, and the `GetPayload` that follows reads a `Bool`.
    let saved_floor = fb.temp_floor;
    fb.temp_floor = u32::from(scrutinee) + 1;
    let mut end_jumps = Vec::new();
    let mut next_arm_jumps: Vec<usize> = Vec::new();
    for arm in &m.arms {
        let here = fb.here();
        for pc in next_arm_jumps.drain(..) {
            fb.patch_jump(pc, here)?;
        }
        if let [single] = arm.patterns.as_slice() {
            compile_pattern(
                fb,
                single,
                scrutinee,
                Refutability::Arm,
                &mut next_arm_jumps,
            )?;
            compile_block(fb, &arm.body)?;
            let jump_end = fb.jump(Op::Jump, 0);
            end_jumps.push(jump_end);
        } else {
            let mut success_jumps = Vec::new();
            let count = arm.patterns.len();
            for (index, pattern) in arm.patterns.iter().enumerate() {
                let mut fail = Vec::new();
                compile_pattern(fb, pattern, scrutinee, Refutability::Arm, &mut fail)?;
                if index + 1 == count {
                    next_arm_jumps.extend(fail);
                } else {
                    let jump_success = fb.jump(Op::Jump, 0);
                    success_jumps.push(jump_success);
                    let here = fb.here();
                    for pc in fail {
                        fb.patch_jump(pc, here)?;
                    }
                }
            }
            let here = fb.here();
            for pc in success_jumps {
                fb.patch_jump(pc, here)?;
            }
            compile_block(fb, &arm.body)?;
            let jump_end = fb.jump(Op::Jump, 0);
            end_jumps.push(jump_end);
        }
    }
    // If the last arm's own pattern can still fail, patch that failure to a
    // trap rather than to `end`: an empty `next_arm_jumps` here means the
    // last arm cannot fail (an `else`, or a bare binding), so this statement
    // truly has nowhere left to fall through to and needs no trap of its
    // own. Otherwise, whether or not the checker's own exhaustiveness
    // analysis (section 7.7) called this `match` complete, a value that
    // reaches here matched none of its arms; a correct program never does,
    // so this is the same defence-in-depth idiom `FnBuilder::finish` uses
    // for its own unreachable tail, HS1099, rather than silently falling
    // through to whatever bytecode happens to follow.
    if !next_arm_jumps.is_empty() {
        let trap = fb.here();
        for pc in next_arm_jumps {
            fb.patch_jump(pc, trap)?;
        }
        fb.abx(Op::Trap, 0, 0);
    }
    let end = fb.here();
    for pc in end_jumps {
        fb.patch_jump(pc, end)?;
    }
    fb.temp_floor = saved_floor;
    fb.reset_temps();
    Ok(())
}

/// Compiles a pattern test against `value` (already evaluated). On a
/// mismatch, a jump to be patched to "try the next alternative" is appended
/// to `fail`; on a match, execution falls through having bound every name
/// the pattern introduces.
/// Whether a pattern may fail to match, which decides how a list pattern
/// tests the length it needs.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Refutability {
    /// A `match` arm: a mismatch falls through to the next arm, so the
    /// length is tested gracefully and a failure jumps to `fail`.
    Arm,
    /// A `let` or `for` binding, which has no next arm. The checker makes
    /// every other part of such a pattern exhaustive, but a list's length is
    /// a run-time property it cannot: `constant [a, b] = f()` where `f`
    /// answers one element is exactly what HS1014 is for, so the length is
    /// tested by `ListCheck`, which faults.
    Binding,
}

fn compile_pattern(
    fb: &mut FnBuilder<'_, '_>,
    pattern: &Pattern,
    value: Reg,
    how: Refutability,
    fail: &mut Vec<usize>,
) -> Result<(), CompileError> {
    match &pattern.kind {
        PatternKind::Binding(local) => {
            if let Some(local) = local {
                bind_local_from_declaration(fb, *local, value)?;
            }
            Ok(())
        }
        PatternKind::None => {
            fail.push(fb.jump(Op::JumpIfSome, value));
            Ok(())
        }
        PatternKind::Literal(constant) => {
            let expected = compile_constant(fb, constant)?;
            let test = fb.alloc_temp()?;
            fb.abc(Op::Eq, test, value, expected);
            fail.push(fb.jump(Op::JumpIfNot, test));
            Ok(())
        }
        PatternKind::Variant { index, args } => {
            let test = fb.alloc_temp()?;
            let c = imm16(*index as usize)?;
            fb.abc(Op::VariantIs, test, value, c);
            fail.push(fb.jump(Op::JumpIfNot, test));
            for (position, arg) in args.iter().enumerate() {
                let payload = fb.alloc_temp()?;
                fb.abc(Op::GetPayload, payload, value, imm16(position)?);
                compile_pattern(fb, arg, payload, how, fail)?;
            }
            Ok(())
        }
        PatternKind::Record(fields) => {
            // Each field carries its own position in the record type's
            // field order, which is the same order
            // `shape_of`/`ExprKind::Record` key their `GetField`/`NewRecord`
            // positions on. A pattern may write a subset of the fields, in
            // any order, so its own indices say nothing about where a field
            // is read from: `{ y }` on a `{ x: Int, y: Int }` reads
            // position 1, not position 0.
            for field in fields {
                let field_reg = fb.alloc_temp()?;
                let position = imm16(field.position as usize)?;
                fb.abc(Op::GetField, field_reg, value, position);
                compile_pattern(fb, &field.pattern, field_reg, how, fail)?;
            }
            Ok(())
        }
        PatternKind::List { elements, rest } => {
            let want = imm16(elements.len())?;
            match how {
                // A `match` arm tests the length gracefully, from `ListLen`
                // and a comparison, so a mismatch can fall through to the
                // next arm.
                Refutability::Arm => {
                    let len = fb.alloc_temp()?;
                    fb.abc(Op::ListLen, len, value, 0);
                    let want_int = i64::from(want);
                    let want_reg = compile_constant(fb, &HirConstant::Int(want_int))?;
                    let test = fb.alloc_temp()?;
                    let cmp_op = if rest.is_some() { Op::Ge } else { Op::Eq };
                    fb.abc(cmp_op, test, len, want_reg);
                    fail.push(fb.jump(Op::JumpIfNot, test));
                }
                // A binding has no next arm: a list that does not fit is
                // HS1014, which is what `ListCheck` raises.
                Refutability::Binding => {
                    let at_least = u16::from(rest.is_some());
                    fb.abc(Op::ListCheck, value, want, at_least);
                }
            }
            for (position, element) in elements.iter().enumerate() {
                let item = fb.alloc_temp()?;
                fb.abc(Op::ListGet, item, value, imm16(position)?);
                compile_pattern(fb, element, item, how, fail)?;
            }
            if let Some(Some(rest_local)) = rest {
                let tail = fb.alloc_temp()?;
                fb.abc(Op::ListRest, tail, value, imm16(elements.len())?);
                bind_local_from_declaration(fb, *rest_local, tail)?;
            }
            Ok(())
        }
    }
}

fn compile_constant(
    fb: &mut FnBuilder<'_, '_>,
    constant: &HirConstant,
) -> Result<Reg, CompileError> {
    let dest = fb.alloc_temp()?;
    match constant {
        HirConstant::Int(value) => {
            if let Ok(small) = i16::try_from(*value) {
                fb.asbx(Op::LoadInt, dest, small);
            } else {
                let k = fb.const_int(*value)?;
                fb.abx(Op::LoadK, dest, k);
            }
        }
        HirConstant::Float(value) => {
            let k = fb.const_float(*value)?;
            fb.abx(Op::LoadK, dest, k);
        }
        HirConstant::Bool(true) => {
            fb.abc(Op::LoadTrue, dest, 0, 0);
        }
        HirConstant::Bool(false) => {
            fb.abc(Op::LoadFalse, dest, 0, 0);
        }
        HirConstant::String(value) => {
            let k = fb.const_str(value)?;
            fb.abx(Op::LoadK, dest, k);
        }
        HirConstant::None => {
            fb.abc(Op::LoadNone, dest, 0, 0);
        }
    }
    Ok(dest)
}

/// Compiles `expr` into the first free register, and returns it: every
/// register above it that the expression used on the way is free again
/// afterwards (see [`FnBuilder`]'s own documentation). An error is located
/// at `expr` unless something inside it was located more precisely.
fn compile_expr(fb: &mut FnBuilder<'_, '_>, expr: &Expr) -> Result<Reg, CompileError> {
    let mark = fb.next_temp;
    let value = compile_expr_inner(fb, expr).map_err(|e| e.located(|| fb.location(expr.node)))?;
    let dest = reg_at(mark as usize).map_err(|e| e.located(|| fb.location(expr.node)))?;
    // Every case below leaves its value in the first register it took, so
    // this move is never emitted; it is here so that a case that one day
    // does not still leaves the stack discipline intact.
    if value != dest {
        fb.mov(dest, value);
    }
    fb.next_temp = mark + 1;
    fb.high_water = fb.high_water.max(fb.next_temp);
    Ok(dest)
}

/// Compiles each of `exprs` into consecutive registers, starting at the
/// first free one, and returns how many there were. [`compile_expr`] leaves
/// every value in the first free register and frees everything above it, so
/// the next one lands right after it without a move.
fn compile_run(fb: &mut FnBuilder<'_, '_>, exprs: &[Expr]) -> Result<u16, CompileError> {
    for expr in exprs {
        compile_expr(fb, expr)?;
    }
    imm16(exprs.len())
}

fn compile_expr_inner(fb: &mut FnBuilder<'_, '_>, expr: &Expr) -> Result<Reg, CompileError> {
    match &expr.kind {
        ExprKind::Literal(constant) => compile_constant(fb, constant),
        ExprKind::Filter(source) => {
            let dest = fb.alloc_temp()?;
            let k = fb.const_filter(source)?;
            fb.abx(Op::LoadK, dest, k);
            Ok(dest)
        }
        ExprKind::Local(local) => {
            let reg = register_of(*local)?;
            let dest = fb.alloc_temp()?;
            if fb.is_boxed(*local) {
                fb.abc(Op::GetCell, dest, reg, 0);
            } else {
                fb.mov(dest, reg);
            }
            Ok(dest)
        }
        ExprKind::Upvalue(index) => {
            let dest = fb.alloc_temp()?;
            fb.abc(Op::GetUpval, dest, imm16(*index as usize)?, 0);
            Ok(dest)
        }
        ExprKind::Global(id) => {
            let dest = fb.alloc_temp()?;
            let slot = *fb
                .ctx
                .global_slot
                .get(&(id.module, id.index))
                .ok_or_else(|| CompileError::unsupported("a global read names an unknown slot"))?;
            fb.abx(Op::GetGlobal, dest, slot);
            Ok(dest)
        }
        ExprKind::NarrowedPath(path) => {
            // Narrowing is a static analysis with no run-time cost
            // (section 4.8), so reading a narrowed path is exactly reading
            // the path: the root, then one `GetField` per field access, at
            // the position `hir::PathField` carries, each reading the
            // register it writes.
            let dest = fb.alloc_temp()?;
            match path.root {
                PathRoot::Local(local) => {
                    let reg = register_of(local)?;
                    if fb.is_boxed(local) {
                        fb.abc(Op::GetCell, dest, reg, 0);
                    } else {
                        fb.mov(dest, reg);
                    }
                }
                PathRoot::Global(id) => {
                    let slot =
                        *fb.ctx
                            .global_slot
                            .get(&(id.module, id.index))
                            .ok_or_else(|| {
                                CompileError::unsupported(
                                    "a narrowed global read names an unknown slot",
                                )
                            })?;
                    fb.abx(Op::GetGlobal, dest, slot);
                }
            }
            for field in &path.fields {
                fb.abc(Op::GetField, dest, dest, imm16(field.index as usize)?);
            }
            Ok(dest)
        }
        // A capability, or one member of a namespace capability, wanted as a
        // value rather than called: `Op::CapFn` builds a function value for
        // it, and calling that value reaches the same capability. An
        // immediate `effect` call does not come through here, because
        // `ExprKind::EffectCall` compiles it directly to `Op::CallCap`.
        ExprKind::Capability(id) => {
            let cap_index = fb.ctx.capability_of(*id)?;
            let arity = fb.ctx.cap_declared_arity(cap_index, None)?;
            let cap_call = fb.ctx.cap_call_of(*id, None, arity)?;
            let dest = fb.alloc_temp()?;
            fb.abx(Op::CapFn, dest, cap_call);
            Ok(dest)
        }
        ExprKind::CapabilityMember {
            capability,
            name,
            index,
        } => {
            let cap_index = fb.ctx.capability_of(*capability)?;
            let member_id = fb.ctx.string(*name)?;
            let arity = fb.ctx.cap_declared_arity(cap_index, Some(member_id))?;
            let cap_call = fb
                .ctx
                .cap_call_of(*capability, Some((*name, *index)), arity)?;
            let dest = fb.alloc_temp()?;
            fb.abx(Op::CapFn, dest, cap_call);
            Ok(dest)
        }
        ExprKind::NativeValue(native) => {
            let import = fb.ctx.native_of(*native)?;
            let dest = fb.alloc_temp()?;
            fb.abx(Op::NativeFn, dest, import);
            Ok(dest)
        }
        ExprKind::FunctionValue(fid) | ExprKind::Closure(fid) => {
            let dest = fb.alloc_temp()?;
            fb.abx(Op::Closure, dest, fid.0);
            Ok(dest)
        }
        ExprKind::VariantValue { decl, index } => {
            let variant_index = fb.ctx.variant_of(*decl, *index, 0)?;
            let dest = fb.alloc_temp()?;
            fb.abx(Op::VariantFn, dest, variant_index);
            Ok(dest)
        }
        ExprKind::List(items) => {
            // `NewList`'s dest register comes right before its run of
            // elements, which it reads from `a+1..`. A literal longer than
            // one chunk is built by `NewList` of the first chunk and then
            // `ListAppend`s of the rest into the same list, each chunk's
            // values in the same registers again, so the elements are still
            // evaluated in order and the list is charged as one literal.
            let dest = fb.alloc_temp()?;
            let mut chunks = items.chunks(LIST_CHUNK);
            let first = chunks.next().unwrap_or(&[]);
            let count = compile_run(fb, first)?;
            fb.abx(Op::NewList, dest, u32::from(count));
            for chunk in chunks {
                fb.next_temp = u32::from(dest) + 1;
                let count = compile_run(fb, chunk)?;
                fb.abc(Op::ListAppend, dest, dest + 1, count);
            }
            Ok(dest)
        }
        ExprKind::Map(pairs) => {
            // As a list, a chunk at a time: `NewMap` of the first chunk's
            // pairs, then `MapPut`s of the rest, each pair a key register
            // followed by its value's.
            let dest = fb.alloc_temp()?;
            let mut chunks = pairs.chunks(MAP_CHUNK);
            let first = chunks.next().unwrap_or(&[]);
            let count = compile_pairs(fb, first)?;
            fb.abx(Op::NewMap, dest, u32::from(count));
            for chunk in chunks {
                fb.next_temp = u32::from(dest) + 1;
                let count = compile_pairs(fb, chunk)?;
                fb.abc(Op::MapPut, dest, dest + 1, count);
            }
            Ok(dest)
        }
        ExprKind::Record(fields) => {
            let names: Vec<String> = fields
                .iter()
                .map(|(name, _)| fb.ctx.symbols.resolve(*name).to_owned())
                .collect();
            let (shape, order) = fb.ctx.shape_of(&names)?;
            // The fields are evaluated in the shape's (alphabetical) order,
            // each straight into its own position of the run `NewRecord`
            // reads, which is the order this compiler has always evaluated
            // them in.
            let dest = fb.alloc_temp()?;
            for &field_index in &order {
                compile_expr(fb, &at(fields, field_index)?.1)?;
            }
            fb.abx(Op::NewRecord, dest, shape);
            Ok(dest)
        }
        ExprKind::Unary { op, operand } => {
            let value = compile_expr(fb, operand)?;
            // The operator, not its operand, is where an overflow is raised
            // (only `NegInt` can raise one, negating the minimum `Int`; see
            // `ExprKind::Binary`'s own note just below for the same rule).
            fb.at(expr.node);
            let opcode = match op {
                hir::UnaryOp::NegInt => Op::NegInt,
                hir::UnaryOp::NegFloat => Op::NegFloat,
            };
            fb.abc(opcode, value, value, 0);
            Ok(value)
        }
        ExprKind::Not(operand) => {
            let value = compile_expr(fb, operand)?;
            fb.abc(Op::Not, value, value, 0);
            Ok(value)
        }
        ExprKind::Binary { op, lhs, rhs } => {
            // The result replaces the left operand, which nothing reads
            // again: every instruction reads its operands before it writes
            // its result.
            let l = compile_expr(fb, lhs)?;
            let r = compile_expr(fb, rhs)?;
            // The operator, not its operands, is where an overflow or a
            // division by zero is raised.
            fb.at(expr.node);
            let opcode = match op {
                hir::BinaryOp::AddInt => Op::AddInt,
                hir::BinaryOp::SubInt => Op::SubInt,
                hir::BinaryOp::MulInt => Op::MulInt,
                hir::BinaryOp::AddFloat => Op::AddFloat,
                hir::BinaryOp::SubFloat => Op::SubFloat,
                hir::BinaryOp::MulFloat => Op::MulFloat,
                hir::BinaryOp::Div => Op::Div,
                hir::BinaryOp::Eq { .. } => Op::Eq,
                hir::BinaryOp::Ne { .. } => Op::Ne,
                hir::BinaryOp::Lt => Op::Lt,
                hir::BinaryOp::Gt => Op::Gt,
                hir::BinaryOp::Le => Op::Le,
                hir::BinaryOp::Ge => Op::Ge,
            };
            fb.abc(opcode, l, l, r);
            Ok(l)
        }
        ExprKind::Logical { op, lhs, rhs, .. } => {
            let dest = fb.alloc_temp()?;
            let l = compile_expr(fb, lhs)?;
            let short_circuit_op = match op {
                hir::LogicalOp::And => Op::JumpIfNot,
                hir::LogicalOp::Or => Op::JumpIf,
            };
            let short_circuit = fb.jump(short_circuit_op, l);
            compile_expr_into(fb, rhs, dest)?;
            let jump_end = fb.jump(Op::Jump, 0);
            let here = fb.here();
            fb.patch_jump(short_circuit, here)?;
            match op {
                hir::LogicalOp::And => fb.abc(Op::LoadFalse, dest, 0, 0),
                hir::LogicalOp::Or => fb.abc(Op::LoadTrue, dest, 0, 0),
            };
            let end = fb.here();
            fb.patch_jump(jump_end, end)?;
            Ok(dest)
        }
        ExprKind::IntToFloat(operand) => {
            let value = compile_expr(fb, operand)?;
            fb.abc(Op::IntToFloat, value, value, 0);
            Ok(value)
        }
        ExprKind::Call { callee, args } => compile_call(fb, callee, args, expr.node),
        ExprKind::EffectCall { callee, args } => {
            // A capability call is `Op::CallCap`, whether the capability is
            // one function (`print`) or one member of a namespace
            // (`clock.now`). Anything else is an ordinary call.
            let capability = match &callee.kind {
                ExprKind::Capability(id) => Some(fb.ctx.cap_call_of(*id, None, args.len())?),
                ExprKind::CapabilityMember {
                    capability,
                    name,
                    index,
                } => Some(
                    fb.ctx
                        .cap_call_of(*capability, Some((*name, *index)), args.len())?,
                ),
                _ => None,
            };
            if let Some(cap_index) = capability {
                let start = fb.alloc_temp()?;
                compile_run(fb, args)?;
                fb.at(expr.node);
                fb.abx(Op::CallCap, start, cap_index);
                Ok(start)
            } else {
                compile_call(fb, callee, args, expr.node)
            }
        }
        ExprKind::NativeCall { native, args } => {
            let local_index = fb.ctx.native_of(*native)?;
            let start = fb.alloc_temp()?;
            compile_run(fb, args)?;
            fb.at(expr.node);
            fb.abx(Op::CallNative, start, local_index);
            Ok(start)
        }
        ExprKind::VariantConstruct { decl, index, args } => {
            let arity = imm16(args.len())?;
            let variant_index = fb.ctx.variant_of(*decl, *index, arity)?;
            let start = fb.alloc_temp()?;
            compile_run(fb, args)?;
            fb.abx(Op::NewEnum, start, variant_index);
            Ok(start)
        }
        ExprKind::Index { target, index } => {
            let t = compile_expr(fb, target)?;
            let i = compile_expr(fb, index)?;
            fb.abc(Op::Index, t, t, i);
            Ok(t)
        }
        ExprKind::Field { target, index } => {
            let t = compile_expr(fb, target)?;
            fb.abc(Op::GetField, t, t, imm16(*index as usize)?);
            Ok(t)
        }
        ExprKind::Try { operand, .. } => {
            let value = compile_expr(fb, operand)?;
            fb.at(expr.node);
            fb.abc(Op::Try, value, value, 0);
            Ok(value)
        }
        ExprKind::Interpolate { text, exprs } => {
            // One `Interp` of every part, the literal text around each
            // `${}` and the text of each `${}`, so the string is charged
            // once, as section 11.4 says, however many parts it has. The
            // text segments are loaded first and the interpolated values
            // after, each moved into its place between two segments, which
            // is the order this compiler has always evaluated them in.
            let count = text.len() + exprs.len();
            let start = fb.alloc_block(count)?;
            for (i, segment) in text.iter().enumerate() {
                let target = reg_at(usize::from(start) + i * 2)?;
                let k = fb.const_str(segment)?;
                fb.abx(Op::LoadK, target, k);
            }
            for (i, expr) in exprs.iter().enumerate() {
                let target = reg_at(usize::from(start) + i * 2 + 1)?;
                compile_expr_into(fb, expr, target)?;
            }
            fb.abc(Op::Interp, start, start, imm16(count)?);
            Ok(start)
        }
        ExprKind::Error => Err(CompileError::unsupported(
            "a program with an unresolved type error reached the compiler",
        )),
    }
}

/// Compiles each key and value of `pairs` into consecutive registers, key
/// before value, and returns how many pairs there were.
fn compile_pairs(fb: &mut FnBuilder<'_, '_>, pairs: &[(Expr, Expr)]) -> Result<u16, CompileError> {
    for (key, value) in pairs {
        compile_expr(fb, key)?;
        compile_expr(fb, value)?;
    }
    imm16(pairs.len())
}

/// Compiles `expr` so its value ends up in exactly register `dest`, used
/// where a value belongs in a register allocated before it (a short
/// circuit's result, an interpolation's run, whose text segments are loaded
/// first). The temporary it was computed in is free again afterwards.
fn compile_expr_into(
    fb: &mut FnBuilder<'_, '_>,
    expr: &Expr,
    dest: Reg,
) -> Result<(), CompileError> {
    let mark = fb.next_temp;
    let value = compile_expr(fb, expr)?;
    if value != dest {
        fb.mov(dest, value);
    }
    fb.next_temp = mark;
    Ok(())
}

fn compile_call(
    fb: &mut FnBuilder<'_, '_>,
    callee: &Expr,
    args: &[Expr],
    at: NodeId,
) -> Result<Reg, CompileError> {
    let start = compile_expr(fb, callee)?;
    let count = compile_run(fb, args)?;
    // The call, not its last argument, is what raises HS1005.
    fb.at(at);
    fb.abc(Op::Call, start, count, 0);
    Ok(start)
}

#[cfg(test)]
mod tests {
    use super::{Item, UNPATCHED, layout};
    use hollowscript_bytecode::opcode::{Instruction, Op, Wide};

    const FILLER: Item = Item::Fixed(None, Instruction::abc(Op::Move, 0, 1, 0));

    /// `count` fillers, then an end marker.
    fn fillers(count: usize) -> Vec<Item> {
        let mut items = vec![FILLER; count];
        items.push(Item::Fixed(None, Instruction::abc(Op::ReturnNone, 0, 0, 0)));
        items
    }

    fn decode(words: &[u32]) -> Vec<Instruction> {
        words
            .iter()
            .map(|w| Instruction::decode(*w).unwrap())
            .collect()
    }

    /// A jump over exactly `gap` words, forward.
    fn forward(op: Op, a: u16, gap: usize) -> (Vec<u32>, Vec<usize>) {
        let mut items = vec![Item::Jump {
            op,
            a,
            target: gap + 1,
        }];
        items.extend(fillers(gap));
        layout(&items).unwrap()
    }

    #[test]
    fn a_jump_stays_short_up_to_32767_words_and_grows_long_after() {
        let (code, _) = forward(Op::Jump, 0, 32_767);
        let first = decode(&code)[0];
        assert_eq!((first.op, first.sbx()), (Op::Jump, i16::MAX));
        assert_eq!(code.len(), 32_767 + 2);

        let (code, starts) = forward(Op::Jump, 0, 32_768);
        let first = decode(&code)[0];
        assert_eq!((first.op, first.ax()), (Op::JumpLong, 32_768));
        assert_eq!(starts[1], 1, "the long form of Jump is one word");
    }

    #[test]
    fn a_backward_jump_reaches_32768_words_short_and_grows_long_after() {
        // `sbx` reaches one word further back than forward: -32,768.
        for (gap, long) in [(32_767, false), (32_768, true)] {
            let mut items = fillers(gap);
            items.pop();
            items.push(Item::Jump {
                op: Op::Jump,
                a: 0,
                target: 0,
            });
            items.extend(fillers(0));
            let (code, _) = layout(&items).unwrap();
            let jump = decode(&code)[gap];
            if long {
                assert_eq!((jump.op, jump.ax()), (Op::JumpLong, -32_769));
            } else {
                assert_eq!((jump.op, jump.sbx()), (Op::Jump, -32_768));
            }
        }
    }

    #[test]
    fn a_long_conditional_jump_is_the_opposite_test_over_a_jump_long() {
        let (code, _) = forward(Op::JumpIfNot, 7, 40_000);
        let words = decode(&code);
        assert_eq!(
            (words[0].op, words[0].a, words[0].sbx()),
            (Op::JumpIf, 7, 1)
        );
        assert_eq!((words[1].op, words[1].ax()), (Op::JumpLong, 40_000));
        for (op, opposite) in [
            (Op::JumpIf, Op::JumpIfNot),
            (Op::JumpIfNone, Op::JumpIfSome),
            (Op::JumpIfSome, Op::JumpIfNone),
        ] {
            let (code, _) = forward(op, 1, 40_000);
            assert_eq!(decode(&code)[0].op, opposite);
        }
        // A wide register keeps its prefix, in front of the opposite test.
        let (code, _) = forward(Op::JumpIfNot, 300, 40_000);
        let words = decode(&code);
        assert_eq!(words[0].op, Op::Ext);
        let test = Wide::new(Some(words[0]), words[1]);
        assert_eq!((test.op, test.a, test.sbx()), (Op::JumpIf, 300, 1));
        assert_eq!((words[2].op, words[2].ax()), (Op::JumpLong, 40_000));
    }

    #[test]
    fn a_long_iteration_exit_skips_its_jump_long_while_there_are_elements() {
        let (code, _) = forward(Op::IterNext, 4, 40_000);
        let words = decode(&code);
        assert_eq!(
            (words[0].op, words[0].a, words[0].sbx()),
            (Op::IterNext, 4, 1)
        );
        assert_eq!((words[1].op, words[1].sbx()), (Op::Jump, 1));
        assert_eq!((words[2].op, words[2].ax()), (Op::JumpLong, 40_000));
    }

    /// Lengthening one jump moves the code after it, which can put a second
    /// jump over that code out of reach: the layout repeats until nothing
    /// changes.
    #[test]
    fn lengthening_one_jump_can_lengthen_another() {
        // The outer jump crosses the inner one and 32,766 fillers, 32,767
        // words while the inner jump is short and 32,768 once it is long.
        let mut items = vec![
            Item::Jump {
                op: Op::JumpIfNot,
                a: 0,
                target: UNPATCHED,
            },
            Item::Jump {
                op: Op::JumpIfNot,
                a: 0,
                target: 40_000,
            },
        ];
        items.extend(vec![FILLER; 40_000 - 2]);
        items.extend(fillers(0));
        if let Some(Item::Jump { target, .. }) = items.get_mut(0) {
            *target = 32_768;
        }
        let (code, _) = layout(&items).unwrap();
        let words = decode(&code);
        assert_eq!(words[0].op, Op::JumpIf, "the outer jump is lengthened too");
        assert_eq!(words[1].op, Op::JumpLong);
        assert_eq!(words[2].op, Op::JumpIf);
        assert_eq!(words[3].op, Op::JumpLong);
        // Both land where their items start.
        assert_eq!(words[1].ax(), 4 + 32_766 - 2);
        assert_eq!(words[3].ax(), 4 + 39_998 - 4);
    }

    #[test]
    fn an_unpatched_jump_is_an_error_not_a_panic() {
        let mut items = vec![Item::Jump {
            op: Op::Jump,
            a: 0,
            target: UNPATCHED,
        }];
        items.extend(fillers(0));
        assert!(layout(&items).is_err());
    }
}

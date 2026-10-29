//! A WebAssembly build of the VM only.
//!
//! This crate depends on the shared vocabulary, the bytecode container and
//! the VM, and on nothing else in the workspace: no `hollowscript-syntax`, no
//! `hollowscript-check`, no `hollowscript-compiler`. That is the whole point
//! of it existing separately from `hollowscript` (the facade). A published
//! Studio game bundles bytecode compiled at publish time; the browser never
//! sees source, and this crate could not compile it if it wanted to, because
//! the front end is not one of its dependencies. [`hollowscript_bytecode::verify`](hollowscript_bytecode::verify::verify)
//! is what a browser VM trusts instead of trusting its supplier: bytecode
//! that does not verify is refused before a single instruction of it runs,
//! whether it came from this build's own publish pipeline or anywhere else.
//!
//! **Today.** [`WasmModule`] is the plain Rust API this crate offers: verify
//! bytes, then run exported functions, both delegating to
//! `hollowscript-bytecode` and `hollowscript-vm`. It has no unsafe code,
//! because it needs none yet.
//!
//! **The JavaScript boundary.** [`abi`] is a hand-written `extern "C"`
//! surface over the module's own linear memory, with no `wasm-bindgen` and
//! no other dependency, which is what keeps this crate's dependency list
//! the three it declares. A page allocates a buffer, writes bytecode into
//! it, loads it, calls exports by name and reads the answers back as text;
//! `web/` holds the JavaScript wrapper and a page that does exactly that.
//! Values cross as text for the same reason they do at the C ABI: a
//! `Value` of a container kind is a handle into one instance's heap, which
//! a caller outside the VM cannot hold.
//!
//! **The `tooling` feature** is a separate, larger build for an editor page,
//! never what a published game ships: it adds the front end and the checker
//! (through `hollowscript`) and the editor services of `hollowscript-ide`
//! (highlighting, hover, completion, definition, the outline), so a page can
//! check, format, compile and ask about source with no server. The default,
//! feature-less build is exactly what this documentation describes above.
//!
//! This crate and `hollowscript-capi` are the two places in the workspace
//! `unsafe` is allowed, for exactly that boundary. Every block here does
//! one thing behind a `SAFETY` comment, and the rest of the crate is
//! ordinary safe Rust that runs and is tested on the host.

#![allow(
    unsafe_code,
    reason = "a JavaScript host reaches this module through raw pointers into its linear memory"
)]

use hollowscript_bytecode::verify::{LoadError, VerifiedModule, verify_bytes};
use hollowscript_core::fault::Fault;
use hollowscript_core::{Control, Limits};
use hollowscript_vm::{CapabilityTable, Command, CommandResult, Interpreter, Run, Value};

/// The `(capability, function)` pairs the compiled bytecode actually calls,
/// in the order `hollowscript_vm::CapabilityTable::new` expects (the same
/// order [`WasmModule::load`]'s own `capabilities` argument must be built
/// in), named the way a host would recognise them: `"print"`, or
/// `"clock.now"` for a member of a namespace capability.
///
/// A host binds capabilities positionally, by index, not by name at load
/// time (chapter 9's shapes are erased to indices in the bytecode a
/// verified module trusts, per `hollowscript-bytecode`'s own container
/// format). This is how it finds out what those indices mean: read this
/// list before loading, since binding happens up front, through
/// [`WasmModule::load`]'s `capabilities` argument, not afterwards.
///
/// # Errors
///
/// Returns [`LoadError`] exactly as [`WasmModule::load`] does; the bytes
/// are verified the same way, only to answer this before a `CapabilityTable`
/// need be built for them.
pub fn capability_names(bytes: &[u8]) -> Result<Vec<String>, LoadError> {
    let verified = verify_bytes(bytes)?;
    let module = verified.module();
    let mut names = Vec::with_capacity(module.cap_calls.len());
    for call in &module.cap_calls {
        let capability = module.capabilities.get(call.capability as usize);
        let base = capability.map_or("", |c| module.string(c.name));
        let qualified = match capability.map(|c| &c.kind) {
            Some(CapabilityKind::Namespace(functions)) => {
                match functions.get(call.function as usize) {
                    Some((function, _)) => format!("{base}.{}", module.string(*function)),
                    None => base.to_owned(),
                }
            }
            _ => base.to_owned(),
        };
        names.push(qualified);
    }
    Ok(names)
}

/// The record shapes and enum variants `bytes`' own module already has,
/// as a [`hollowscript_vm::HostTypes`] a [`CapabilityTable`] can build against
/// (see [`hollowscript_vm::hostvalues::HostCall::enum_value`] and
/// [`hollowscript_vm::hostvalues::HostCall::record`]).
///
/// Unlike the facade, which mutates a module to add an entry for every
/// record and enum a `CapabilityInterface` mentions before it is compiled
/// (`hollowscript::hosttypes::register`), this crate never compiles
/// anything: the bytecode it loads was already compiled somewhere that ran
/// that step, so every shape and variant a capability might need to build
/// is already in the module's own tables (see `docs/design/capabilities.md`).
/// This just mirrors what is already there, at the position it is already
/// at, into the shape the VM's own capability boundary expects.
///
/// # Errors
///
/// Returns [`LoadError`] exactly as [`capability_names`] does.
pub fn host_types(bytes: &[u8]) -> Result<hollowscript_vm::HostTypes, LoadError> {
    let verified = verify_bytes(bytes)?;
    let module = verified.module();
    let mut types = hollowscript_vm::HostTypes::new();
    for (index, variant) in module.variants.iter().enumerate() {
        let Ok(position) = u32::try_from(index) else {
            continue;
        };
        types.add_variant(
            module.string(variant.enum_name),
            module.string(variant.name),
            position,
        );
    }
    for (index, shape) in module.shapes.iter().enumerate() {
        let Ok(position) = u16::try_from(index) else {
            continue;
        };
        // The module stores a shape's fields sorted by name already
        // (`hollowscript::hosttypes::register_record`'s own doc says so,
        // and it is the only writer of this table), which is the order
        // `HostTypes::add_shape` expects them named in.
        let names = shape
            .fields
            .iter()
            .map(|&id| module.string(id).to_owned())
            .collect();
        let field_ids = shape.fields.iter().map(|id| id.0).collect();
        types.add_shape(names, position, field_ids);
    }
    Ok(types)
}

/// Each `cap_calls` entry's own declared result type (`None` for one that
/// answers nothing), in the same order [`capability_names`] lists them,
/// together with the module's own compiled type table those `TypeRef`s
/// index into: what a host's synchronous capability answer is decoded
/// against, so a bare JSON number becomes the `Int` or `Float` the
/// declaration calls for rather than whichever `Json::parse` guessed from
/// the number's own spelling (see `coerce_json_numbers`'s own
/// documentation).
///
/// # Errors
///
/// Returns [`LoadError`] exactly as [`capability_names`] does.
fn capability_result_types(bytes: &[u8]) -> Result<(Vec<Option<TypeRef>>, TypeTable), LoadError> {
    let verified = verify_bytes(bytes)?;
    let module = verified.module();
    let types = (0..module.cap_calls.len())
        .map(|i| {
            u32::try_from(i)
                .ok()
                .and_then(|i| cap_call_result_type(module, i))
        })
        .collect();
    Ok((types, module.types.clone()))
}

/// The positions in `cap_calls` (as [`capability_names`] lists them) of every
/// call of a capability named `print`, which is the capability whose output
/// counts against `maxOutputBytes` (sections 11.6 and 16.2). A JavaScript host
/// that binds `print` is a function this build cannot look inside, so this
/// build charges it (see [`CapabilityTable::with_output_charged`]) rather than
/// leaving it to the host to remember.
///
/// The declared shape does not matter, and that is deliberate. The standard
/// `print` takes one `String`, but a module can declare a capability called
/// `print` that takes a list of strings (or a record, or two strings), and the
/// host binds handlers by name alone, so the handler behind such a declaration
/// writes whatever it likes with what it is given. A charge that looked only
/// for the standard shape let bytecode declare `print` as `[String]` and
/// deliver 256 MiB through a handler under an output limit of 1,000. What is
/// charged is the text the arguments carry (see the VM's `rendered_size`); for
/// the standard shape that is the string and a line feed, as before.
///
/// # Errors
///
/// Returns [`LoadError`] exactly as [`capability_names`] does.
pub fn print_calls(bytes: &[u8]) -> Result<Vec<u32>, LoadError> {
    let verified = verify_bytes(bytes)?;
    let module = verified.module();
    let mut calls = Vec::new();
    for (index, call) in module.cap_calls.iter().enumerate() {
        let Some(capability) = module.capabilities.get(call.capability as usize) else {
            continue;
        };
        if module.string(capability.name) == "print" {
            if let Ok(index) = u32::try_from(index) {
                calls.push(index);
            }
        }
    }
    Ok(calls)
}

/// A verified, loaded program, ready to run its exported functions. Building
/// one is the only way into this crate: there is no path from source text.
pub struct WasmModule {
    interpreter: Interpreter,
    /// The `cap_calls` index of each [`Command`] [`WasmModule::commands`]
    /// last drained, in order: [`WasmModule::value_of_json_result`] uses
    /// this to find the capability a resume answer is for, since a drained
    /// `Command` itself is gone by the time the host's own answer comes
    /// back (see `coerce_json_numbers`'s own documentation for what this is
    /// needed for).
    last_commands: Vec<u32>,
}

impl std::fmt::Debug for WasmModule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmModule").finish_non_exhaustive()
    }
}

impl WasmModule {
    /// Verifies `bytes` as a bytecode container and prepares it to run, with
    /// `capabilities` bound as its capability table. Does not run the
    /// module's own initialisers; call [`WasmModule::initialise`] next.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError`] if `bytes` is not a well formed container of the
    /// version this build reads, or breaks a rule of the verifier.
    pub fn load(bytes: &[u8], capabilities: CapabilityTable) -> Result<WasmModule, LoadError> {
        let verified: VerifiedModule = verify_bytes(bytes)?;
        Ok(WasmModule {
            interpreter: Interpreter::new(verified, capabilities),
            last_commands: Vec::new(),
        })
    }

    /// Runs the top-level statements of every module in initialisation
    /// order, in a fresh run. `control` is as [`Control`] documents it; pass
    /// [`Control::none`] for a run the host never needs to cancel.
    ///
    /// # Errors
    ///
    /// Returns the fault if initialisation faults, including the fault
    /// HS1006 if `control` says to cancel before this run finishes.
    pub fn initialise(
        &mut self,
        limits: &Limits,
        control: &mut Control<'_>,
    ) -> Result<Option<Value>, Fault> {
        self.interpreter.initialise(limits, control)
    }

    /// Calls an exported function by its index into the module's function
    /// table (the host looks this up once from the module's export table,
    /// which `hollowscript-bytecode::Module::exports` lists by name).
    /// `control` is as [`WasmModule::initialise`]'s.
    ///
    /// # Errors
    ///
    /// Returns the fault if the call faults.
    pub fn call_export(
        &mut self,
        function: u32,
        args: &[Value],
        limits: &Limits,
        control: &mut Control<'_>,
    ) -> Result<Option<Value>, Fault> {
        self.interpreter
            .call_export(function, args, limits, control)
    }

    /// The current value of a global slot.
    #[must_use]
    pub fn global(&self, index: u32) -> Option<Value> {
        self.interpreter.global(index)
    }

    /// The function index of the export named `name`, for
    /// [`WasmModule::call_export`], or `None` if the module exports no
    /// function by that name.
    #[must_use]
    pub fn export_index(&self, name: &str) -> Option<u32> {
        let module = self.interpreter.module().module();
        module.exports.iter().find_map(|e| match &e.kind {
            ExportKind::Function { function, .. } if module.string(e.name) == name => {
                Some(*function)
            }
            _ => None,
        })
    }

    /// The global slot of the exported `constant` named `name`.
    #[must_use]
    pub fn constant_slot(&self, name: &str) -> Option<u32> {
        let module = self.interpreter.module().module();
        module.exports.iter().find_map(|e| match &e.kind {
            ExportKind::Constant { global, .. } if module.string(e.name) == name => Some(*global),
            _ => None,
        })
    }

    /// `value` as the text `corpus/FORMAT.md` fixes for a result, which is
    /// how a value crosses to a caller that cannot hold one.
    #[must_use]
    pub fn render(&self, value: Value) -> String {
        hollowscript_vm::render(
            self.interpreter.module().module(),
            self.interpreter.heap(),
            value,
        )
    }

    /// `value` as JSON, in the encoding `value_to_json`'s own module
    /// documentation defines, with one difference: an `Int` whose magnitude
    /// is above 2^53 - 1 is written as the tagged string `{"bigint":
    /// "<decimal>"}` rather than as a number, so a reader that holds a JSON
    /// number as a double never reads back a different `Int`. This is the
    /// structured counterpart of [`WasmModule::render`], which answers the
    /// text `corpus/FORMAT.md` fixes: both are lossless, and this one is
    /// what a host reads with a JSON parser instead of a text parser.
    ///
    /// The rendering is bounded by `budget` (see [`JsonBudget`]): a value
    /// that shares its sub-values (`t = Tree.Node(t, t)`, repeated) has far
    /// more paths than objects, and this writes one copy of a sub-value for
    /// every path to it, so the text of a small value can be astronomically
    /// long. The walk stops, within one piece of the budget, as soon
    /// as the text passes it.
    ///
    /// # Errors
    ///
    /// HS1004, the output limit, when the text would be longer than `budget`
    /// allows, and HS1006 when the budget's cancellation poll says to stop.
    pub fn render_json(
        &mut self,
        value: Value,
        budget: &mut JsonBudget<'_>,
    ) -> Result<String, Fault> {
        let mut out = String::new();
        self.interpreter
            .with_host_call_read(&Limits::default(), |call| {
                value_to_json(call, value, &mut out, budget)
            })?;
        Ok(out)
    }

    /// Builds the arguments for a call to the exported function at
    /// `function` (the index [`WasmModule::export_index`] gives) from a
    /// JSON array in the encoding `value_to_json`'s own module
    /// documentation defines (the same one `hollowscript-capi`'s C ABI
    /// uses): every transferable type of chapter 12, not only a scalar,
    /// since a caller outside the VM has no way to build a heap value
    /// itself. A bare JSON number decodes to the declared parameter type
    /// (`Int` or `Float`) that `function`'s own compiled signature names at
    /// that position, not by guessing from the JSON text alone (see
    /// `coerce_json_numbers`'s own documentation for why a bare `3` cannot
    /// otherwise say which one it means). A record or enum argument crosses
    /// only when its shape or variant is already registered (see
    /// [`host_types`]).
    ///
    /// Every argument is then accepted by the rule of section 9.6 against the
    /// declared parameter type, as the facade's own `call` does (section 12.4,
    /// step 2): an `Int` for a `Float` parameter is converted, and anything
    /// else that is not a value of the declared type is refused.
    ///
    /// # Errors
    ///
    /// The reason, when the text is not a JSON array, the number of arguments
    /// is not the number of parameters, an entry is not one of this
    /// encoding's own shapes, or an entry is not a value of its parameter's
    /// type.
    pub fn arguments(&mut self, function: u32, json: &str) -> Result<Vec<Value>, String> {
        let parsed = hollowscript_vm::json::parse(json)
            .map_err(|e| format!("the arguments are not JSON: {}", e.message))?;
        let hollowscript_vm::json::Json::Array(items) = parsed else {
            return Err("the arguments must be a JSON array".to_owned());
        };
        let module = self.interpreter.module().module();
        let param_types = export_param_types(module, function);
        let types = module.types.clone();
        // Section 12.4, step 1: the count is checked before anything else.
        if items.len() != param_types.len() {
            return Err(format!(
                "expected {} argument(s), found {}",
                param_types.len(),
                items.len()
            ));
        }
        let mut out = Vec::with_capacity(items.len());
        for (index, (item, &ty)) in items.into_iter().zip(param_types.iter()).enumerate() {
            let item = coerce_json_numbers(item, ty, &types);
            let value = self
                .interpreter
                .with_host_call(&Limits::default(), |call| json_to_value(call, item))
                .map_err(|e| e.to_diagnostic().message)?;
            // Section 12.4, step 2: every argument is accepted by section
            // 9.6's rule, which also converts an `Int` for a `Float`
            // parameter, before the program sees it. The JSON decoder above
            // only builds a value of the shape the text has, so without
            // this a string for an `Int` parameter, a set for a list, or a
            // map of strings for a `Map<String, Int>` reached the program,
            // which then answered wrongly or faulted as an internal error. A
            // refusal is the host's mistake and nothing has run, so it is
            // reported as a refused call and does not fault the instance.
            let Some(accepted) = self.interpreter.accept_argument(value, ty) else {
                return Err(format!(
                    "argument {} is not a value of {}",
                    index + 1,
                    self.interpreter.type_name(ty)
                ));
            };
            out.push(accepted);
        }
        Ok(out)
    }

    // ---- the batched (suspend/resume) capability path --------------------
    //
    // A page whose host cannot answer a capability call synchronously (an
    // `await fetch(...)`, a `postMessage` round trip to a main thread that
    // is not cross-origin isolated and so has no `SharedArrayBuffer` to
    // block a Worker on) cannot use the synchronous path at all: nothing
    // here can make a WebAssembly import wait for a JavaScript `Promise`.
    // This is what `CommandBuffer` exists for instead: rather than calling
    // a capability function and waiting for its answer, the run queues a
    // `Command` and pauses (`Run::Suspended`); the host drains the queue,
    // performs the calls however long they take, and resumes the same run
    // with one `CommandResult` per `Command`, as many times as the program
    // makes more capability calls before it finishes or faults. No
    // `SharedArrayBuffer` is needed anywhere in this path, because nothing
    // here ever blocks: each slice between a suspend and the next resume is
    // an ordinary synchronous call this crate's caller (`abi::hs_wasm_load_batched`
    // and friends) makes from JavaScript's own event loop.

    /// Puts this instance on the batched path for the rest of its life:
    /// every capability call queues a [`Command`] and the run pauses,
    /// instead of calling a [`hollowscript_vm::HostFn`] and blocking for an
    /// answer. Call before the first run ([`WasmModule::initialise_batched`]).
    pub fn use_command_buffer(&mut self) {
        self.interpreter.use_command_buffer();
    }

    /// [`WasmModule::initialise`] on the batched path, which may answer
    /// [`Run::Suspended`] if a module's own top-level statements call a
    /// capability.
    ///
    /// # Errors
    ///
    /// As [`WasmModule::initialise`].
    pub fn initialise_batched(
        &mut self,
        limits: &Limits,
        control: &mut Control<'_>,
    ) -> Result<Run, Fault> {
        self.interpreter.initialise_batched(limits, control)
    }

    /// [`WasmModule::call_export`] on the batched path, which may answer
    /// [`Run::Suspended`].
    ///
    /// # Errors
    ///
    /// As [`WasmModule::call_export`].
    pub fn call_export_batched(
        &mut self,
        function: u32,
        args: &[Value],
        limits: &Limits,
        control: &mut Control<'_>,
    ) -> Result<Run, Fault> {
        self.interpreter
            .call_export_batched(function, args, limits, control)
    }

    /// The capability calls queued since the last drain, in order, removing
    /// them from the buffer: the host performs each in turn (in whatever
    /// time it needs, since nothing here is waiting synchronously) and
    /// gives one [`CommandResult`] back to [`WasmModule::resume`], in the
    /// same order.
    pub fn commands(&mut self) -> Vec<Command> {
        let drained = self.interpreter.commands();
        self.last_commands = drained.iter().map(|c| c.function).collect();
        drained
    }

    /// Continues a run [`Run::Suspended`] paused, with one [`CommandResult`]
    /// per [`Command`] [`WasmModule::commands`] drained, in the same order.
    /// May itself answer [`Run::Suspended`] again, if the program calls
    /// another capability before it finishes.
    ///
    /// # Errors
    ///
    /// As [`WasmModule::call_export`], and HS1012 if `results` is not the
    /// same length as the commands drained.
    pub fn resume(
        &mut self,
        results: Vec<CommandResult>,
        limits: &Limits,
        control: &mut Control<'_>,
    ) -> Result<Run, Fault> {
        self.interpreter.resume(results, limits, control)
    }

    /// A [`Value`] from JSON, in the encoding `value_to_json`'s own
    /// module documentation defines: what a [`CommandResult::Ok`] the host
    /// builds asynchronously is made of. The VM's own acceptance rule
    /// (section 9.6) checks it against the capability's declared type once
    /// it is used, so this only needs to build a value of roughly the
    /// right shape.
    ///
    /// # Errors
    ///
    /// A [`Fault`] if `json` is not one of this encoding's own shapes, or
    /// the heap is exhausted.
    pub fn value_of_json_scalar(
        &mut self,
        json: hollowscript_vm::json::Json,
    ) -> Result<Value, Fault> {
        self.interpreter
            .with_host_call(&Limits::default(), |call| json_to_value(call, json))
    }

    /// As [`WasmModule::value_of_json_scalar`]: the name is kept (a
    /// `Result`-typed answer's `{"Ok": ...}`/`{"Err": ...}` sugar is just
    /// one more shape `value_of_json_scalar`'s own `json_to_value`
    /// already understands, the same as every other tagged container, so
    /// building one needs no separate method any more) for a caller
    /// (`crate::abi`'s resume path) that already names it this way. `index`
    /// is this answer's own position within the batch
    /// [`WasmModule::commands`] last drained (the same order `resume`'s own
    /// contract requires the host's answers to keep): that command's own
    /// `cap_calls` index (retained here since the `Command` itself is gone
    /// by the time an answer comes back) names the capability whose
    /// declared result type directs a bare JSON number to `Int` or `Float`
    /// before decoding, exactly as [`WasmModule::arguments`] already does
    /// for `hs_call`'s own arguments (see `coerce_json_numbers`'s own
    /// documentation).
    ///
    /// # Errors
    ///
    /// As [`WasmModule::value_of_json_scalar`].
    pub fn value_of_json_result(
        &mut self,
        index: usize,
        json: hollowscript_vm::json::Json,
    ) -> Result<Value, Fault> {
        let function = self.last_commands.get(index).copied();
        let module = self.interpreter.module().module();
        let json = match function.and_then(|f| cap_call_result_type(module, f)) {
            Some(ty) => coerce_json_numbers(json, ty, &module.types),
            None => json,
        };
        self.value_of_json_scalar(json)
    }

    /// `value` (a [`Command`]'s own argument) as JSON, the same shape
    /// [`hs_wasm_call`](crate::abi::hs_wasm_call)'s own arguments already
    /// are, and in the same encoding `value_to_json`'s own module
    /// documentation defines: a queued command has no
    /// [`hollowscript_vm::HostCall`] of its own to read through, so this
    /// borrows the instance's heap and host types for the length of the
    /// call to build one. Bounded by `budget`, as [`WasmModule::render_json`]
    /// is, since an argument can share its sub-values in the same way.
    ///
    /// # Errors
    ///
    /// As [`WasmModule::render_json`].
    pub fn command_argument_json(
        &mut self,
        value: Value,
        budget: &mut JsonBudget<'_>,
    ) -> Result<String, Fault> {
        let mut out = String::new();
        self.interpreter
            .with_host_call_read(&Limits::default(), |call| {
                value_to_json(call, value, &mut out, budget)
            })?;
        Ok(out)
    }
}

// ---- the shared value <-> JSON encoding (chapter 12) ----------------------
//
// Every value that crosses this crate's own JS boundary, in either
// direction, uses this one encoding: `hs_wasm_call`'s arguments and result,
// a capability's own arguments and answer (`crate::abi::render_capability_arguments`
// and `crate::abi::json_result_to_value`), and a batched `Command`'s own
// argument (`WasmModule::command_argument_json`). It is exactly the encoding
// `hollowscript-capi`'s own module documentation defines and uses for the C
// ABI, so a value crosses either boundary the same way:
//
// | HollowScript | JSON |
// |---|---|
// | `Int`, `Float`, `Bool` | a JSON number or `true`/`false` (`Float` always keeps a decimal point) |
// | `String` | a JSON string |
// | `none` | `null` |
// | `[T]` | `{"list": [...]}` |
// | `Set<T>` | `{"set": [...]}` |
// | `Map<K, V>` | `{"map": [[k, v], ...]}` |
// | a record | `{"record": {"field": value, ...}}` |
// | an enum value | `{"enum": {"name": "E", "variant": "V", "payload": [...]}}` |
// | `Result<T, E>` | `{"Ok": value}` or `{"Err": value}` |
// | `Filter` | `{"filter": "<source text>"}`, outbound only |
//
// A record or enum value crosses only once its shape or variant is
// registered in `HostTypes` (see `host_types`, and `hollowscript::hosttypes`
// on the facade side of `hollowscript-capi`).

use hollowscript_bytecode::module::{CapabilityKind, ExportKind};
use hollowscript_core::transfer::{TransferType, TypeRef, TypeTable, VariantDef};
use hollowscript_vm::hostvalues::{ERR, OK, RESULT};

// ---- type-directed decoding (JSON -> Value) --------------------------------
//
// JSON cannot tell `3` from `3.0` (`Json::parse` reads a number with no `.`
// and no exponent as `Json::Int`, whichever the sender meant, and
// `JSON.stringify(3.0)` on a JS host is already just `"3"` by the time it
// gets here), so a bare number is decoded against the *declared* type of
// the position it is in, not against its own spelling: `coerce_json_numbers`
// walks the JSON alongside the declared `TransferType`, turning an `Int`
// that arrived at a `Float` position into a `Float`, before the ordinary
// (undirected) decode in the "shared value <-> JSON encoding" section below
// ever sees it. This is the encoding's own definition of which language
// value a JSON number denotes, given the declared type; it is not the
// conversion section 9.6 says never happens "inside a collection, a record
// or an enum payload", which is about a language *value* the boundary has
// already decided on, not about deciding what one JSON number means in the
// first place.

/// The declared parameter types of the exported function at `function` (the
/// index [`WasmModule::export_index`] gives), or an empty vector if none
/// matches (which the caller's own export lookup already refused with a
/// more specific message; this only needs to not panic).
fn export_param_types(
    module: &hollowscript_bytecode::module::Module,
    function: u32,
) -> Vec<TypeRef> {
    for export in &module.exports {
        if let ExportKind::Function {
            function: f,
            signature,
        } = &export.kind
        {
            if *f == function {
                return signature.params.clone();
            }
        }
    }
    Vec::new()
}

/// The declared result type of the capability call at `function` (the
/// `cap_calls` index a [`Command`] names as its own `function`), or `None`
/// if `function` is out of range or that capability answers nothing.
fn cap_call_result_type(
    module: &hollowscript_bytecode::module::Module,
    function: u32,
) -> Option<TypeRef> {
    let cap_call = module.cap_calls.get(function as usize)?;
    let capability = module.capabilities.get(cap_call.capability as usize)?;
    match &capability.kind {
        CapabilityKind::Function(signature) => signature.ret,
        CapabilityKind::Namespace(functions) => functions
            .get(cap_call.function as usize)
            .and_then(|(_, signature)| signature.ret),
    }
}

/// Recursively coerces a JSON number that arrived looking like an integer
/// to a JSON float wherever `expected` says the position is `Float`,
/// walking into a list, a map's own value, a record field, an enum's or
/// `Result`'s own payload and an optional's own inner type the same way.
/// Every other shape is left exactly as parsed, for the ordinary decode and
/// section 9.6's own acceptance rule to accept or refuse afterwards,
/// unchanged. See this section's own module documentation for why this
/// exists at all.
fn coerce_json_numbers(
    json: hollowscript_vm::json::Json,
    expected: TypeRef,
    types: &TypeTable,
) -> hollowscript_vm::json::Json {
    use hollowscript_vm::json::Json;
    let Some(entry) = types.get(expected) else {
        return json;
    };
    match entry {
        TransferType::Float => match json {
            Json::Int(n) => {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "the JSON boundary's own contract: a wire integer at a Float \
                              position becomes the nearest Float, exactly as a host's own \
                              `Number` already would"
                )]
                let f = n as f64;
                Json::Float(f)
            }
            other => other,
        },
        TransferType::List(element) => coerce_tagged_array(json, "list", *element, types),
        TransferType::Set(_) => json, // a Set's element is Int, String or Bool: never Float.
        TransferType::Map(_, value_ty) => coerce_map(json, *value_ty, types),
        TransferType::Optional(inner) => match json {
            Json::Null => Json::Null,
            other => coerce_json_numbers(other, *inner, types),
        },
        TransferType::Record(fields) => coerce_record(json, fields, types),
        TransferType::Enum { name, variants, .. } if name == RESULT => {
            coerce_result_sugar(json, variants, types)
        }
        TransferType::Enum { variants, .. } => coerce_enum(json, variants, types),
        TransferType::Int | TransferType::Bool | TransferType::String | TransferType::Filter => {
            json
        }
    }
}

/// `json`, with `{tag: [...]}`'s own array elements coerced against
/// `element`; any other shape (a mismatch the ordinary decode will refuse)
/// is left untouched.
fn coerce_tagged_array(
    json: hollowscript_vm::json::Json,
    tag: &str,
    element: TypeRef,
    types: &TypeTable,
) -> hollowscript_vm::json::Json {
    use hollowscript_vm::json::Json;
    let Json::Object(fields) = json else {
        return json;
    };
    let coerced = fields
        .into_iter()
        .map(|(key, value)| {
            if key == tag {
                if let Json::Array(items) = value {
                    let items = items
                        .into_iter()
                        .map(|item| coerce_json_numbers(item, element, types))
                        .collect();
                    (key, Json::Array(items))
                } else {
                    (key, value)
                }
            } else {
                (key, value)
            }
        })
        .collect();
    Json::Object(coerced)
}

/// `json`, with `{"map": [[k, v], ...]}`'s own `v`s coerced against
/// `value_ty` (a key is always `Int`, `String` or `Bool`, never `Float`).
fn coerce_map(
    json: hollowscript_vm::json::Json,
    value_ty: TypeRef,
    types: &TypeTable,
) -> hollowscript_vm::json::Json {
    use hollowscript_vm::json::Json;
    let Json::Object(fields) = json else {
        return json;
    };
    let coerced = fields
        .into_iter()
        .map(|(key, value)| {
            if key != "map" {
                return (key, value);
            }
            let Json::Array(entries) = value else {
                return (key, value);
            };
            let entries = entries
                .into_iter()
                .map(|entry| {
                    let Json::Array(pair) = entry else {
                        return entry;
                    };
                    let mut pair = pair;
                    if let Some(v) = pair.get_mut(1) {
                        *v = coerce_json_numbers(std::mem::replace(v, Json::Null), value_ty, types);
                    }
                    Json::Array(pair)
                })
                .collect();
            (key, Json::Array(entries))
        })
        .collect();
    Json::Object(coerced)
}

/// `json`, with `{"record": {"field": value, ...}}`'s own field values
/// coerced against `fields`' own declared field types, matched by name.
fn coerce_record(
    json: hollowscript_vm::json::Json,
    fields: &[(String, TypeRef)],
    types: &TypeTable,
) -> hollowscript_vm::json::Json {
    use hollowscript_vm::json::Json;
    let Json::Object(tagged) = json else {
        return json;
    };
    let coerced = tagged
        .into_iter()
        .map(|(key, value)| {
            if key != "record" {
                return (key, value);
            }
            let Json::Object(record_fields) = value else {
                return (key, value);
            };
            let record_fields = record_fields
                .into_iter()
                .map(|(field_name, field_value)| {
                    let ty = fields
                        .iter()
                        .find(|(name, _)| *name == field_name)
                        .map(|(_, ty)| *ty);
                    match ty {
                        Some(ty) => (field_name, coerce_json_numbers(field_value, ty, types)),
                        None => (field_name, field_value),
                    }
                })
                .collect();
            (key, Json::Object(record_fields))
        })
        .collect();
    Json::Object(coerced)
}

/// `json`, with `{"enum": {"name", "variant", "payload": [...]}}`'s own
/// payload items coerced against the matching variant's own declared
/// payload types, by position.
fn coerce_enum(
    json: hollowscript_vm::json::Json,
    variants: &[VariantDef],
    types: &TypeTable,
) -> hollowscript_vm::json::Json {
    use hollowscript_vm::json::Json;
    let Json::Object(tagged) = json else {
        return json;
    };
    let coerced = tagged
        .into_iter()
        .map(|(key, value)| {
            if key != "enum" {
                return (key, value);
            }
            let Json::Object(enum_fields) = value else {
                return (key, value);
            };
            let variant_name =
                enum_fields
                    .iter()
                    .find(|(k, _)| k == "variant")
                    .and_then(|(_, v)| match v {
                        Json::String(s) => Some(s.clone()),
                        _ => None,
                    });
            let payload_types = variant_name
                .and_then(|name| variants.iter().find(|v| v.name == name))
                .map(|v| v.payload.clone());
            let enum_fields = enum_fields
                .into_iter()
                .map(|(k, v)| {
                    if k != "payload" {
                        return (k, v);
                    }
                    let Json::Array(items) = v else {
                        return (k, v);
                    };
                    let items = match &payload_types {
                        Some(payload_types) => items
                            .into_iter()
                            .enumerate()
                            .map(|(i, item)| match payload_types.get(i) {
                                Some(&ty) => coerce_json_numbers(item, ty, types),
                                None => item,
                            })
                            .collect(),
                        None => items,
                    };
                    (k, Json::Array(items))
                })
                .collect();
            (key, Json::Object(enum_fields))
        })
        .collect();
    Json::Object(coerced)
}

/// `json`, as [`coerce_result_sugar`] handles `Result`'s own `{"Ok": v}`/
/// `{"Err": v}` sugar (not a `{"enum": {...}}`-tagged value at all): `v`
/// coerced against the matching variant's one payload type.
fn coerce_result_sugar(
    json: hollowscript_vm::json::Json,
    variants: &[VariantDef],
    types: &TypeTable,
) -> hollowscript_vm::json::Json {
    use hollowscript_vm::json::Json;
    let Json::Object(fields) = json else {
        return json;
    };
    let coerced = fields
        .into_iter()
        .map(|(key, value)| {
            let variant_name = if key == OK {
                Some(OK)
            } else if key == ERR {
                Some(ERR)
            } else {
                None
            };
            let payload_ty = variant_name
                .and_then(|name| variants.iter().find(|v| v.name == name))
                .and_then(|v| v.payload.first().copied());
            match payload_ty {
                Some(ty) => (key, coerce_json_numbers(value, ty, types)),
                None => (key, value),
            }
        })
        .collect();
    Json::Object(coerced)
}

/// One step of [`value_to_json`]'s walk: a value still to be written, a
/// literal to append, or the rest of a container's children. A container's
/// children are not turned into frames all at once: a frame holds the slice
/// and the position of the next child, so a list of 600,000 pairs costs one
/// frame at a time on the stack and not a million, which is what keeps the
/// writer's own memory a small fraction of the text it writes.
enum JsonFrame<'a> {
    /// A value still to be written.
    Value(Value),
    /// Text to append as it is.
    Literal(&'static str),
    /// The children of a list or an enum payload from `next` on, then `close`.
    Items {
        items: &'a [Value],
        next: usize,
        close: &'static str,
    },
    /// The entries of a map from `next` on, then the closing `]}`.
    Entries {
        entries: &'a [hollowscript_vm::MapEntry],
        next: usize,
    },
    /// The fields of a record from `next` on, then the closing `}}`.
    Fields {
        fields: Vec<(&'a str, Value)>,
        next: usize,
    },
}

/// What bounds one rendering of a value to JSON (or several that share it).
///
/// A value can have vastly more paths than objects: `t = Tree.Node(t, t)`
/// repeated fifty times is fifty objects and 2^50 paths, and this encoding
/// writes a sub-value once for every path to it, because JSON has no way to
/// name a value once and refer back to it. Section 11.4 says that a value's
/// size is counted along every path, so such a value is astronomically large
/// by the language's own measure, and writing it out unbounded would allocate
/// until the host's memory ran out, with no step charge, no memory charge and
/// no safepoint to stop it.
///
/// A budget makes the walk stop instead. Every value and every piece of
/// punctuation the walk writes is at least one byte, so a bound on the bytes
/// produced is a bound on the work done, however much of the structure is
/// shared. The bytes are charged against `limit`, which a host sets as the
/// output limit ([section 11.6](../../docs/spec/11-resource-limits.md)):
/// when the text passes it, the walk stops with HS1004, within one piece of
/// the limit (a piece is punctuation, a number, or a string that is checked
/// against the limit before it is escaped), and what it wrote is discarded. The walk also asks `poll` whether to
/// stop, every 1,024 steps of its own work, and stops with HS1006 when it
/// answers `true`, so a cancellation or a deadline reaches a rendering as it
/// reaches a run.
///
/// `limit` counts the bytes of every rendering that shares one budget, so a
/// list of values rendered one after another is bounded as a whole.
pub struct JsonBudget<'a> {
    limit: u64,
    spent: u64,
    poll: &'a mut dyn FnMut() -> bool,
    frames: u32,
}

impl std::fmt::Debug for JsonBudget<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsonBudget")
            .field("limit", &self.limit)
            .field("spent", &self.spent)
            .finish_non_exhaustive()
    }
}

/// How many pieces of a rendering are written between two asks of the
/// budget's `poll`: the same 1,024 that section 11.3 allows between two
/// safepoints.
const POLL_EVERY: u32 = 1_024;

impl<'a> JsonBudget<'a> {
    /// A budget of `limit` bytes. `poll` answers `true` to stop the walk; it
    /// is asked about every 1,024 steps of the walk's own work.
    pub fn new(limit: u64, poll: &'a mut dyn FnMut() -> bool) -> JsonBudget<'a> {
        JsonBudget {
            limit,
            spent: 0,
            poll,
            frames: 0,
        }
    }

    /// The bytes that renderings sharing this budget have produced.
    #[must_use]
    pub fn spent(&self) -> u64 {
        self.spent
    }

    /// Whether `produced` more bytes (the current rendering's own length so
    /// far, plus `extra` about to be written) would pass the limit.
    fn exceeded(&self, produced: usize, extra: usize) -> bool {
        let total = self
            .spent
            .saturating_add(produced as u64)
            .saturating_add(extra as u64);
        total > self.limit
    }

    /// The fault for a rendering that has produced `produced` bytes and
    /// would pass the limit with its next piece.
    fn over(&self, produced: usize) -> Fault {
        Fault::output_limit(
            self.limit,
            self.spent.saturating_add(produced as u64).min(self.limit),
        )
    }

    /// Counts one piece of work, asking `poll` once in every [`POLL_EVERY`].
    fn tick(&mut self) -> Result<(), Fault> {
        self.frames = self.frames.wrapping_add(1);
        if self.frames % POLL_EVERY == 0 && (self.poll)() {
            return Err(Fault::cancelled());
        }
        Ok(())
    }
}

/// Appends `value` to `out` as JSON, in the encoding this section documents,
/// within `budget`.
///
/// An `Int` whose magnitude is above 2^53 - 1 is written as the tagged
/// string `{"bigint": "<decimal>"}` ([`write_int`]), so a reader that holds a
/// JSON number as a double never reads back a different `Int`: this is the
/// one form, for results, capability arguments and queued commands alike.
///
/// **Iterative, not recursive.** `value` can be a capability
/// argument a program built, and a program can nest a list, a record or an
/// enum payload inside itself as deep as the heap allows (`Tree.Node(Tree.
/// Node(...))`, one heap object per level, or a bytecode argument register
/// patched to name a deep local the verifier never checked against the
/// capability's own declared parameter type, which is how the review
/// reached this): recursing here once per level would put that depth on
/// the host's own call stack. The walk instead keeps its own explicit
/// stack of pending [`JsonFrame`]s, the same discipline `accept.rs`'s
/// acceptance walk and `render.rs`'s own rendering walk already use for
/// the same reason.
///
/// # Errors
///
/// HS1004 when the text would pass the budget, HS1006 when the budget's
/// poll says to stop. `out` holds what was written until then.
pub(crate) fn value_to_json(
    call: &hollowscript_vm::HostCall<'_>,
    value: Value,
    out: &mut String,
    budget: &mut JsonBudget<'_>,
) -> Result<(), Fault> {
    let mut stack = vec![JsonFrame::Value(value)];
    while let Some(frame) = stack.pop() {
        budget.tick()?;
        match frame {
            JsonFrame::Literal(text) => out.push_str(text),
            JsonFrame::Value(value) => write_value_frame(call, value, out, &mut stack, budget)?,
            JsonFrame::Items { items, next, close } => match items.get(next) {
                None => out.push_str(close),
                Some(&item) => {
                    if next > 0 {
                        out.push(',');
                    }
                    stack.push(JsonFrame::Items {
                        items,
                        next: next + 1,
                        close,
                    });
                    stack.push(JsonFrame::Value(item));
                }
            },
            JsonFrame::Entries { entries, next } => match entries.get(next) {
                None => out.push_str("]}"),
                Some(entry) => {
                    if next > 0 {
                        out.push(',');
                    }
                    out.push('[');
                    map_key_to_json(call, entry.key, out, 0, budget)?;
                    out.push(',');
                    stack.push(JsonFrame::Entries {
                        entries,
                        next: next + 1,
                    });
                    stack.push(JsonFrame::Literal("]"));
                    stack.push(JsonFrame::Value(entry.value));
                }
            },
            JsonFrame::Fields { fields, next } => match fields.get(next).copied() {
                None => out.push_str("}}"),
                Some((name, field)) => {
                    if next > 0 {
                        out.push(',');
                    }
                    out.push('"');
                    escape_json_string(name, out);
                    out.push_str("\":");
                    stack.push(JsonFrame::Fields {
                        fields,
                        next: next + 1,
                    });
                    stack.push(JsonFrame::Value(field));
                }
            },
        }
        if budget.exceeded(out.len(), 0) {
            return Err(budget.over(out.len()));
        }
    }
    budget.spent = budget.spent.saturating_add(out.len() as u64);
    Ok(())
}

/// The largest magnitude at which every integer is exactly a double
/// (JavaScript's `Number.MAX_SAFE_INTEGER`).
const MAX_SAFE_INT: u64 = (1 << 53) - 1;

/// Appends `n` to `out`: a JSON number when the `Int` is safe (its magnitude
/// is at most 2^53 - 1), and otherwise the tagged string `{"bigint":
/// "<decimal>"}`, so that no `Int` is ever read back as a different one.
/// Outbound only: this ABI never reads the tag.
pub(crate) fn write_int(out: &mut String, n: i64) {
    use std::fmt::Write as _;
    if n.unsigned_abs() > MAX_SAFE_INT {
        let _ = write!(out, r#"{{"bigint":"{n}"}}"#);
    } else {
        let _ = write!(out, "{n}");
    }
}

/// Writes the leaf text of one value directly to `out`, or, for a value
/// with children, writes its opening punctuation and pushes its children
/// (and their own closing punctuation) onto `stack` for a later iteration
/// of [`value_to_json`]'s loop to expand: never calls itself, and never
/// calls [`value_to_json`].
fn write_value_frame<'a>(
    call: &'a hollowscript_vm::HostCall<'_>,
    value: Value,
    out: &mut String,
    stack: &mut Vec<JsonFrame<'a>>,
    budget: &JsonBudget<'_>,
) -> Result<(), Fault> {
    use std::fmt::Write as _;
    match value {
        Value::Int(n) => write_int(out, n),
        // `{f:?}` always writes a decimal point or an exponent (`3.0` and
        // `1e21`, never `3`), and an `Int` never has either, so on the wire a
        // `Float` and an `Int` are different. A JavaScript `JSON.parse` reads
        // both as a `Number`, though, so a host that reads the JSON result
        // with it cannot tell `3.0` from `3` (see `web/README.md`).
        Value::Float(f) => {
            let _ = write!(out, "{f:?}");
        }
        Value::Bool(b) => out.push_str(if b { "true" } else { "false" }),
        Value::None => out.push_str("null"),
        Value::Heap(_) => {
            if let Some(text) = call.str_of(value) {
                // The text is checked before it is escaped, not after: one
                // string can be as long as the memory limit allows, and
                // escaping can write six bytes for each of its own.
                if budget.exceeded(out.len(), text.len().saturating_add(2)) {
                    return Err(budget.over(out.len()));
                }
                out.push('"');
                escape_json_string(text, out);
                out.push('"');
            } else if let Some(items) = call.list_of(value) {
                out.push_str(r#"{"list":["#);
                stack.push(JsonFrame::Items {
                    items,
                    next: 0,
                    close: "]}",
                });
            } else if let Some(elements) = call.set_of(value) {
                out.push_str(r#"{"set":["#);
                // A set's elements are keys, not values (`Int`, `String`
                // or `Bool`, never a container), so `map_key_to_json`
                // cannot recurse into another value: safe to write
                // straight to `out`, the same as it always has been.
                for (i, &key) in elements.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    if budget.exceeded(out.len(), 0) {
                        return Err(budget.over(out.len()));
                    }
                    map_key_to_json(call, key, out, 0, budget)?;
                }
                out.push_str("]}");
            } else if let Some(entries) = call.map_of(value) {
                out.push_str(r#"{"map":["#);
                stack.push(JsonFrame::Entries { entries, next: 0 });
            } else if let Some(fields) = call.record_of(value) {
                out.push_str(r#"{"record":{"#);
                stack.push(JsonFrame::Fields { fields, next: 0 });
            } else if let Some((enum_name, variant_name, payload)) = call.enum_of(value) {
                out.push_str(r#"{"enum":{"name":""#);
                escape_json_string(enum_name, out);
                out.push_str(r#"","variant":""#);
                escape_json_string(variant_name, out);
                out.push_str(r#"","payload":["#);
                stack.push(JsonFrame::Items {
                    items: payload,
                    next: 0,
                    close: "]}}",
                });
            } else if let Some(source) = call.filter_of(value) {
                out.push_str(r#"{"filter":""#);
                escape_json_string(source, out);
                out.push_str("\"}");
            } else {
                out.push_str("null");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod value_to_json_tests {
    use super::{JsonBudget, value_to_json};
    use hollowscript_vm::{HostCall, HostTypes, MapEntry, MapKey, Object, Value};

    /// A deeply nested value (a list holding a list holding a
    /// list, as deep as the heap allows, which is exactly what a
    /// program's own recursive enum or a patched capability-argument
    /// register can hand a capability call) must not overflow the host's
    /// stack converting it to JSON. Before the fix, `value_to_json`
    /// recursed once per level and crashed well under this depth on a
    /// normal thread stack; the review found it reachable in a real
    /// embedding through a JS `print` handler calling back into its own
    /// instance.
    #[test]
    fn a_deeply_nested_capability_argument_converts_to_json_without_exhausting_the_stack() {
        let mut heap = hollowscript_vm::Heap::new();
        let mut value = heap.alloc(Object::List(vec![])).unwrap();
        for _ in 0..200_000 {
            value = heap.alloc(Object::List(vec![value])).unwrap();
        }
        let types = HostTypes::new();
        let mut counters = hollowscript_vm::RunCounters::new();
        let limits = hollowscript_core::Limits::default();
        let call = HostCall::new(&mut heap, &types, &mut counters, &limits, None);
        let mut out = String::new();
        let mut never = || false;
        let mut budget = JsonBudget::new(u64::MAX, &mut never);
        value_to_json(&call, value, &mut out, &mut budget).unwrap();
        // Every level of nesting writes `{"list":[` and closes with `]}`
        // (11 bytes), around the innermost empty list's `{"list":[]}`.
        assert_eq!(out.len(), 200_000 * 11 + r#"{"list":[]}"#.len());
    }

    /// A value built by repeated doubling (`t = [t, t]`, fifty times: fifty
    /// objects and 2^50 paths) is refused with the output limit once its text
    /// would pass it, having stopped within one piece of the limit, instead of
    /// being written out until memory runs out.
    #[test]
    fn a_shared_structure_value_is_bounded_by_the_budget() {
        let mut heap = hollowscript_vm::Heap::new();
        let mut value = heap.alloc(Object::List(vec![])).unwrap();
        for _ in 0..50 {
            value = heap.alloc(Object::List(vec![value, value])).unwrap();
        }
        let types = HostTypes::new();
        let mut counters = hollowscript_vm::RunCounters::new();
        let limits = hollowscript_core::Limits::default();
        let call = HostCall::new(&mut heap, &types, &mut counters, &limits, None);
        let mut never = || false;
        let mut budget = JsonBudget::new(1_000_000, &mut never);
        let mut out = String::new();
        let started = std::time::Instant::now();
        let fault = value_to_json(&call, value, &mut out, &mut budget).unwrap_err();
        let diagnostic = fault.to_diagnostic();
        assert_eq!(diagnostic.code.to_text(), "HS1004");
        let data = diagnostic.data.unwrap();
        assert_eq!(data.limit, 1_000_000);
        assert!(data.used <= 1_000_000);
        // The walk stops within one piece of the limit (a piece is small:
        // punctuation, a number, or a string that was checked first).
        assert!(out.len() <= 1_000_000 + 64, "{}", out.len());
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "took {:?}",
            started.elapsed()
        );
    }

    /// The budget is the bytes of everything written with it, a long string
    /// is refused before it is escaped, and a rendering that fits is the same
    /// as it was without a budget.
    #[test]
    fn the_budget_is_shared_and_a_fitting_value_is_untouched() {
        let mut heap = hollowscript_vm::Heap::new();
        let long = heap.alloc(Object::Str("x".repeat(600))).unwrap();
        let list = heap
            .alloc(Object::List(vec![Value::Int(1), Value::Int(2)]))
            .unwrap();
        let types = HostTypes::new();
        let mut counters = hollowscript_vm::RunCounters::new();
        let limits = hollowscript_core::Limits::default();
        let call = HostCall::new(&mut heap, &types, &mut counters, &limits, None);
        let mut never = || false;

        let mut budget = JsonBudget::new(1_000, &mut never);
        let mut out = String::new();
        value_to_json(&call, list, &mut out, &mut budget).unwrap();
        assert_eq!(out, r#"{"list":[1,2]}"#);
        assert_eq!(budget.spent(), out.len() as u64);

        let mut budget = JsonBudget::new(1_000, &mut never);
        let mut first = String::new();
        value_to_json(&call, long, &mut first, &mut budget).unwrap();
        assert_eq!(first.len(), 602);
        let mut second = String::new();
        assert!(value_to_json(&call, long, &mut second, &mut budget).is_err());
        assert!(second.is_empty(), "a string over the budget is not written");
    }

    /// An `Int` above 2^53 - 1 is the tagged exact form wherever a value is
    /// written, a list element and a map key included, so two distinct 64-bit
    /// `Int`s never collide for the host.
    #[test]
    fn large_ints_are_written_exactly() {
        let mut heap = hollowscript_vm::Heap::new();
        let list = heap
            .alloc(Object::List(vec![
                Value::Int(9_007_199_254_740_993),
                Value::Int(i64::MAX),
                Value::Int(9_007_199_254_740_991),
                Value::Int(-9_007_199_254_740_993),
            ]))
            .unwrap();
        let types = HostTypes::new();
        let mut counters = hollowscript_vm::RunCounters::new();
        let limits = hollowscript_core::Limits::default();
        let call = HostCall::new(&mut heap, &types, &mut counters, &limits, None);
        let mut never = || false;
        let mut budget = JsonBudget::new(u64::MAX, &mut never);
        let mut out = String::new();
        value_to_json(&call, list, &mut out, &mut budget).unwrap();
        assert_eq!(
            out,
            concat!(
                r#"{"list":[{"bigint":"9007199254740993"},{"bigint":"9223372036854775807"},"#,
                r#"9007199254740991,{"bigint":"-9007199254740993"}]}"#
            )
        );
    }

    /// The budget asks its poll, and stops with the cancelled fault when it
    /// says to, well before a value this large could be written.
    #[test]
    fn the_budget_polls_for_cancellation() {
        let mut heap = hollowscript_vm::Heap::new();
        let mut value = heap.alloc(Object::List(vec![])).unwrap();
        for _ in 0..50 {
            value = heap.alloc(Object::List(vec![value, value])).unwrap();
        }
        let types = HostTypes::new();
        let mut counters = hollowscript_vm::RunCounters::new();
        let limits = hollowscript_core::Limits::default();
        let call = HostCall::new(&mut heap, &types, &mut counters, &limits, None);
        let mut polls = 0u32;
        let mut stop_after_three = || {
            polls += 1;
            polls > 3
        };
        let mut budget = JsonBudget::new(u64::MAX, &mut stop_after_three);
        let mut out = String::new();
        let fault = value_to_json(&call, value, &mut out, &mut budget).unwrap_err();
        assert_eq!(fault.to_diagnostic().code.to_text(), "HS1006");
        assert!(out.len() < 1_000_000, "{}", out.len());
    }

    /// The comma placement the iterative rewrite above computes by hand
    /// (one between each pair of a container's children, none before the
    /// first or after the last) checked directly against a multi-item
    /// list, a multi-entry map and a multi-field record, since the deep
    /// test above only ever nests one child per level and so never
    /// exercises more than zero commas.
    #[test]
    fn multi_child_containers_place_commas_correctly() {
        let mut heap = hollowscript_vm::Heap::new();
        let a = heap.alloc(Object::Str("a".to_owned())).unwrap();
        let one = Value::Int(1);
        let two = Value::Int(2);
        let three = Value::Int(3);
        let list = heap.alloc(Object::List(vec![one, two, three])).unwrap();
        let map = heap
            .alloc(Object::Map(vec![
                MapEntry {
                    key: MapKey::Int(1),
                    value: one,
                },
                MapEntry {
                    key: MapKey::Int(2),
                    value: two,
                },
            ]))
            .unwrap();
        let record = heap
            .alloc(Object::Record {
                shape: vec![
                    hollowscript_bytecode::module::StringId(100),
                    hollowscript_bytecode::module::StringId(101),
                ],
                fields: vec![one, a],
            })
            .unwrap();

        let mut types = HostTypes::new();
        types.add_shape(vec!["p".to_owned(), "q".to_owned()], 0, vec![100, 101]);
        let mut counters = hollowscript_vm::RunCounters::new();
        let limits = hollowscript_core::Limits::default();
        let call = HostCall::new(&mut heap, &types, &mut counters, &limits, None);

        let mut never = || false;
        let mut budget = JsonBudget::new(u64::MAX, &mut never);
        let mut list_json = String::new();
        value_to_json(&call, list, &mut list_json, &mut budget).unwrap();
        assert_eq!(list_json, r#"{"list":[1,2,3]}"#);

        let mut map_json = String::new();
        value_to_json(&call, map, &mut map_json, &mut budget).unwrap();
        assert_eq!(map_json, r#"{"map":[[1,1],[2,2]]}"#);

        let mut record_json = String::new();
        value_to_json(&call, record, &mut record_json, &mut budget).unwrap();
        assert_eq!(record_json, r#"{"record":{"p":1,"q":"a"}}"#);
    }
}

/// Appends `key` to `out` as `value_to_json` would render the equivalent
/// `Value` (a map key is always `Int`, `String` or `Bool`). `already` is the
/// number of bytes of the same rendering written elsewhere (`out` itself may
/// be a scratch string that is appended to the rendering later), so that a
/// long key is refused against the budget before it is escaped.
///
/// # Errors
///
/// HS1004 when the key would pass `budget`.
fn map_key_to_json(
    call: &hollowscript_vm::HostCall<'_>,
    key: hollowscript_vm::MapKey,
    out: &mut String,
    already: usize,
    budget: &JsonBudget<'_>,
) -> Result<(), Fault> {
    match key {
        hollowscript_vm::MapKey::Int(n) => write_int(out, n),
        hollowscript_vm::MapKey::Bool(b) => out.push_str(if b { "true" } else { "false" }),
        hollowscript_vm::MapKey::Str(_) => {
            let text = call.key_str_of(key).unwrap_or("");
            let used = already.saturating_add(out.len());
            if budget.exceeded(used, text.len().saturating_add(2)) {
                return Err(budget.over(used));
            }
            out.push('"');
            escape_json_string(text, out);
            out.push('"');
        }
    }
    Ok(())
}

/// `json`, as a [`Value`], in the encoding this section documents.
///
/// # Errors
///
/// A [`Fault`] built exactly once, never a message that has already been
/// rendered through [`Fault::to_diagnostic`] and wrapped a second time (that
/// would double the "Internal error: " a template like [`Fault::internal`]'s
/// own adds); a caller renders the `Fault` to text once, at the boundary
/// where it is reported.
pub(crate) fn json_to_value(
    call: &mut hollowscript_vm::HostCall<'_>,
    json: hollowscript_vm::json::Json,
) -> Result<Value, Fault> {
    use hollowscript_vm::json::Json;
    match json {
        Json::Null => Ok(Value::None),
        Json::Bool(b) => Ok(Value::Bool(b)),
        Json::Int(n) => Ok(Value::Int(n)),
        Json::Float(f) => Ok(Value::Float(f)),
        Json::String(text) => call.string(text),
        Json::Array(_) => Err(Fault::internal(
            "a bare array is not a value here; use {\"list\": [...]} or {\"set\": [...]}",
        )),
        Json::Object(fields) => json_tagged_to_value(call, fields),
    }
}

fn json_tagged_to_value(
    call: &mut hollowscript_vm::HostCall<'_>,
    fields: Vec<(String, hollowscript_vm::json::Json)>,
) -> Result<Value, Fault> {
    use hollowscript_vm::json::Json;
    let [(tag, body)] = <[(String, Json); 1]>::try_from(fields).map_err(|_| {
        Fault::internal(
            "a value that is a JSON object must have exactly one field, naming its kind \
             (list, set, map, record, enum, Ok or Err)",
        )
    })?;
    match tag.as_str() {
        "list" => {
            let Json::Array(items) = body else {
                return Err(Fault::internal("\"list\" needs an array"));
            };
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                values.push(json_to_value(call, item)?);
            }
            call.list(values)
        }
        "set" => {
            let Json::Array(items) = body else {
                return Err(Fault::internal("\"set\" needs an array"));
            };
            let mut keys = Vec::with_capacity(items.len());
            for item in items {
                let value = json_to_value(call, item)?;
                keys.push(
                    value_to_map_key(value).ok_or_else(|| {
                        Fault::internal("a set element must be Int, String or Bool")
                    })?,
                );
            }
            call.set(keys)
        }
        "map" => {
            let Json::Array(items) = body else {
                return Err(Fault::internal(
                    "\"map\" needs an array of [key, value] pairs",
                ));
            };
            let mut entries = Vec::with_capacity(items.len());
            for item in items {
                let Json::Array(pair) = item else {
                    return Err(Fault::internal(
                        "each map entry must be a [key, value] pair",
                    ));
                };
                let [key_json, value_json] = <[Json; 2]>::try_from(pair).map_err(|_| {
                    Fault::internal("each map entry must have exactly two elements")
                })?;
                let key_value = json_to_value(call, key_json)?;
                let key = value_to_map_key(key_value)
                    .ok_or_else(|| Fault::internal("a map key must be Int, String or Bool"))?;
                let value = json_to_value(call, value_json)?;
                entries.push(hollowscript_vm::MapEntry { key, value });
            }
            call.map(entries)
        }
        "record" => {
            let Json::Object(field_entries) = body else {
                return Err(Fault::internal("\"record\" needs an object"));
            };
            let mut owned: Vec<(String, Value)> = Vec::with_capacity(field_entries.len());
            for (name, value_json) in field_entries {
                owned.push((name, json_to_value(call, value_json)?));
            }
            let fields: Vec<(&str, Value)> =
                owned.iter().map(|(name, v)| (name.as_str(), *v)).collect();
            call.record(fields)
        }
        "enum" => {
            let Json::Object(enum_fields) = body else {
                return Err(Fault::internal("\"enum\" needs an object"));
            };
            let field = |key: &str| enum_fields.iter().find(|(k, _)| k == key).map(|(_, v)| v);
            let Some(Json::String(name)) = field("name") else {
                return Err(Fault::internal("\"enum\" needs a string \"name\""));
            };
            let Some(Json::String(variant)) = field("variant") else {
                return Err(Fault::internal("\"enum\" needs a string \"variant\""));
            };
            let payload_json = match field("payload") {
                None => Vec::new(),
                Some(Json::Array(items)) => items.clone(),
                Some(_) => return Err(Fault::internal("\"payload\" must be an array")),
            };
            let mut payload = Vec::with_capacity(payload_json.len());
            for item in payload_json {
                payload.push(json_to_value(call, item)?);
            }
            call.enum_value(name, variant, payload)
        }
        _ if tag == OK => {
            let inner = json_to_value(call, body)?;
            call.ok(inner)
        }
        _ if tag == ERR => {
            let inner = json_to_value(call, body)?;
            call.err(inner)
        }
        other => Err(Fault::internal(&format!(
            "'{other}' is not a value kind this ABI understands \
             (list, set, map, record, enum, Ok, Err)"
        ))),
    }
}

/// `value`, as a map/set key, or `None` if it is not `Int`, `String` or
/// `Bool` (section 2.8's key types).
fn value_to_map_key(value: Value) -> Option<hollowscript_vm::MapKey> {
    match value {
        Value::Bool(b) => Some(hollowscript_vm::MapKey::Bool(b)),
        Value::Int(n) => Some(hollowscript_vm::MapKey::Int(n)),
        Value::Heap(h) => Some(hollowscript_vm::MapKey::Str(h)),
        Value::None | Value::Float(_) => None,
    }
}

/// Appends the JSON string-literal encoding of `text` (without the
/// surrounding quotes) to `out`.
pub(crate) fn escape_json_string(text: &str, out: &mut String) {
    use std::fmt::Write as _;
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
}

pub mod abi;

#[cfg(feature = "tooling")]
pub mod abi_ide;
#[cfg(feature = "tooling")]
pub mod abi_tooling;
#[cfg(feature = "tooling")]
pub mod tooling;

#[cfg(test)]
mod tests {
    use super::WasmModule;
    use hollowscript_bytecode::module::{Function, FunctionKind, Module, ModuleEntry};
    use hollowscript_bytecode::opcode::{Instruction, Op};
    use hollowscript_vm::CapabilityTable;

    fn sample_bytes() -> Vec<u8> {
        let mut m = Module::new();
        let path = m.add_string("main.hws").unwrap();
        let name = m.add_string("main").unwrap();
        m.functions.push(Function {
            name,
            kind: FunctionKind::ModuleInit,
            arity: 0,
            register_count: 1,
            returns_value: false,
            captures: vec![],
            constants: vec![],
            code: vec![Instruction::abc(Op::ReturnNone, 0, 0, 0).encode()],
            spans: vec![],
        });
        m.modules.push(ModuleEntry { path, init: 0 });
        hollowscript_bytecode::encode(&m)
    }

    #[test]
    fn a_verified_module_loads_and_its_globals_start_absent() {
        let module = WasmModule::load(&sample_bytes(), CapabilityTable::new(vec![])).unwrap();
        assert!(
            module.global(0).is_none(),
            "the sample module declares no globals"
        );
    }

    #[test]
    fn unverifiable_bytes_are_refused_before_anything_runs() {
        assert!(WasmModule::load(b"not bytecode", CapabilityTable::new(vec![])).is_err());
    }

    #[test]
    fn initialise_runs_a_trivial_module_to_completion() {
        let mut module = WasmModule::load(&sample_bytes(), CapabilityTable::new(vec![])).unwrap();
        let result = module
            .initialise(
                &hollowscript_core::Limits::default(),
                &mut hollowscript_core::Control::none(),
            )
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn capability_names_reads_the_compiled_calls_positionally() {
        // The sample module declares no capabilities, so the list is empty;
        // the interesting cases (a plain function, a namespace member) are
        // covered by `abi`'s own tests, which build a module that has some.
        let names = super::capability_names(&sample_bytes()).unwrap();
        assert!(names.is_empty());
    }
}

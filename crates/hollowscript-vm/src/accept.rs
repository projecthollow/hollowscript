//! What a host is allowed to return from a capability function (section 9.6).
//!
//! A capability's declared result type is part of the program's static
//! types: the checker types `fs.read(path)` as `Result<String, IoError>`
//! and every later expression is checked against that. The host, though, is
//! ordinary Rust or C, outside the type system, and hands back a
//! [`Value`] that nothing has yet compared against the declaration. Without
//! this check a host that returns an `Int` where it declared a `String` puts
//! a value of the wrong type into a register, and the program goes on using
//! it as a `String`: a host defect turns into type confusion inside the
//! script, which is exactly what the language promises cannot happen.
//!
//! Section 9.6 fixes the rule. A value is accepted if it is a language
//! value of the declared type, or an `Int` where the declaration is `Float`
//! or `Float?`, "in which case it is converted; that conversion is made at
//! the top level of the result only, and nothing is converted inside a
//! collection, a record or an enum payload". Anything else is HS1013.
//!
//! The walk is iterative, over an explicit stack of (value, type) pairs,
//! because a host can build a list of lists as deep as the heap allows and
//! recursion here would put that depth on the host's own stack.

use crate::interp::Interpreter;
use crate::value::{MapKey, Object, Value};
use hollowscript_bytecode::module::CapabilityKind;
use hollowscript_core::fault::{Fault, FaultCode};
use hollowscript_core::transfer::{TransferType, TypeRef};

/// How deep [`Interpreter::describe_type`] will go before it stops naming
/// the type it is inside. Only a message, never a decision.
const DESCRIBE_DEPTH: usize = 8;

impl Interpreter {
    /// The declared result type of the capability function at `index` in the
    /// module's `cap_calls`, or `None` for one that returns nothing.
    ///
    /// # Errors
    ///
    /// HS1099 if any of the three indices is out of range, which is a defect
    /// of the module rather than a property of any script.
    pub(crate) fn cap_call_result(&self, index: u32) -> Result<Option<TypeRef>, Fault> {
        let module = self.module().module();
        let call = module
            .cap_calls
            .get(index as usize)
            .ok_or_else(|| Fault::internal("a capability call index is out of range"))?;
        let capability = module
            .capabilities
            .get(call.capability as usize)
            .ok_or_else(|| Fault::internal("a capability index is out of range"))?;
        let signature = match &capability.kind {
            CapabilityKind::Function(signature) => signature,
            CapabilityKind::Namespace(functions) => {
                &functions
                    .get(call.function as usize)
                    .ok_or_else(|| Fault::internal("a capability function index is out of range"))?
                    .1
            }
        };
        Ok(signature.ret)
    }

    /// The declared parameter types of the capability function at `index`,
    /// in argument order.
    ///
    /// # Errors
    ///
    /// HS1099 if any of the three indices is out of range, which is a defect
    /// of the module rather than a property of any script.
    pub(crate) fn cap_call_params(&self, index: u32) -> Result<Vec<TypeRef>, Fault> {
        let module = self.module().module();
        let call = module
            .cap_calls
            .get(index as usize)
            .ok_or_else(|| Fault::internal("a capability call index is out of range"))?;
        let capability = module
            .capabilities
            .get(call.capability as usize)
            .ok_or_else(|| Fault::internal("a capability index is out of range"))?;
        let signature = match &capability.kind {
            CapabilityKind::Function(signature) => signature,
            CapabilityKind::Namespace(functions) => {
                &functions
                    .get(call.function as usize)
                    .ok_or_else(|| Fault::internal("a capability function index is out of range"))?
                    .1
            }
        };
        Ok(signature.params.clone())
    }

    /// Section 9.6: `args` compared against the declared parameter
    /// types of the capability function at `index`, before the call is
    /// made at all.
    ///
    /// A well-formed compiled module always satisfies this (the checker
    /// types every capability call's arguments against the same
    /// declaration this reads back from `cap_calls`), so this can only
    /// refuse a run whose bytecode does not match what a real compile of
    /// any program produces: a verifier gap, or a register a host handed a
    /// value it built with `hollowscript_vm::HostCall`, patched at rest, or
    /// otherwise corrupted between compiling and running. `hollowscript-wasm`'s
    /// `value_to_json` (and `hollowscript-capi`'s own copy) walk exactly
    /// this argument on the way to a host, so without this check a
    /// corrupted argument register can hand a host-facing JSON encoder a
    /// value of a kind and depth its own declared parameter type never
    /// allowed, which is how a single patched argument register in an
    /// otherwise verified container could overflow the host's own stack.
    ///
    /// # Errors
    ///
    /// HS1099 (not HS1013, which section 9.6 defines for what a *host*
    /// hands *back*, not for what the bytecode hands the host): this is
    /// always a defect of the module, never a property of a host's own
    /// behaviour, so a program can neither trigger nor observe it by
    /// writing ordinary source.
    pub(crate) fn check_capability_arguments(
        &self,
        index: u32,
        args: &[Value],
    ) -> Result<(), Fault> {
        let params = self.cap_call_params(index)?;
        if params.len() != args.len() {
            return Err(Fault::internal(
                "a capability call passed the wrong number of arguments",
            ));
        }
        for (&value, &ty) in args.iter().zip(params.iter()) {
            if !self.value_fits(normalise(self.convert_for(value, ty)), ty) {
                return Err(Fault::internal(
                    "a capability call passed an argument of the wrong type",
                ));
            }
        }
        Ok(())
    }

    /// The name of that capability function as a script writes it: `print`
    /// for a single function, `clock.now` for one of a namespace. Built only
    /// for a message, so an index out of range answers with a placeholder
    /// rather than a fault the caller would have to handle.
    pub(crate) fn cap_call_name(&self, index: u32) -> String {
        let module = self.module().module();
        let Some(call) = module.cap_calls.get(index as usize) else {
            return "?".to_owned();
        };
        let Some(capability) = module.capabilities.get(call.capability as usize) else {
            return "?".to_owned();
        };
        let name = module.string(capability.name);
        match &capability.kind {
            CapabilityKind::Function(_) => name.to_owned(),
            CapabilityKind::Namespace(functions) => match functions.get(call.function as usize) {
                Some((function, _)) => format!("{name}.{}", module.string(*function)),
                None => name.to_owned(),
            },
        }
    }

    /// Section 9.6's rule, applied to what a host returned from the
    /// capability function at `index`. Answers the value the program should
    /// see, which differs from `value` only for the two normalisations the
    /// section names: an `Int` for a declared `Float` is converted, and a
    /// negative zero becomes `0.0`.
    ///
    /// # Errors
    ///
    /// HS1013 if the value is not one the declaration accepts.
    pub(crate) fn accept_result(&self, index: u32, value: Value) -> Result<Value, Fault> {
        let Some(ret) = self.cap_call_result(index)? else {
            // A function that returns nothing may only answer `none`; the
            // compiler gives the call's register no other use.
            return if matches!(value, Value::None) {
                Ok(value)
            } else {
                Err(self.wrong_type(index, None))
            };
        };
        let converted = normalise(self.convert_for(value, ret));
        if self.value_fits(converted, ret) {
            Ok(converted)
        } else {
            Err(self.wrong_type(index, Some(ret)))
        }
    }

    /// Section 9.6's acceptance rule applied to an argument a host passes
    /// to an exported function, which section 12.4's step 2 accepts by that
    /// same rule. Answers the value the program should see, which differs
    /// from `value` only for the two normalisations the section names, or
    /// `None` for an argument the declaration does not accept, which the
    /// caller reports as HS1202 rather than as a fault: an argument that is
    /// refused never reaches the program, so nothing has faulted.
    #[must_use]
    pub fn accept_argument(&self, value: Value, ty: TypeRef) -> Option<Value> {
        let converted = normalise(self.convert_for(value, ty));
        self.value_fits(converted, ty).then_some(converted)
    }

    /// The type a declaration names, as a host would write it, for a message
    /// about an argument or a result that did not fit it.
    #[must_use]
    pub fn type_name(&self, ty: TypeRef) -> String {
        self.describe_type(ty, 0)
    }

    /// The one conversion section 9.6 allows, and only at the top level: an
    /// `Int` for a declared `Float` or `Float?`. An `Int` the conversion
    /// cannot represent exactly is left alone, so that it fails the check
    /// that follows rather than being rounded silently.
    fn convert_for(&self, value: Value, ty: TypeRef) -> Value {
        let types = &self.module().module().types;
        let converts = match types.get(ty) {
            Some(TransferType::Float) => true,
            Some(TransferType::Optional(inner)) => {
                matches!(types.get(*inner), Some(TransferType::Float))
            }
            _ => false,
        };
        match value {
            Value::Int(i) if converts => int_as_float(i).map_or(value, Value::Float),
            other => other,
        }
    }

    /// HS1013 for the capability function at `index`, naming the type it
    /// declared.
    fn wrong_type(&self, index: u32, ret: Option<TypeRef>) -> Fault {
        let name = self.cap_call_name(index);
        let expected = match ret {
            Some(ret) => self.describe_type(ret, 0),
            None => "nothing".to_owned(),
        };
        Fault::new(
            FaultCode::CapabilityType,
            &[("name", &name), ("expected", &expected)],
        )
    }

    /// Whether `value` is a language value of the type `ty` names. Walks an
    /// explicit stack rather than recursing, so a deeply nested value the
    /// host built costs heap rather than host stack.
    fn value_fits(&self, value: Value, ty: TypeRef) -> bool {
        let types = &self.module().module().types;
        let mut pending = vec![(value, ty)];
        while let Some((value, ty)) = pending.pop() {
            let Some(declared) = types.get(ty) else {
                return false;
            };
            match (declared, value) {
                (TransferType::Int, Value::Int(_)) | (TransferType::Bool, Value::Bool(_)) => {}
                // Section 3.4: a `Float` is finite and is never a negative
                // zero, so a host that hands back a NaN, an infinity or a
                // `-0.0` has not returned a language value at all. The
                // normalisation above has already turned an acceptable
                // negative zero into `0.0`; anything left is a defect.
                (TransferType::Float, Value::Float(f)) => {
                    if !f.is_finite() || (f == 0.0 && f.is_sign_negative()) {
                        return false;
                    }
                }
                (TransferType::Optional(inner), Value::None) => {
                    // `none` fits every optional, and section 9.6 forbids an
                    // optional of an optional, so there is nothing to check
                    // beyond the outer type being one.
                    let _ = inner;
                }
                (TransferType::Optional(inner), other) => pending.push((other, *inner)),
                (TransferType::String, Value::Heap(handle)) => {
                    if !matches!(self.heap().get(handle), Some(Object::Str(_))) {
                        return false;
                    }
                }
                // Section 9.6: `Filter` is a parameter-only type, never a
                // result, so `value_fits` never had to know its shape until
                // `check_capability_arguments` started reusing this same
                // check for a capability's outgoing arguments too; a
                // `Filter` is a plain heap leaf, never built with anything
                // to recurse into.
                (TransferType::Filter, Value::Heap(handle)) => {
                    if !matches!(self.heap().get(handle), Some(Object::Filter(_))) {
                        return false;
                    }
                }
                (TransferType::List(element), Value::Heap(handle)) => {
                    let Some(Object::List(elements)) = self.heap().get(handle) else {
                        return false;
                    };
                    pending.extend(elements.iter().map(|&value| (value, *element)));
                }
                (TransferType::Map(key, value_type), Value::Heap(handle)) => {
                    let Some(Object::Map(entries)) = self.heap().get(handle) else {
                        return false;
                    };
                    if !self.keys_are_ordered(entries.iter().map(|entry| entry.key)) {
                        return false;
                    }
                    for entry in entries {
                        if !self.key_fits(entry.key, *key) {
                            return false;
                        }
                        pending.push((entry.value, *value_type));
                    }
                }
                (TransferType::Set(element), Value::Heap(handle)) => {
                    let Some(Object::Set(elements)) = self.heap().get(handle) else {
                        return false;
                    };
                    if !self.keys_are_ordered(elements.iter().copied()) {
                        return false;
                    }
                    if !elements.iter().all(|&key| self.key_fits(key, *element)) {
                        return false;
                    }
                }
                (TransferType::Record(fields), Value::Heap(handle)) => {
                    let Some(Object::Record {
                        shape,
                        fields: values,
                    }) = self.heap().get(handle)
                    else {
                        return false;
                    };
                    // The shape, its values and the declared fields are all
                    // in sorted field order, so a missing or an extra field
                    // shows up as a mismatch at one position.
                    if shape.len() != fields.len() || values.len() != fields.len() {
                        return false;
                    }
                    let module = self.module().module();
                    for (((name, field_type), &id), &value) in
                        fields.iter().zip(shape.iter()).zip(values.iter())
                    {
                        if module.string(id) != name {
                            return false;
                        }
                        pending.push((value, *field_type));
                    }
                }
                (TransferType::Enum { id, variants, .. }, Value::Heap(handle)) => {
                    let Some(Object::Enum { variant, payload }) = self.heap().get(handle) else {
                        return false;
                    };
                    let Ok(desc) = self.variant_desc(*variant) else {
                        return false;
                    };
                    // Section 10.4: an enum is identified by its declaring
                    // module and name, not by the spelling alone. A
                    // program can declare its own enum with the same name as
                    // a capability's; comparing `desc.enum_name` against
                    // `name` here let a value of one satisfy a declaration of
                    // the other, whichever the module's variant table
                    // happened to reuse first.
                    if desc.decl_module != id.module || desc.decl_index != id.index {
                        return false;
                    }
                    let module = self.module().module();
                    let written = module.string(desc.name);
                    let Some(declared) = variants.iter().find(|v| v.name == written) else {
                        return false;
                    };
                    if declared.payload.len() != payload.len() {
                        return false;
                    }
                    pending.extend(
                        payload
                            .iter()
                            .copied()
                            .zip(declared.payload.iter().copied()),
                    );
                }
                // Everything else is a value of the wrong kind for what
                // was declared, including a `Filter`, which section 9.6
                // allows only as a parameter and so never as a result.
                _ => return false,
            }
        }
        true
    }

    /// Whether a map or set key is of the key type `ty` names. A key type is
    /// `Int`, `String` or `Bool` (section 9.6); anything else is refused
    /// here rather than treated as a key.
    fn key_fits(&self, key: MapKey, ty: TypeRef) -> bool {
        let declared = self.module().module().types.get(ty);
        match (declared, key) {
            (Some(TransferType::Int), MapKey::Int(_))
            | (Some(TransferType::Bool), MapKey::Bool(_)) => true,
            (Some(TransferType::String), MapKey::Str(handle)) => {
                matches!(self.heap().get(handle), Some(Object::Str(_)))
            }
            _ => false,
        }
    }

    /// Whether these keys are strictly ascending, which is what makes a map
    /// or a set a language value rather than a list of pairs: a repeated key
    /// or an unsorted one is HS1013 (section 9.6).
    fn keys_are_ordered(&self, keys: impl Iterator<Item = MapKey>) -> bool {
        let mut previous: Option<MapKey> = None;
        for key in keys {
            if let Some(before) = previous {
                if self.heap().compare_keys(before, key) != std::cmp::Ordering::Less {
                    return false;
                }
            }
            previous = Some(key);
        }
        true
    }

    /// The declared type as a script would write it, for HS1013's message.
    /// Stops naming at [`DESCRIBE_DEPTH`], since the message is for a human
    /// reading it, not for a machine parsing it.
    fn describe_type(&self, ty: TypeRef, depth: usize) -> String {
        if depth >= DESCRIBE_DEPTH {
            return "...".to_owned();
        }
        let Some(declared) = self.module().module().types.get(ty) else {
            return "?".to_owned();
        };
        match declared {
            TransferType::Int => "Int".to_owned(),
            TransferType::Float => "Float".to_owned(),
            TransferType::Bool => "Bool".to_owned(),
            TransferType::String => "String".to_owned(),
            TransferType::Filter => "Filter".to_owned(),
            TransferType::List(element) => {
                format!("[{}]", self.describe_type(*element, depth + 1))
            }
            TransferType::Map(key, value) => format!(
                "Map<{}, {}>",
                self.describe_type(*key, depth + 1),
                self.describe_type(*value, depth + 1)
            ),
            TransferType::Set(element) => {
                format!("Set<{}>", self.describe_type(*element, depth + 1))
            }
            TransferType::Optional(inner) => {
                format!("{}?", self.describe_type(*inner, depth + 1))
            }
            TransferType::Record(fields) => {
                let written: Vec<String> = fields
                    .iter()
                    .map(|(name, field)| {
                        format!("{name}: {}", self.describe_type(*field, depth + 1))
                    })
                    .collect();
                format!("{{ {} }}", written.join(", "))
            }
            TransferType::Enum { name, .. } => name.clone(),
        }
    }
}

/// Section 3.4's `Float` carries 53 bits of integer precision, so an `Int`
/// outside that range is not one the conversion can preserve. Answering
/// `None` leaves the value an `Int` where a `Float` was declared, which the
/// caller then reports as the HS1013 it is rather than rounding silently.
fn int_as_float(i: i64) -> Option<f64> {
    const LIMIT: i64 = 1 << 53;
    if (-LIMIT..=LIMIT).contains(&i) {
        #[allow(
            clippy::cast_precision_loss,
            reason = "the range check above makes the conversion exact"
        )]
        Some(i as f64)
    } else {
        None
    }
}

/// Section 3.4 has no negative zero, so a host that hands one back means
/// `0.0` and is given it.
fn normalise(value: Value) -> Value {
    match value {
        // `-0.0 == 0.0`, so the sign is the whole question here: the two
        // are a different value only in their bits.
        Value::Float(f) if f == 0.0 && f.is_sign_negative() => Value::Float(0.0),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use crate::capability::{CapabilityTable, HostFn};
    use crate::hostvalues::HostCall;
    use crate::interp::Interpreter;
    use crate::value::{MapEntry, MapKey, Object, Value};
    use hollowscript_bytecode::module::{
        CapCall, Capability, CapabilityKind, Function, FunctionKind, Module, ModuleEntry, Signature,
    };
    use hollowscript_bytecode::opcode::{Instruction, Op};
    use hollowscript_bytecode::verify::verify;
    use hollowscript_core::fault::Fault;
    use hollowscript_core::transfer::{TransferType, TypeRef, VariantDef};
    use hollowscript_core::{Control, Limits};

    /// A host function written as a closure, as every binding is.
    type Host = Box<dyn FnMut(&[Value], &mut HostCall<'_>) -> Result<Value, Fault>>;

    /// A module whose one export calls one capability and returns what it
    /// answered, so that a test writes only the declared result type and the
    /// value the host hands back.
    fn program(ret: Option<TypeRef>, namespace: bool, module: &mut Module) {
        let path = module.add_string("main.hws").unwrap();
        let cap_name = module.add_string("ask").unwrap();
        let signature = Signature {
            params: vec![],
            ret,
        };
        let kind = if namespace {
            let one = module.add_string("once").unwrap();
            CapabilityKind::Namespace(vec![(one, signature)])
        } else {
            CapabilityKind::Function(signature)
        };
        module.capabilities.push(Capability {
            name: cap_name,
            kind,
        });
        module.cap_calls.push(CapCall {
            capability: 0,
            function: 0,
        });
        let fn_name = module.add_string("run").unwrap();
        module.functions.push(Function {
            name: fn_name,
            kind: FunctionKind::Closure,
            arity: 0,
            register_count: 2,
            returns_value: true,
            captures: vec![],
            constants: vec![],
            code: vec![
                Instruction::abx(Op::CallCap, 1, 0).encode(),
                Instruction::abc(Op::Return, 1, 0, 0).encode(),
            ],
            spans: vec![],
        });
        let init_name = module.add_string("main").unwrap();
        module.functions.push(Function {
            name: init_name,
            kind: FunctionKind::ModuleInit,
            arity: 0,
            register_count: 1,
            returns_value: false,
            captures: vec![],
            constants: vec![],
            code: vec![Instruction::abc(Op::ReturnNone, 0, 0, 0).encode()],
            spans: vec![],
        });
        module.modules.push(ModuleEntry { path, init: 1 });
    }

    /// Runs that program with `host` bound to its capability, and answers
    /// the interpreter (so that a test can look at the heap) and what the
    /// program saw.
    fn run(
        declare: impl FnOnce(&mut Module) -> Option<TypeRef>,
        namespace: bool,
        host: Host,
    ) -> (Interpreter, Result<Option<Value>, Fault>) {
        let mut module = Module::new();
        let ret = declare(&mut module);
        program(ret, namespace, &mut module);
        let verified = verify(module).unwrap();
        let boxed: Box<dyn HostFn> = Box::new(host);
        let mut vm = Interpreter::new(verified, CapabilityTable::new(vec![boxed]));
        vm.initialise(&Limits::default(), &mut Control::none())
            .unwrap();
        let result = vm.call_export(0, &[], &Limits::default(), &mut Control::none());
        (vm, result)
    }

    /// The value the program saw, for a result the check accepts.
    fn accepted(declare: impl FnOnce(&mut Module) -> Option<TypeRef>, host: Host) -> Value {
        let (_, result) = run(declare, false, host);
        result
            .expect("the result should be accepted")
            .expect("the export returns a value")
    }

    /// The HS1013 message, for a result the check refuses.
    fn refused(declare: impl FnOnce(&mut Module) -> Option<TypeRef>, host: Host) -> String {
        let (_, result) = run(declare, false, host);
        let fault = result.expect_err("the result should be refused");
        assert_eq!(fault.code.code().number(), 1013);
        fault.message
    }

    /// The declarations several tests share, as closures rather than
    /// functions so that each can be passed more than once.
    const INT: fn(&mut Module) -> Option<TypeRef> =
        |module| Some(module.types.add(TransferType::Int));
    const FLOAT: fn(&mut Module) -> Option<TypeRef> =
        |module| Some(module.types.add(TransferType::Float));

    #[test]
    fn a_value_of_the_declared_type_is_accepted() {
        let value = accepted(INT, Box::new(|_, _| Ok(Value::Int(7))));
        assert!(matches!(value, Value::Int(7)));
    }

    /// Section 9.6's one conversion.
    #[test]
    fn an_int_is_converted_for_a_declared_float() {
        let value = accepted(FLOAT, Box::new(|_, _| Ok(Value::Int(3))));
        assert!(matches!(value, Value::Float(f) if (f - 3.0).abs() < f64::EPSILON));
    }

    #[test]
    fn an_int_is_converted_for_a_declared_optional_float() {
        let value = accepted(
            |module| {
                let inner = module.types.add(TransferType::Float);
                Some(module.types.add(TransferType::Optional(inner)))
            },
            Box::new(|_, _| Ok(Value::Int(3))),
        );
        assert!(matches!(value, Value::Float(f) if (f - 3.0).abs() < f64::EPSILON));
    }

    /// "that conversion is made at the top level of the result only, and
    /// nothing is converted inside a collection".
    #[test]
    fn an_int_inside_a_list_of_float_is_not_converted() {
        let message = refused(
            |module| {
                let inner = module.types.add(TransferType::Float);
                Some(module.types.add(TransferType::List(inner)))
            },
            Box::new(|_, call| call.list(vec![Value::Int(1)])),
        );
        assert_eq!(
            message,
            "The capability 'ask' returned a value of the wrong type; expected [Float]."
        );
    }

    /// An `Int` a `Float` cannot hold exactly is refused, not rounded.
    #[test]
    fn an_int_beyond_the_float_precision_is_refused() {
        let message = refused(FLOAT, Box::new(|_, _| Ok(Value::Int((1 << 53) + 1))));
        assert!(message.contains("expected Float"), "{message}");
    }

    #[test]
    fn a_scalar_of_the_wrong_kind_is_refused() {
        let message = refused(
            |module| Some(module.types.add(TransferType::String)),
            Box::new(|_, _| Ok(Value::Bool(true))),
        );
        assert!(message.contains("expected String"), "{message}");
    }

    #[test]
    fn a_non_finite_float_is_not_a_language_value() {
        let message = refused(FLOAT, Box::new(|_, _| Ok(Value::Float(f64::NAN))));
        assert!(message.contains("expected Float"), "{message}");
    }

    /// Section 3.4 has no negative zero, so one is normalised, not refused.
    #[test]
    fn a_negative_zero_becomes_zero() {
        let value = accepted(FLOAT, Box::new(|_, _| Ok(Value::Float(-0.0))));
        let Value::Float(f) = value else {
            panic!("expected a float, got {value:?}");
        };
        assert!(f == 0.0 && !f.is_sign_negative());
    }

    #[test]
    fn none_fits_an_optional_and_a_wrong_scalar_does_not() {
        let optional: fn(&mut Module) -> Option<TypeRef> = |module| {
            let inner = module.types.add(TransferType::Int);
            Some(module.types.add(TransferType::Optional(inner)))
        };
        let value = accepted(optional, Box::new(|_, _| Ok(Value::None)));
        assert!(matches!(value, Value::None));
        let message = refused(optional, Box::new(|_, _| Ok(Value::Bool(false))));
        assert!(message.contains("expected Int?"), "{message}");
    }

    /// A function that declares no result may only answer `none`.
    #[test]
    fn a_capability_that_returns_nothing_may_only_answer_none() {
        let value = accepted(|_| None, Box::new(|_, _| Ok(Value::None)));
        assert!(matches!(value, Value::None));
        let message = refused(|_| None, Box::new(|_, _| Ok(Value::Int(1))));
        assert_eq!(
            message,
            "The capability 'ask' returned a value of the wrong type; expected nothing."
        );
    }

    /// A namespace function is named the way a script writes it.
    #[test]
    fn a_namespace_function_is_named_with_its_capability() {
        let (_, result) = run(INT, true, Box::new(|_, _| Ok(Value::Bool(true))));
        let fault = result.expect_err("a Bool is not an Int");
        assert_eq!(
            fault.message,
            "The capability 'ask.once' returned a value of the wrong type; expected Int."
        );
    }

    #[test]
    fn a_record_with_a_missing_field_is_refused() {
        let shape: fn(&mut Module) -> Option<TypeRef> = |module| {
            let string = module.types.add(TransferType::String);
            let int = module.types.add(TransferType::Int);
            Some(module.types.add(TransferType::Record(vec![
                ("name".to_owned(), string),
                ("size".to_owned(), int),
            ])))
        };
        // The host builds the record straight on the heap, with only the
        // first of the two declared fields.
        let message = refused(
            shape,
            Box::new(|_, call| {
                let name = call.string("a")?;
                let id = hollowscript_bytecode::module::StringId(0);
                let object = Object::Record {
                    shape: vec![id],
                    fields: vec![name],
                };
                call.heap()
                    .alloc(object)
                    .map_err(|_| Fault::internal("the heap is exhausted"))
            }),
        );
        assert_eq!(
            message,
            "The capability 'ask' returned a value of the wrong type; \
             expected { name: String, size: Int }."
        );
    }

    /// Section 9.6: "a map or set given in any order is put in key order by
    /// the binding".
    #[test]
    fn a_map_the_host_wrote_out_of_order_is_sorted_and_accepted() {
        let map: fn(&mut Module) -> Option<TypeRef> = |module| {
            let int = module.types.add(TransferType::Int);
            Some(module.types.add(TransferType::Map(int, int)))
        };
        let (vm, result) = run(
            map,
            false,
            Box::new(|_, call| {
                call.map(vec![
                    MapEntry {
                        key: MapKey::Int(2),
                        value: Value::Int(20),
                    },
                    MapEntry {
                        key: MapKey::Int(1),
                        value: Value::Int(10),
                    },
                ])
            }),
        );
        let Some(Value::Heap(handle)) = result.unwrap() else {
            panic!("expected a map");
        };
        let Some(Object::Map(entries)) = vm.heap().get(handle) else {
            panic!("expected a map object");
        };
        let keys: Vec<MapKey> = entries.iter().map(|entry| entry.key).collect();
        assert!(matches!(keys.as_slice(), [MapKey::Int(1), MapKey::Int(2)]));
    }

    /// An order the binding can fix; two values for one key it cannot.
    #[test]
    fn a_repeated_map_key_is_a_host_defect() {
        let (_, result) = run(
            |module| {
                let int = module.types.add(TransferType::Int);
                Some(module.types.add(TransferType::Map(int, int)))
            },
            false,
            Box::new(|_, call| {
                call.map(vec![
                    MapEntry {
                        key: MapKey::Int(1),
                        value: Value::Int(10),
                    },
                    MapEntry {
                        key: MapKey::Int(1),
                        value: Value::Int(11),
                    },
                ])
            }),
        );
        let fault = result.expect_err("a repeated key is refused");
        assert_eq!(fault.code.code().number(), 1013);
    }

    #[test]
    fn an_enum_value_must_name_a_variant_of_the_declared_enum() {
        let message = refused(
            |module| {
                Some(module.types.add_enum(
                    "Colour",
                    vec![VariantDef {
                        name: "Red".to_owned(),
                        payload: vec![],
                    }],
                ))
            },
            Box::new(|_, _| Ok(Value::Int(0))),
        );
        assert!(message.contains("expected Colour"), "{message}");
    }

    /// Section 10.4: an enum is identified by its declaring module
    /// and name, not by the spelling alone. `module.variants` here holds a
    /// variant of some *other* declaration also named `Colour.Red` (as a
    /// program's own `enum Colour` would compile to, at a different
    /// `decl_module`/`decl_index`); the capability declares a *different*
    /// `Colour`, and a host that answers the wrong one's variant is refused
    /// exactly as if it had answered a value of the wrong kind, not
    /// accepted because the name happens to match.
    #[test]
    fn a_variant_of_a_different_declaration_with_the_same_name_is_refused() {
        use hollowscript_bytecode::module::VariantDesc;

        let message = refused(
            |module| {
                // A variant belonging to an unrelated declaration: a real
                // program module (0), not the capability's own `EnumId`.
                let enum_name = module.add_string("Colour").unwrap();
                let variant_name = module.add_string("Red").unwrap();
                module.variants.push(VariantDesc {
                    enum_name,
                    name: variant_name,
                    index: 0,
                    arity: 0,
                    decl_module: 0,
                    decl_index: 0,
                });
                Some(module.types.add_enum(
                    "Colour",
                    vec![VariantDef {
                        name: "Red".to_owned(),
                        payload: vec![],
                    }],
                ))
            },
            Box::new(|_, call: &mut HostCall<'_>| {
                // The host answers variant 0 directly (bypassing
                // `HostCall::enum_value`, which a real host always goes
                // through and which never hands back a foreign variant):
                // this is the shape a defect in that layer, or in
                // `hollowscript::hosttypes`, would produce.
                call.heap()
                    .alloc(Object::Enum {
                        variant: 0,
                        payload: vec![],
                    })
                    .map_err(|_| Fault::internal("heap exhausted"))
            }),
        );
        assert!(message.contains("expected Colour"), "{message}");
    }
}

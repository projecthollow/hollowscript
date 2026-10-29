//! Checking, formatting and compiling source, in the browser, without a
//! server. Behind the `tooling` feature only: this is what pulls in
//! `hollowscript` (the facade), and through it `hollowscript-syntax` and
//! `hollowscript-check`, so an editor page can go from source text to a
//! running program without shelling out to the CLI.
//!
//! This is a **separate build from the VM-only one** the crate's own module
//! doc describes: [`crate::abi`] and [`crate::abi_tooling`] both compile into the
//! same `cdylib`, but only when `tooling` is enabled, and a published game
//! is built with the default, feature-less crate, which stays
//! exactly what it was: bytecode in, running it, nothing else. `web/`
//! documents building both and measures the size difference.
//!
//! Every function here answers JSON, the same convention [`crate::abi`]
//! already uses for text results, built by the same small, dependency-free
//! encoder (this module cannot reuse `hollowscript-capi`'s copy of it
//! without depending on that crate, which would pull the C ABI's own
//! `unsafe` surface in for no reason, so it is duplicated rather than
//! shared, exactly the size of `hollowscript-capi::diagnostics_to_json`
//! and `format_to_json`).

use hollowscript::{
    BuildOutcome, CapabilityDecl, CapabilityInterface, CapabilityShape, Control, Diagnostic,
    Loader, LoaderError, Options, build, check as hs_check, format as hs_format,
};
use hollowscript_core::filter::{FieldDesc, FieldType, FilterSchema};
use hollowscript_core::transfer::{TransferType, TypeRef, TypeTable};
use std::collections::BTreeMap;

// The editor services (highlighting, keywords, and the analysis handle a
// browser editor queries) live in a child module so that they can use this
// module's private capability and module-map readers without widening them.
pub(crate) mod ide;

/// A [`Loader`] over a fixed set of modules a page supplies up front, for a
/// Code workspace of more than one file (section 10.3): `check` and
/// `compile` resolve every `import` this way instead of always answering
/// "not found", the one thing a single-file browser build could do before.
/// `hollowscript-check`'s own `check` (and, through it, `hollowscript::build`)
/// already resolves an import's path to its *canonical* form (relative to
/// the importing module, refusing one that climbs above the entry module's
/// own root with `..`) before ever calling [`Loader::load`]; this loader
/// only has to answer a canonical path with the source `modules` already
/// holds it under, exactly as `hollowscript::instance`'s own closure-backed
/// loaders (used by `hollowscript-cli` and any other host with a real file
/// system) do for their own storage.
struct MapLoader {
    modules: BTreeMap<String, String>,
}

impl Loader for MapLoader {
    fn load(&mut self, canonical_path: &str) -> Result<Vec<u8>, LoaderError> {
        self.modules
            .get(canonical_path)
            .map(|source| source.clone().into_bytes())
            .ok_or(LoaderError::NotFound)
    }
}

/// Parses `modules_json` into a [`MapLoader`]: a JSON object mapping each
/// non-entry module's own canonical path (relative to the entry module,
/// exactly as an `import` line writes it, section 10.3) to its source text,
/// `{"lib/helpers.hws": "export fn double(n: Int): Int {\n    return n * 2\n}\n", ...}`.
/// The empty string (no object at all) is the common case: a single-file
/// program needs no entry here, and behaves exactly as it did before this
/// loader existed (every import "not found").
///
/// # Errors
///
/// The reason, as plain text, if `modules_json` is not that shape.
fn module_loader(modules_json: &str) -> Result<MapLoader, String> {
    if modules_json.trim().is_empty() {
        return Ok(MapLoader {
            modules: BTreeMap::new(),
        });
    }
    let parsed = hollowscript::json::parse(modules_json.trim())
        .map_err(|e| format!("the modules are not JSON: {}", e.message))?;
    let hollowscript::json::Json::Object(fields) = parsed else {
        return Err("the modules must be a JSON object".to_owned());
    };
    let mut modules = BTreeMap::new();
    for (path, source) in fields {
        let hollowscript::json::Json::String(source) = source else {
            return Err(format!("the module '{path}' must be a JSON string"));
        };
        modules.insert(path, source);
    }
    Ok(MapLoader { modules })
}

/// The capabilities every CLI run grants regardless of `--allow`
/// (`hollowscript-cli::default_grants::capabilities_for`'s own `print`,
/// `clock` and `random` entries, reproduced here so an editor page checks
/// and compiles against the same standard interface the CLI and a plain
/// `hollowscript-capi` run already do): a program that only `uses` these
/// three checks the same way in every one of this implementation's hosts,
/// with no declaration needed from the page. A host-defined capability
/// beyond these is declared separately; see [`check`] and [`compile`].
fn standard_capabilities(types: &mut TypeTable) -> Vec<CapabilityDecl> {
    let string_ty = types.add(TransferType::String);
    let int_ty = types.add(TransferType::Int);
    let float_ty = types.add(TransferType::Float);
    vec![
        CapabilityDecl {
            name: "print".to_owned(),
            shape: CapabilityShape::Function {
                params: vec![string_ty],
                ret: None,
            },
        },
        CapabilityDecl {
            name: "clock".to_owned(),
            shape: CapabilityShape::Namespace(vec![
                ("now".to_owned(), vec![], Some(string_ty)),
                ("today".to_owned(), vec![], Some(string_ty)),
                ("unixMillis".to_owned(), vec![], Some(int_ty)),
                ("monotonicMillis".to_owned(), vec![], Some(int_ty)),
            ]),
        },
        CapabilityDecl {
            name: "random".to_owned(),
            shape: CapabilityShape::Namespace(vec![
                ("int".to_owned(), vec![int_ty, int_ty], Some(int_ty)),
                ("float".to_owned(), vec![], Some(float_ty)),
            ]),
        },
    ]
}

/// One entry [`extra_capabilities`] parsed out of its JSON, before entries
/// sharing a `name` are grouped into one capability declaration.
struct Entry {
    name: String,
    function: Option<String>,
    params: Vec<TypeRef>,
    ret: Option<TypeRef>,
}

/// A type description, as [`extra_capabilities`]'s `params` and `result`
/// entries write it: a bare string for one of the six scalars
/// (`"Int"`/`"Float"`/`"Bool"`/`"String"`/`"Filter"`, `Filter` valid only in
/// `params`, per [`TransferType::Filter`]'s own restriction, which the
/// checker enforces, not this parser), or a JSON object with exactly one
/// field naming a container kind and describing its own element type(s) the
/// same way, recursively:
///
/// - `{"list": T}`, `{"set": T}`, `{"optional": T}`
/// - `{"map": {"key": T, "value": T}}`
/// - `{"record": {"field": T, ...}}`, at least one field
/// - `{"enum": {"name": "E", "variants": {"V": [T, ...], ...}}}`, at least
///   one variant; a payload-less variant still needs its array, empty
/// - `{"ok": T, "err": T}`, `Result<T, E>` (section 8.2), kept as a bare
///   two-field object rather than a single-field tag, matching the shape
///   this boundary has always taken for a capability's own `result`
///
/// This is the same encoding `hollowscript-capi`'s value JSON uses for a
/// *value* crossing the boundary (`{"list": [...]}`, `{"record": {...}}`,
/// and so on), applied here to a *type*, one level up: a `{"record": {...}}`
/// value's shape is exactly what a `{"record": {...}}` type here declares.
///
/// # Errors
///
/// The reason, as plain text, if `json` does not have one of the shapes
/// above.
fn declared_type(
    json: &hollowscript::json::Json,
    types: &mut TypeTable,
) -> Result<TypeRef, String> {
    use hollowscript::json::Json;
    match json {
        Json::String(name) => Ok(match name.as_str() {
            "Int" => types.add(TransferType::Int),
            "Float" => types.add(TransferType::Float),
            "Bool" => types.add(TransferType::Bool),
            "String" => types.add(TransferType::String),
            "Filter" => types.add(TransferType::Filter),
            other => return Err(format!("'{other}' is not a type this boundary can declare")),
        }),
        Json::Object(fields) => {
            let field = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| v);
            // `{"ok": T, "err": T}` is kept as a flat two-field object,
            // matching the shape a capability's own `result` has always
            // taken here, rather than folded under a single-field tag like
            // the container kinds below.
            if let (Some(ok), Some(err)) = (field("ok"), field("err")) {
                if fields.len() != 2 {
                    return Err(
                        "a Result type's object must have exactly \"ok\" and \"err\"".to_owned(),
                    );
                }
                let ok_ty = declared_type(ok, types)?;
                let err_ty = declared_type(err, types)?;
                return Ok(result_of(types, ok_ty, err_ty));
            }
            let [(tag, body)] = <[(String, Json); 1]>::try_from(fields.clone()).map_err(|_| {
                "a type object must have exactly one field naming its kind \
                 (list, set, map, optional, record, enum), or be \"ok\"/\"err\""
                    .to_owned()
            })?;
            match tag.as_str() {
                "list" => {
                    let element = declared_type(&body, types)?;
                    Ok(types.add(TransferType::List(element)))
                }
                "set" => {
                    let element = declared_type(&body, types)?;
                    Ok(types.add(TransferType::Set(element)))
                }
                "optional" => {
                    let inner = declared_type(&body, types)?;
                    Ok(types.add(TransferType::Optional(inner)))
                }
                "map" => {
                    let Json::Object(map_fields) = &body else {
                        return Err("\"map\" needs an object with \"key\" and \"value\"".to_owned());
                    };
                    let get = |key: &str| map_fields.iter().find(|(k, _)| k == key).map(|(_, v)| v);
                    let (Some(key), Some(value)) = (get("key"), get("value")) else {
                        return Err("\"map\" needs \"key\" and \"value\"".to_owned());
                    };
                    let key_ty = declared_type(key, types)?;
                    let value_ty = declared_type(value, types)?;
                    Ok(types.add(TransferType::Map(key_ty, value_ty)))
                }
                "record" => {
                    let Json::Object(record_fields) = &body else {
                        return Err("\"record\" needs an object".to_owned());
                    };
                    if record_fields.is_empty() {
                        return Err("\"record\" needs at least one field".to_owned());
                    }
                    let mut built = Vec::with_capacity(record_fields.len());
                    for (name, field_ty) in record_fields {
                        built.push((name.clone(), declared_type(field_ty, types)?));
                    }
                    Ok(types.add(TransferType::Record(built)))
                }
                "enum" => {
                    let Json::Object(enum_fields) = &body else {
                        return Err("\"enum\" needs an object".to_owned());
                    };
                    let get =
                        |key: &str| enum_fields.iter().find(|(k, _)| k == key).map(|(_, v)| v);
                    let Some(Json::String(name)) = get("name") else {
                        return Err("\"enum\" needs a string \"name\"".to_owned());
                    };
                    let Some(Json::Object(variant_fields)) = get("variants") else {
                        return Err("\"enum\" needs an object \"variants\"".to_owned());
                    };
                    if variant_fields.is_empty() {
                        return Err("\"enum\" needs at least one variant".to_owned());
                    }
                    let mut variants = Vec::with_capacity(variant_fields.len());
                    for (variant_name, payload_json) in variant_fields {
                        let Json::Array(payload_items) = payload_json else {
                            return Err(format!(
                                "variant '{variant_name}' needs an array of payload types"
                            ));
                        };
                        let mut payload = Vec::with_capacity(payload_items.len());
                        for item in payload_items {
                            payload.push(declared_type(item, types)?);
                        }
                        variants.push(hollowscript_core::transfer::VariantDef {
                            name: variant_name.clone(),
                            payload,
                        });
                    }
                    // A fresh identity per declared enum (section 10.4,
                    // section 10.4): each `{"enum": ...}` here names a distinct
                    // declaration even if two happen to share a name.
                    Ok(types.add_enum(name.clone(), variants))
                }
                other => Err(format!(
                    "'{other}' is not a type kind this boundary can declare \
                     (list, set, map, optional, record, enum)"
                )),
            }
        }
        _ => Err("a type must be a string or an object naming its kind".to_owned()),
    }
}

/// `Result<ok, err>` (section 8.2), built once per distinct call the same
/// way [`declared_type`]'s own `{"ok": T, "err": T}` case, `params` and
/// `hollowscript-capi::read_capabilities`'s own `result_type_ref` all do.
fn result_of(types: &mut TypeTable, ok: TypeRef, err: TypeRef) -> TypeRef {
    types.add(TransferType::Enum {
        // The language's own `Result`, at its reserved id (see
        // `hollowscript_capi::result_type_of`, which does the same).
        id: hollowscript_core::transfer::EnumId {
            module: hollowscript_core::transfer::HOST_MODULE,
            index: hollowscript_core::transfer::RESULT_INDEX,
        },
        name: hollowscript::RESULT.to_owned(),
        variants: vec![
            hollowscript_core::transfer::VariantDef {
                name: hollowscript::OK.to_owned(),
                payload: vec![ok],
            },
            hollowscript_core::transfer::VariantDef {
                name: hollowscript::ERR.to_owned(),
                payload: vec![err],
            },
        ],
    })
}

/// A host-declared capability interface beyond the standard one, as a JSON
/// array of entries with a shape the JS wrapper builds directly:
/// `[{"name":"tasks","function":"list","params":["String"],"result":"String"}, ...]`.
/// `function` is omitted (or `null`) for a capability that is a single
/// function, not a namespace; entries sharing a `name` with a non-null
/// `function` are grouped into one namespace capability, the same rule
/// `hollowscript-capi`'s `HsCapabilityFn` uses. `params` is an array of
/// [`declared_type`] descriptions (a scalar name, or a tagged object for a
/// list, map, set, optional, record or enum). `result` is omitted (or
/// `null`) for a function with no result, one [`declared_type`] description
/// for a plain value, or `{"ok": <type>, "err": <type>}` for a capability
/// declared `Result<T, E>` (section 8.2).
///
/// The empty string (no declaration at all) is the common case: an editor
/// checking a program that only `uses` the standard capabilities needs
/// nothing here.
///
/// # Errors
///
/// The reason, as plain text, if `json` is not that shape.
fn extra_capabilities(json: &str, types: &mut TypeTable) -> Result<Vec<CapabilityDecl>, String> {
    if json.trim().is_empty() {
        return Ok(Vec::new());
    }
    let parsed = hollowscript::json::parse(json.trim())
        .map_err(|e| format!("the capabilities are not JSON: {}", e.message))?;
    let hollowscript::json::Json::Array(entries) = parsed else {
        return Err("the capabilities must be a JSON array".to_owned());
    };
    let mut raw = Vec::with_capacity(entries.len());
    for entry in entries {
        let hollowscript::json::Json::Object(fields) = entry else {
            return Err("each capability must be a JSON object".to_owned());
        };
        let field = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| v);
        let Some(hollowscript::json::Json::String(name)) = field("name") else {
            return Err("each capability needs a string \"name\"".to_owned());
        };
        let function = match field("function") {
            None | Some(hollowscript::json::Json::Null) => None,
            Some(hollowscript::json::Json::String(f)) => Some(f.clone()),
            Some(_) => return Err("\"function\" must be a string or null".to_owned()),
        };
        let params = match field("params") {
            None => Vec::new(),
            Some(hollowscript::json::Json::Array(items)) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(declared_type(item, types)?);
                }
                out
            }
            Some(_) => return Err("\"params\" must be an array".to_owned()),
        };
        let ret = match field("result") {
            None | Some(hollowscript::json::Json::Null) => None,
            Some(result_json) => Some(declared_type(result_json, types)?),
        };
        raw.push(Entry {
            name: name.clone(),
            function,
            params,
            ret,
        });
    }

    let mut order: Vec<String> = Vec::new();
    for entry in &raw {
        if !order.contains(&entry.name) {
            order.push(entry.name.clone());
        }
    }
    let mut decls = Vec::with_capacity(order.len());
    for name in &order {
        let members: Vec<&Entry> = raw.iter().filter(|e| &e.name == name).collect();
        let shape = if let [only] = members.as_slice() {
            if only.function.is_none() {
                CapabilityShape::Function {
                    params: only.params.clone(),
                    ret: only.ret,
                }
            } else {
                CapabilityShape::Namespace(vec![(
                    only.function.clone().unwrap_or_default(),
                    only.params.clone(),
                    only.ret,
                )])
            }
        } else {
            let mut functions = Vec::with_capacity(members.len());
            for member in &members {
                let Some(function) = member.function.clone() else {
                    return Err(format!(
                        "'{name}' has more than one entry, so each needs a \"function\" name"
                    ));
                };
                functions.push((function, member.params.clone(), member.ret));
            }
            CapabilityShape::Namespace(functions)
        };
        decls.push(CapabilityDecl {
            name: name.clone(),
            shape,
        });
    }
    Ok(decls)
}

/// The capability interface [`check`] and [`compile`] both use: the
/// standard always-granted set, plus whatever `extra_capabilities_json`
/// declares beyond it.
///
/// # Errors
///
/// The reason, as plain text, if `extra_capabilities_json` is malformed.
fn capability_interface(extra_capabilities_json: &str) -> Result<CapabilityInterface, String> {
    let mut types = TypeTable::new();
    let mut capabilities = standard_capabilities(&mut types);
    capabilities.extend(extra_capabilities(extra_capabilities_json, &mut types)?);
    Ok(CapabilityInterface {
        capabilities,
        types,
    })
}

/// A host's filter schema (Appendix A, section A.6), as JSON in the shape of
/// section 19.4's `filterSchema`: `{"fields": {"<name>": {"type": "enum" |
/// "text" | "textMulti" | "date" | "number" | "boolean", "aliases":
/// {"<input>": "<canonical>"}, "values": ["<canonical>", ...]}, ...}}`.
/// `aliases` and `values` are optional, and `values` is for `enum` only. The
/// empty string declares no schema, which is not the same as a schema with no
/// fields: with none, only the checks that need no schema are made (section
/// A.7), and with an empty one every predicate names an unknown field.
///
/// Read strictly, unlike section 19.4's own tolerant reading of the same
/// document, because a schema that is silently read as something looser
/// accepts what the host meant to refuse: an unknown member, a type that is
/// not one of the six, a field name or an alias input that is not lower case
/// (a filter's own field names are lower-cased, so such a field could never
/// match) and a `values` on anything but an `enum` are all refused.
///
/// # Errors
///
/// The reason, as plain text, if `json` is not that shape.
fn filter_schema(json: &str) -> Result<Option<FilterSchema>, String> {
    use hollowscript::json::Json;
    if json.trim().is_empty() {
        return Ok(None);
    }
    let parsed = hollowscript::json::parse(json.trim())
        .map_err(|e| format!("the filter schema is not JSON: {}", e.message))?;
    let Json::Object(members) = parsed else {
        return Err("the filter schema must be a JSON object".to_owned());
    };
    let [(member, Json::Object(fields))] = <[(String, Json); 1]>::try_from(members)
        .map_err(|_| "the filter schema must have exactly one member, \"fields\"".to_owned())?
    else {
        return Err("the filter schema's \"fields\" must be a JSON object".to_owned());
    };
    if member != "fields" {
        return Err(format!(
            "'{member}' is not a member of a filter schema (only \"fields\" is)"
        ));
    }
    let mut schema = FilterSchema::default();
    for (name, description) in fields {
        if name.is_empty() || name != name.to_lowercase() {
            return Err(format!(
                "the filter field '{name}' must be lower case and not empty"
            ));
        }
        let Json::Object(members) = description else {
            return Err(format!("the filter field '{name}' must be a JSON object"));
        };
        let mut ty = None;
        let mut aliases = BTreeMap::new();
        let mut values = None;
        for (key, value) in members {
            match (key.as_str(), value) {
                ("type", Json::String(text)) => {
                    ty = Some(match text.as_str() {
                        "enum" => FieldType::Enum,
                        "text" => FieldType::Text,
                        "textMulti" => FieldType::TextMulti,
                        "date" => FieldType::Date,
                        "number" => FieldType::Number,
                        "boolean" => FieldType::Boolean,
                        other => {
                            return Err(format!(
                                "'{other}' is not a filter field type for '{name}' (enum, text, \
                                 textMulti, date, number, boolean)"
                            ));
                        }
                    });
                }
                ("aliases", Json::Object(pairs)) => {
                    for (input, canonical) in pairs {
                        let Json::String(canonical) = canonical else {
                            return Err(format!(
                                "the alias '{input}' of '{name}' must map to a string"
                            ));
                        };
                        if input != input.to_lowercase() {
                            return Err(format!(
                                "the alias '{input}' of '{name}' must be lower case"
                            ));
                        }
                        aliases.insert(input, canonical);
                    }
                }
                ("values", Json::Array(items)) => {
                    let mut texts = Vec::with_capacity(items.len());
                    for item in items {
                        let Json::String(text) = item else {
                            return Err(format!("the values of '{name}' must be strings"));
                        };
                        texts.push(text);
                    }
                    values = Some(texts);
                }
                (other, _) => {
                    return Err(format!(
                        "'{other}' is not a valid member of the filter field '{name}' \
                         (type, aliases and values are, with those shapes)"
                    ));
                }
            }
        }
        let Some(ty) = ty else {
            return Err(format!("the filter field '{name}' needs a \"type\""));
        };
        if values.is_some() && ty != FieldType::Enum {
            return Err(format!(
                "the filter field '{name}' has values but is not an enum"
            ));
        }
        schema.fields.insert(
            name,
            FieldDesc {
                ty,
                aliases,
                values,
            },
        );
    }
    Ok(Some(schema))
}

/// What [`check`], [`compile`] and [`filter_literals`] each read from their
/// JSON arguments before they do anything: the capability interface, the
/// module loader and the filter schema, if there is one.
struct Inputs {
    capabilities: CapabilityInterface,
    loader: MapLoader,
    filter_schema: Option<FilterSchema>,
}

/// Reads the JSON arguments of [`check`] and [`compile`].
///
/// # Errors
///
/// The reason, as plain text, naming the argument that is malformed.
fn inputs(
    extra_capabilities_json: &str,
    modules_json: &str,
    filter_schema_json: &str,
) -> Result<Inputs, String> {
    Ok(Inputs {
        capabilities: capability_interface(extra_capabilities_json)?,
        loader: module_loader(modules_json)?,
        filter_schema: filter_schema(filter_schema_json)?,
    })
}

/// Checks `source` and answers a JSON array of diagnostics, in the shape
/// `corpus/FORMAT.md` and the CLI's `--json` mode use:
/// `[{"severity":"error","line":1,"code":"HS1099","message":"..."}, ...]`.
/// Checked against the standard always-granted capabilities (`print`,
/// `clock`, `random`; the same ones the CLI grants with no `--allow` and a
/// plain `hollowscript-capi` run with no `HsCapabilityFn` still has, since
/// they need no host state), plus whatever `extra_capabilities_json`
/// declares (see `extra_capabilities`; the empty string declares nothing
/// beyond the standard set). An `import` resolves against `modules_json`
/// (see `module_loader`; the empty string is a single-file program, where
/// an import always answers "not found", exactly as before this existed).
/// `filter_schema_json` (see `filter_schema`; the empty string declares none)
/// is the host's description of its records: with one, a filter literal is
/// also validated against it (Appendix A, section A.7), so a field the host
/// does not have is refused here (HS0708) and not met at run time.
///
/// A malformed `extra_capabilities_json`, `modules_json` or
/// `filter_schema_json` is reported the same way a malformed program is: as
/// a single HS1099 diagnostic in the answered array, not a second error
/// channel a caller has to check separately.
#[must_use]
pub fn check(
    source: &str,
    entry_name: &str,
    extra_capabilities_json: &str,
    modules_json: &str,
    filter_schema_json: &str,
) -> String {
    let Inputs {
        capabilities,
        mut loader,
        filter_schema,
    } = match inputs(extra_capabilities_json, modules_json, filter_schema_json) {
        Ok(inputs) => inputs,
        Err(reason) => {
            return diagnostics_to_json(
                &[malformed_input_diagnostic(&reason)],
                entry_name,
                source,
                &BTreeMap::new(),
            );
        }
    };
    let diagnostics = hs_check(
        source,
        entry_name,
        &capabilities,
        // With a schema, every filter literal is also validated against it
        // (Appendix A, section A.7): a field the host does not have, an
        // operator or a value that does not suit a field's type. Without
        // one, a literal is checked for syntax only.
        filter_schema.as_ref(),
        &mut loader,
        &mut Control::none(),
    );
    diagnostics_to_json(&diagnostics, entry_name, source, &loader.modules)
}

/// Formats `source` to its canonical layout and answers a JSON document, in
/// the shape `corpus/FORMAT.md` and the CLI's `--json` mode use:
/// `{"output":"...text..."|null,"diagnostics":[...]}`. Needs no checker, so
/// this works whether or not `source` would check cleanly, exactly as
/// `hollowscript-capi`'s `hs_format_json` documents, and needs no
/// capability interface either, for the same reason.
#[must_use]
pub fn format(source: &str, entry_name: &str) -> String {
    let formatted = hs_format(source, entry_name, &mut Control::none());
    format_to_json(
        formatted.output.as_deref(),
        &formatted.diagnostics,
        entry_name,
        source,
    )
}

/// Compiles `source` to a bytecode container (chapter 12's `load`, without
/// the run: check, compile and verify, but do not initialise), against the
/// same capability interface, modules and filter schema [`check`] uses (a
/// program [`check`] refuses for a filter literal is refused here too). On
/// success, the bytes are exactly what `hollowscript_bytecode::verify::verify_bytes` reads and any
/// VM of this version runs, including the VM-only build this crate's
/// default, feature-less build already is: an editor page can go from
/// source to a running program with `compile` then
/// `crate::abi::hs_wasm_load`, with no server and no CLI in between.
///
/// # Errors
///
/// The diagnostics of a program with an error, as the same JSON array
/// [`check`] answers, rather than bytes.
pub fn compile(
    source: &str,
    entry_name: &str,
    extra_capabilities_json: &str,
    modules_json: &str,
    filter_schema_json: &str,
) -> Result<Vec<u8>, String> {
    let Inputs {
        capabilities,
        mut loader,
        filter_schema,
    } = match inputs(extra_capabilities_json, modules_json, filter_schema_json) {
        Ok(inputs) => inputs,
        Err(reason) => {
            return Err(diagnostics_to_json(
                &[malformed_input_diagnostic(&reason)],
                entry_name,
                source,
                &BTreeMap::new(),
            ));
        }
    };
    let options = Options {
        capabilities,
        filter_schema,
        ..Options::default()
    };
    match build(
        source,
        entry_name,
        &options,
        &mut loader,
        &mut Control::none(),
    ) {
        BuildOutcome::Bytecode(bytes) => Ok(bytes),
        BuildOutcome::CheckFailed(diagnostics) => Err(diagnostics_to_json(
            &diagnostics,
            entry_name,
            source,
            &loader.modules,
        )),
    }
}

/// Every filter literal of `source` and of the modules it imports, as a JSON
/// array in source order (the order of [`hollowscript::filter_literals`]):
/// `[{"source":"status:open","file":"main.hws","line":3,"column":14,
/// "spanStartUtf16":41,"spanEndUtf16":54}, ...]`. `source` is the filter's
/// own text, with `` \` `` and `\\` unescaped, which is exactly what a
/// capability that is given the filter receives as `{"filter": "..."}`, so a
/// host can key what it evaluates by it. The span is of the whole literal,
/// backticks included, in UTF-16 code units, and `line` and `column` are as a
/// diagnostic's are; `file` is the entry name or the canonical path of a
/// module in `modules_json`. A literal that appears twice is listed twice,
/// at its two places; a host that wants each text once removes the repeats.
///
/// The program need not check cleanly, and the literals are listed whether or
/// not they are valid (use [`check`] for that). `extra_capabilities_json` and
/// `modules_json` are as [`check`]'s.
///
/// # Errors
///
/// The reason, as plain text, if `extra_capabilities_json` or `modules_json`
/// is malformed.
pub fn filter_literals(
    source: &str,
    entry_name: &str,
    extra_capabilities_json: &str,
    modules_json: &str,
) -> Result<String, String> {
    let Inputs {
        capabilities,
        mut loader,
        ..
    } = inputs(extra_capabilities_json, modules_json, "")?;
    let found = hollowscript::filter_literals(
        source,
        entry_name,
        &capabilities,
        &mut loader,
        &mut Control::none(),
    );
    let mut out = String::from("[");
    for (i, literal) in found.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(r#"{"source":""#);
        escape_json_string(&literal.source, &mut out);
        out.push_str("\",");
        push_location_fields(
            &mut out,
            Some(&literal.location),
            entry_name,
            source,
            &loader.modules,
        );
        out.push('}');
    }
    out.push(']');
    Ok(out)
}

/// A diagnostic reporting that an argument (the capability declaration, the
/// modules or the filter schema) could not be read, in the same family as an
/// internal error (HS1099): this is a defect in what the host passed in, not
/// in `source`. `reason` names the argument.
fn malformed_input_diagnostic(reason: &str) -> Diagnostic {
    Diagnostic::not_implemented(&format!("the request is malformed: {reason}"))
}

/// A minimal, dependency-free JSON array of diagnostics; see
/// `hollowscript-capi::diagnostics_to_json`, which this mirrors exactly.
fn diagnostics_to_json(
    diagnostics: &[Diagnostic],
    entry_name: &str,
    entry_source: &str,
    modules: &BTreeMap<String, String>,
) -> String {
    let mut out = String::from("[");
    for (i, d) in diagnostics.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        push_diagnostic(&mut out, d, entry_name, entry_source, modules);
    }
    out.push(']');
    out
}

/// `file`'s own source text: `entry_source` if `file` is `entry_name`,
/// otherwise a lookup in `modules` (the same two sources
/// [`push_diagnostic`]'s own caller already has in hand); `None` if `file`
/// names neither (an internal error naming a file this build never saw,
/// which should not happen).
fn source_of<'a>(
    file: &str,
    entry_name: &str,
    entry_source: &'a str,
    modules: &'a BTreeMap<String, String>,
) -> Option<&'a str> {
    if file == entry_name {
        Some(entry_source)
    } else {
        modules.get(file).map(String::as_str)
    }
}

/// Appends one diagnostic object to `out`: `severity`, `code`, `message`,
/// and, when `d.location` is `Some` (every diagnostic except the host
/// errors HS1201 to HS1205, which answer a host operation, not a position
/// in a program), `file`, `line`, 1-based `column` in code points (as the
/// specification itself defines it), `spanStartUtf16`/`spanEndUtf16` (the
/// half-open byte span converted to UTF-16 code units, which is what a web
/// text editor such as Monaco counts an offset or a column in, stated
/// explicitly by these two field names so a caller never mistakes them for
/// `column`'s own code-point count) and `related` (further locations,
/// [`Diagnostic::related`], the same shape one level down, without its own
/// `related`). A location field is `null` when there is no location, or
/// when [`source_of`] cannot find the named file's own source text to
/// convert its span against.
fn push_diagnostic(
    out: &mut String,
    d: &Diagnostic,
    entry_name: &str,
    entry_source: &str,
    modules: &BTreeMap<String, String>,
) {
    out.push_str(r#"{"severity":""#);
    out.push_str(d.severity.as_str());
    out.push_str(r#"","code":""#);
    out.push_str(&d.code.to_text());
    out.push_str(r#"","message":""#);
    escape_json_string(&d.message, out);
    out.push_str("\",");
    push_location_fields(out, d.location.as_ref(), entry_name, entry_source, modules);
    out.push_str(r#","related":["#);
    for (i, r) in d.related.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('{');
        push_location_fields(out, Some(&r.location), entry_name, entry_source, modules);
        out.push_str(r#","message":""#);
        escape_json_string(&r.message, out);
        out.push('"');
        out.push('}');
    }
    out.push_str("]}");
}

/// Appends `"file":...,"line":...,"column":...,"spanStartUtf16":...,"spanEndUtf16":...`
/// (no leading comma, `{` or trailing `}`, so [`push_diagnostic`] can use this for
/// both a diagnostic's own location and each of its `related` ones) for
/// `location`, or the same keys with `null` values when it is `None`.
fn push_location_fields(
    out: &mut String,
    location: Option<&hollowscript_core::Location>,
    entry_name: &str,
    entry_source: &str,
    modules: &BTreeMap<String, String>,
) {
    use std::fmt::Write as _;
    let Some(location) = location else {
        out.push_str(
            r#""file":null,"line":null,"column":null,"spanStartUtf16":null,"spanEndUtf16":null"#,
        );
        return;
    };
    out.push_str(r#""file":""#);
    escape_json_string(&location.file, out);
    out.push_str(r#"","line":"#);
    let _ = write!(out, "{}", location.line);
    out.push_str(r#","column":"#);
    let _ = write!(out, "{}", location.column);
    match source_of(&location.file, entry_name, entry_source, modules) {
        Some(source) => {
            let start = utf16_offset(source, location.span.start);
            let end = utf16_offset(source, location.span.end);
            let _ = write!(out, r#","spanStartUtf16":{start},"spanEndUtf16":{end}"#);
        }
        None => out.push_str(r#","spanStartUtf16":null,"spanEndUtf16":null"#),
    }
}

/// The number of UTF-16 code units the first `byte_offset` bytes of `text`
/// encode as. A web text editor (Monaco among them) counts a position in
/// UTF-16 code units, not bytes (`hollowscript_core::source::Span`'s own
/// unit) or code points (`Location::column`'s own unit), so a diagnostic's
/// byte span needs this conversion before a page can use it to place a
/// squiggle. `byte_offset` past `text`'s own length, or one that does not
/// fall on a `char` boundary (should not happen for a diagnostic's own
/// span, but this must not panic either way), is walked back to the
/// nearest boundary at or before it first.
fn utf16_offset(text: &str, byte_offset: u32) -> u32 {
    let mut boundary = (byte_offset as usize).min(text.len());
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    let Some(prefix) = text.get(..boundary) else {
        return 0;
    };
    let units: usize = prefix.chars().map(char::len_utf16).sum();
    u32::try_from(units).unwrap_or(u32::MAX)
}

/// A minimal, dependency-free JSON document for a formatted file; see
/// `hollowscript-capi::format_to_json`, which this mirrors exactly.
fn format_to_json(
    output: Option<&str>,
    diagnostics: &[Diagnostic],
    entry_name: &str,
    source: &str,
) -> String {
    let mut out = String::from(r#"{"output":"#);
    match output {
        None => out.push_str("null"),
        Some(text) => {
            out.push('[');
            for (i, line) in text.split('\n').enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('"');
                escape_json_string(line, &mut out);
                out.push('"');
            }
            out.push(']');
        }
    }
    out.push_str(r#","diagnostics":"#);
    out.push_str(&diagnostics_to_json(
        diagnostics,
        entry_name,
        source,
        &BTreeMap::new(),
    ));
    out.push('}');
    out
}

/// Appends the JSON string-literal encoding of `text` (without the
/// surrounding quotes) to `out`; see
/// `hollowscript-capi::escape_json_string`, which this mirrors exactly.
fn escape_json_string(text: &str, out: &mut String) {
    use std::fmt::Write as _;
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
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

#[cfg(test)]
mod tests {
    use super::{filter_literals, format};

    /// [`super::check`] with no filter schema, which is how most of these
    /// tests check.
    fn check(source: &str, entry_name: &str, capabilities: &str, modules: &str) -> String {
        super::check(source, entry_name, capabilities, modules, "")
    }

    /// [`super::compile`] with no filter schema.
    fn compile(
        source: &str,
        entry_name: &str,
        capabilities: &str,
        modules: &str,
    ) -> Result<Vec<u8>, String> {
        super::compile(source, entry_name, capabilities, modules, "")
    }

    #[test]
    fn check_reports_no_diagnostics_for_a_clean_program() {
        assert_eq!(check("return 1", "main.hws", "", ""), "[]");
    }

    #[test]
    fn check_reports_a_real_diagnostic_for_an_unresolved_name() {
        let json = check("return missing", "main.hws", "", "");
        assert!(json.contains(r#""code":"HS0205""#), "{json}");
    }

    /// A diagnostic's own JSON carries what the facade's `Diagnostic`
    /// itself has: file, line, column, the span converted to UTF-16 code
    /// units (stated explicitly, `spanStartUtf16`/`spanEndUtf16`, since
    /// Monaco and every other web text editor counts a position that way,
    /// not in bytes or in code points), severity, code, message and
    /// `related` (empty here; HS0205 names none). The span converts through
    /// a multi-byte character (an emoji, four UTF-8 bytes and two UTF-16
    /// code units) earlier on the line, so a byte-for-UTF-16 mixup would
    /// show up as a wrong offset here, not merely a missing field.
    #[test]
    fn check_reports_the_full_location_of_a_diagnostic() {
        let source = "return \"\u{1f600}\" == missing\n";
        let json = check(source, "main.hws", "", "");
        assert!(json.contains(r#""code":"HS0205""#), "{json}");
        assert!(json.contains(r#""file":"main.hws""#), "{json}");
        assert!(json.contains(r#""line":1"#), "{json}");
        assert!(json.contains(r#""column":"#), "{json}");
        assert!(json.contains(r#""spanStartUtf16":"#), "{json}");
        assert!(json.contains(r#""spanEndUtf16":"#), "{json}");
        // `return "` is 8 bytes/UTF-16 units, the emoji is 4 bytes but 2
        // UTF-16 units (a surrogate pair), then `" == ` is 5 more: `missing`
        // starts at UTF-16 offset 8 + 2 + 5 = 15, not the byte offset 17 a
        // byte-for-UTF-16 mixup would report instead.
        assert!(json.contains(r#""spanStartUtf16":15"#), "{json}");
        assert!(json.contains(r#""related":[]"#), "{json}");
    }

    /// A diagnostic with a related location (a repeated declaration points
    /// back at the first one) must still be well-formed JSON: each `related`
    /// object once began with a stray comma, `{,"file"`, which no JSON
    /// parser accepts.
    #[test]
    fn check_writes_well_formed_json_for_a_related_location() {
        let source = "fn twice(): Int { return 1 }\nfn twice(): Int { return 2 }\n";
        let json = check(source, "main.hws", "", "");
        assert!(json.contains(r#""code":"HS0201""#), "{json}");
        assert!(json.contains(r#""related":[{"file":"main.hws""#), "{json}");
        assert!(!json.contains("{,"), "{json}");
        assert!(!json.contains(",,"), "{json}");
    }

    /// The bug the Varnook integration found: `uses print` (or `clock`,
    /// `random`) must check with no declaration at all, the same as the CLI
    /// with no `--allow` and a plain `hollowscript-capi` run with no
    /// `HsCapabilityFn`.
    #[test]
    fn check_accepts_every_standard_capability_with_no_declaration() {
        let source = concat!(
            "uses print\n",
            "uses clock\n",
            "uses random\n",
            "effect print(\"${effect clock.now()}\")\n",
            "effect print(\"${effect random.float()}\")\n",
        );
        assert_eq!(
            check(source, "main.hws", "", ""),
            "[]",
            "{}",
            check(source, "main.hws", "", "")
        );
    }

    /// A host-defined capability, declared through `extra_capabilities_json`
    /// as the JS wrapper builds it, checks; without the declaration the
    /// same source is refused with HS0404, not panicked on.
    #[test]
    fn check_accepts_a_declared_host_capability_alongside_the_standard_ones() {
        let source = "uses tasks\nreturn effect tasks(\"x\")\n";
        let undeclared = check(source, "main.hws", "", "");
        assert!(undeclared.contains("HS0404"), "{undeclared}");

        let declared = check(
            source,
            "main.hws",
            r#"[{"name":"tasks","params":["String"],"result":"String"}]"#,
            "",
        );
        assert_eq!(declared, "[]", "{declared}");
    }

    /// A namespace capability (several functions sharing one name) and a
    /// capability declared `Result<T, E>` both declare and check.
    #[test]
    fn check_accepts_a_declared_namespace_and_a_result_typed_capability() {
        let source = concat!(
            "uses tasks\n",
            "export fn run(): String {\n",
            "    match effect tasks.get(\"x\") {\n",
            "        Ok(value) { return value }\n",
            "        Err(reason) { return reason }\n",
            "    }\n",
            "}\n",
            "export fn count(): Int {\n",
            "    return effect tasks.count()\n",
            "}\n",
        );
        let declared = check(
            source,
            "main.hws",
            concat!(
                r#"[{"name":"tasks","function":"get","params":["String"],"#,
                r#""result":{"ok":"String","err":"String"}},"#,
                r#"{"name":"tasks","function":"count","result":"Int"}]"#,
            ),
            "",
        );
        assert_eq!(declared, "[]", "{declared}");
    }

    /// A capability declaring a record parameter or result type (Studio's
    /// `entities.get`, cited by the Studio integration: it returns a
    /// record, which the four-scalar-only `params`/`result` this boundary
    /// used to take could not express, HS0320): `{"record": {"field": T}}`
    /// declares one, the same tag the value encoding itself uses for a
    /// record value.
    #[test]
    fn check_accepts_a_capability_declaring_a_record_parameter_and_result() {
        let source = concat!(
            "uses entities\n",
            "export fn run(): Int {\n",
            "    constant e = effect entities.get(\"player-1\")\n",
            "    return effect entities.set(e)\n",
            "}\n",
        );
        let declared = check(
            source,
            "main.hws",
            concat!(
                r#"[{"name":"entities","function":"get","params":["String"],"#,
                r#""result":{"record":{"id":"String","x":"Float","y":"Float"}}},"#,
                r#"{"name":"entities","function":"set","#,
                r#""params":[{"record":{"id":"String","x":"Float","y":"Float"}}],"result":"Int"}]"#,
            ),
            "",
        );
        assert_eq!(declared, "[]", "{declared}");
    }

    /// A capability declaring an enum, and one declaring `[Int]` (a list):
    /// the two container kinds `{"record": ...}` above does not cover on
    /// its own.
    #[test]
    fn check_accepts_a_capability_declaring_an_enum_and_a_list() {
        let source = concat!(
            "uses input\n",
            "export fn run(): Int {\n",
            "    effect input.moveAxis([1, 2, 3])\n",
            "    match effect input.state() {\n",
            "        Idle { return 0 }\n",
            "        Moving(n) { return n }\n",
            "    }\n",
            "}\n",
        );
        let declared = check(
            source,
            "main.hws",
            concat!(
                r#"[{"name":"input","function":"moveAxis","params":[{"list":"Int"}]},"#,
                r#"{"name":"input","function":"state","#,
                r#""result":{"enum":{"name":"State","variants":{"Idle":[],"Moving":["Int"]}}}}]"#,
            ),
            "",
        );
        assert_eq!(declared, "[]", "{declared}");
    }

    /// Malformed JSON for the capability declaration is reported as a
    /// diagnostic, not panicked on or silently ignored.
    #[test]
    fn check_reports_a_diagnostic_for_malformed_extra_capabilities() {
        let json = check("return 1", "main.hws", "not json", "");
        assert!(json.contains("HS1099"), "{json}");
    }

    /// A two-file program: the entry module imports a second one by its
    /// canonical path, which `modules_json` supplies (section 10.3). Before
    /// this loader existed, any import at all was "not found" (HS0601),
    /// which a Code workspace of more than one file cannot check or compile
    /// against.
    #[test]
    fn check_resolves_an_import_against_the_supplied_modules() {
        let source = "import double from \"./lib/helpers\"\nreturn double(3)\n";
        let without_modules = check(source, "main.hws", "", "");
        assert!(without_modules.contains("HS0601"), "{without_modules}");

        let modules =
            r#"{"lib/helpers.hws": "export fn double(n: Int): Int {\n    return n * 2\n}\n"}"#;
        let with_modules = check(source, "main.hws", "", modules);
        assert_eq!(with_modules, "[]", "{with_modules}");
    }

    /// Malformed JSON for `modules_json` is reported as a diagnostic too,
    /// the same way malformed capabilities are.
    #[test]
    fn check_reports_a_diagnostic_for_malformed_modules() {
        let json = check("return 1", "main.hws", "", "not json");
        assert!(json.contains("HS1099"), "{json}");
    }

    #[test]
    fn format_formats_a_clean_source_with_no_checker_needed() {
        let json = format("constant x=1", "main.hws");
        assert!(json.contains(r#""output":["constant x = 1",""]"#), "{json}");
        assert!(json.contains(r#""diagnostics":[]"#), "{json}");
    }

    #[test]
    fn compile_answers_bytes_that_verify_bytes_reads() {
        let bytes = compile("export fn one(): Int { return 1 }", "main.hws", "", "").unwrap();
        let verified = hollowscript_bytecode::verify::verify_bytes(&bytes).unwrap();
        assert!(
            verified
                .module()
                .exports
                .iter()
                .any(|e| verified.module().string(e.name) == "one")
        );
    }

    /// The same fix as `check`'s: the standard capabilities compile with no
    /// declaration.
    #[test]
    fn compile_accepts_the_standard_capabilities_with_no_declaration() {
        let source = "uses print\neffect print(\"hello\")\n";
        let bytes = compile(source, "main.hws", "", "").unwrap();
        assert!(!bytes.is_empty());
    }

    #[test]
    fn compile_answers_diagnostics_for_a_program_with_an_error() {
        let error = compile("return missing", "main.hws", "", "").unwrap_err();
        assert!(error.contains("HS0205"), "{error}");
    }

    /// `compile` resolves an import against `modules_json` too, and the
    /// resulting bytecode actually runs, proving the imported module's own
    /// code made it into the container, not only that checking passed.
    #[test]
    fn compile_resolves_an_import_against_the_supplied_modules() {
        let source = "import double from \"./lib/helpers\"\nexport fn run(): Int {\n    return double(3)\n}\n";
        let modules =
            r#"{"lib/helpers.hws": "export fn double(n: Int): Int {\n    return n * 2\n}\n"}"#;
        let bytes = compile(source, "main.hws", "", modules).unwrap();
        let verified = hollowscript_bytecode::verify::verify_bytes(&bytes).unwrap();
        let mut module =
            crate::WasmModule::load(&bytes, hollowscript_vm::CapabilityTable::new(vec![])).unwrap();
        module
            .initialise(
                &hollowscript_core::Limits::default(),
                &mut hollowscript_core::Control::none(),
            )
            .unwrap();
        let index = module.export_index("run").expect("run is exported");
        let result = module
            .call_export(
                index,
                &[],
                &hollowscript_core::Limits::default(),
                &mut hollowscript_core::Control::none(),
            )
            .unwrap();
        assert_eq!(
            module.render(result.expect("run returns a value")),
            "Int(6)"
        );
        let _ = verified;
    }

    // ---- filter literals and the filter schema ----------------------------

    /// A host that offers a `tasks` capability taking a `Filter`, and a schema
    /// for the records it filters.
    const TASKS: &str = r#"[{"name":"tasks","params":["Filter"],"result":"Int"}]"#;
    const SCHEMA: &str = r#"{"fields": {
        "status": {"type": "enum", "values": ["open", "done"], "aliases": {"closed": "done"}},
        "title": {"type": "text"},
        "due": {"type": "date"},
        "points": {"type": "number"},
        "starred": {"type": "boolean"},
        "label": {"type": "textMulti"}
    }}"#;

    fn tasks_program(filter: &str) -> String {
        format!("uses tasks\nreturn effect tasks(`{filter}`)\n")
    }

    /// Without a schema a literal is checked for syntax only, so a field the
    /// host does not have is accepted; with one it is HS0708, at check time.
    #[test]
    fn a_filter_schema_rejects_a_field_the_host_does_not_have() {
        let source = tasks_program("colour:red");
        assert_eq!(check(&source, "main.hws", TASKS, ""), "[]");
        let json = super::check(&source, "main.hws", TASKS, "", SCHEMA);
        assert!(json.contains(r#""code":"HS0708""#), "{json}");
        // The diagnostic is placed on the field, inside the literal.
        assert!(json.contains(r#""line":2"#), "{json}");
    }

    /// Every rule of section A.7 reaches the host: an operator the field does
    /// not allow, a value that does not suit it, an enum value that is not in
    /// its list (but an alias of one is), and a literal that is valid.
    #[test]
    fn a_filter_schema_applies_every_rule_of_section_a7() {
        let check_with_schema =
            |filter: &str| super::check(&tasks_program(filter), "main.hws", TASKS, "", SCHEMA);
        for valid in [
            "status:open",
            "status:closed",
            "status>=open",
            "title:fix points>3 starred:yes label:bug",
            "due<today+7 OR -starred",
            "status:none",
            "plain words",
        ] {
            assert_eq!(check_with_schema(valid), "[]", "{valid}");
        }
        for (invalid, code) in [
            ("colour:red", "HS0708"),
            ("title>x", "HS0707"),
            ("starred<yes", "HS0707"),
            ("status:blocked", "HS0709"),
            ("points:many", "HS0709"),
            ("starred:ture", "HS0709"),
            ("due:tomorrowish", "HS0709"),
        ] {
            let json = check_with_schema(invalid);
            assert!(
                json.contains(&format!(r#""code":"{code}""#)),
                "{invalid}: {json}"
            );
        }
    }

    /// `compile` refuses what `check` refuses, so a program that names a
    /// field the host does not have never becomes bytecode.
    #[test]
    fn compile_applies_the_filter_schema_too() {
        let source = tasks_program("colour:red");
        assert!(super::compile(&source, "main.hws", TASKS, "", "").is_ok());
        let refused = super::compile(&source, "main.hws", TASKS, "", SCHEMA).unwrap_err();
        assert!(refused.contains("HS0708"), "{refused}");
        let accepted = super::compile(&tasks_program("status:open"), "main.hws", TASKS, "", SCHEMA);
        assert!(accepted.is_ok(), "{accepted:?}");
    }

    /// An empty schema is a schema: every field is unknown. No schema at all
    /// (the empty string) is not.
    #[test]
    fn an_empty_filter_schema_is_not_the_absence_of_one() {
        let source = tasks_program("status:open");
        assert_eq!(super::check(&source, "main.hws", TASKS, "", ""), "[]");
        let json = super::check(&source, "main.hws", TASKS, "", r#"{"fields": {}}"#);
        assert!(json.contains("HS0708"), "{json}");
    }

    /// A schema that is silently read as something looser would accept what
    /// the host meant to refuse, so every malformed one is a diagnostic that
    /// says which part.
    #[test]
    fn a_malformed_filter_schema_is_a_diagnostic_that_names_the_part() {
        let cases = [
            ("not json", "is not JSON"),
            ("[]", "must be a JSON object"),
            (r#"{"field": {}}"#, "is not a member of a filter schema"),
            (r#"{"fields": {}, "extra": 1}"#, "exactly one member"),
            (r#"{"fields": []}"#, "must be a JSON object"),
            (r#"{"fields": {"Status": {"type": "text"}}}"#, "lower case"),
            (r#"{"fields": {"status": "text"}}"#, "must be a JSON object"),
            (r#"{"fields": {"status": {}}}"#, "'status' needs a"),
            (
                r#"{"fields": {"status": {"type": "string"}}}"#,
                "not a filter field type",
            ),
            (
                r#"{"fields": {"status": {"type": "text", "value": []}}}"#,
                "not a valid member",
            ),
            (
                r#"{"fields": {"status": {"type": "text", "values": ["a"]}}}"#,
                "not an enum",
            ),
            (
                r#"{"fields": {"status": {"type": "enum", "values": [1]}}}"#,
                "must be strings",
            ),
            (
                r#"{"fields": {"status": {"type": "enum", "aliases": {"Done": "done"}}}}"#,
                "must be lower case",
            ),
            (
                r#"{"fields": {"status": {"type": "enum", "aliases": {"done": 1}}}}"#,
                "must map to a string",
            ),
        ];
        for (schema, reason) in cases {
            let json = super::check("return 1", "main.hws", "", "", schema);
            assert!(json.contains("HS1099"), "{schema}: {json}");
            assert!(json.contains(reason), "{schema}: {json}");
            assert!(
                super::compile("return 1", "main.hws", "", "", schema)
                    .unwrap_err()
                    .contains(reason),
                "{schema}"
            );
        }
    }

    /// Every literal comes back with its decoded source, in source order, with
    /// the span of the whole literal in UTF-16 code units: the first is after
    /// an emoji on its line, which is four UTF-8 bytes and two UTF-16 code
    /// units, so a byte offset would be wrong here.
    #[test]
    fn filter_literals_lists_every_literal_with_its_utf16_span() {
        let source = concat!(
            "uses tasks\n",
            "constant n = \"\u{1f600}\".length() + effect tasks(`status:open`)\n",
            "constant second = `title:\"a\\`b\"`\n",
            "return n\n",
        );
        let json = filter_literals(source, "main.hws", TASKS, "").unwrap();
        let literal = "`status:open`";
        let byte_start = source.find(literal).unwrap();
        let utf16_start = source[..byte_start].encode_utf16().count();
        assert_ne!(byte_start, utf16_start, "the emoji must matter");
        assert!(
            json.starts_with(&format!(
                r#"[{{"source":"status:open","file":"main.hws","line":2,"column":{},"spanStartUtf16":{utf16_start},"spanEndUtf16":{}}}"#,
                // The column counts code points: the emoji is one.
                source[..byte_start].lines().last().unwrap() .chars().count() + 1,
                utf16_start + literal.len(),
            )),
            "{json}"
        );
        // The decoded text has the backtick unescaped, JSON-escaped in turn.
        assert!(
            json.contains(r#"{"source":"title:\"a`b\"","file":"main.hws","line":3"#),
            "{json}"
        );
        assert_eq!(json.matches(r#""source":"#).count(), 2, "{json}");
    }

    /// A program with no filter literal answers an empty array, and one that
    /// does not check at all still lists what it has.
    #[test]
    fn filter_literals_answers_what_it_finds_in_any_program() {
        assert_eq!(
            filter_literals("return 1\n", "main.hws", "", "").unwrap(),
            "[]"
        );
        // `tasks` is not declared (HS0404) and `missing` is not a name, yet
        // the literal is there to read.
        let json = filter_literals(
            "uses tasks\nconstant a = `status:open`\nreturn missing\n",
            "main.hws",
            "",
            "",
        )
        .unwrap();
        assert!(json.contains(r#""source":"status:open""#), "{json}");
    }

    /// The literals of an imported module are listed under its own file, with
    /// the span counted in that file's own text.
    #[test]
    fn filter_literals_reaches_the_modules_a_program_imports() {
        let source = "import queries from \"./lib/queries\"\nreturn queries\n";
        let modules = r#"{"lib/queries.hws": "// é\nexport constant queries = `status:open`\n"}"#;
        let json = filter_literals(source, "main.hws", "", modules).unwrap();
        assert!(
            json.contains(
                r#"{"source":"status:open","file":"lib/queries.hws","line":2,"column":27"#
            ),
            "{json}"
        );
        // The `é` is one UTF-16 unit but two bytes: the span is in units.
        assert!(
            json.contains(r#""spanStartUtf16":31,"spanEndUtf16":44}"#),
            "{json}"
        );
    }

    /// A malformed argument is the reason, not a panic and not an empty array
    /// that looks like "no literals".
    #[test]
    fn filter_literals_refuses_a_malformed_argument() {
        let error = filter_literals("return 1\n", "main.hws", "not json", "").unwrap_err();
        assert!(error.contains("capabilities"), "{error}");
        let error = filter_literals("return 1\n", "main.hws", "", "not json").unwrap_err();
        assert!(error.contains("modules"), "{error}");
    }
}

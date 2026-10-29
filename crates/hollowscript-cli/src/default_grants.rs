//! The CLI's capability declarations and bindings (section 9.5's CLI policy
//! and chapter 16).
//!
//! [`capabilities_for`] builds the checker's `CapabilityInterface`, the type
//! signatures `check` needs; [`bind_capability`] builds the matching runtime
//! functions, which `load` asks for one `(capability, function)` pair at a
//! time in the order the compiled module's own `cap_calls` table assigns
//! them. The two must declare and bind the same capabilities, in the same
//! shapes; a mismatch here is a defect in this file, not something the
//! checker or the VM can catch, so
//! `every_declared_capability_function_is_bound` in this module's own tests
//! compares the two directly.
//!
//! `print`, `clock` and `random` are granted to every run; `env`, `args`,
//! `fs`, `stdin` and `http` when `--allow` asks for them.

use crate::capabilities::IoErrorKind;
use crate::granted::Granted;
use crate::http::{self, HttpErrorKind};
use crate::random::Rng;
use hollowscript::{
    CapabilityDecl, CapabilityInterface, CapabilityShape, Fault, HostCall, HostFn, MapEntry,
    MapKey, Object, Value,
};
use hollowscript_core::transfer::{TransferType, TypeRef, TypeTable, VariantDef};
use std::rc::Rc;

/// The name of the language's own error enum for input and output
/// (section 16.1).
const IO_ERROR: &str = "IoError";

/// Its variants, in the order section 16.1 declares them, which is the order
/// a `match` on one compares by.
const IO_ERROR_VARIANTS: [&str; 5] = ["NotFound", "Denied", "AlreadyExists", "Invalid", "Failed"];

/// The name of the language's own error enum for `http` (section 16.1).
const HTTP_ERROR: &str = "HttpError";

/// Its variants, in the order section 16.1 declares them.
const HTTP_ERROR_VARIANTS: [&str; 5] = ["Denied", "Invalid", "Failed", "Timeout", "TooLarge"];

/// The capabilities one CLI run grants, matching what [`bind_capability`]
/// binds.
#[must_use]
pub(crate) fn capabilities_for(granted: &Granted) -> CapabilityInterface {
    let mut types = TypeTable::new();
    let string_ty = types.add(TransferType::String);
    let int_ty = types.add(TransferType::Int);
    let float_ty = types.add(TransferType::Float);
    let bool_ty = types.add(TransferType::Bool);
    let optional_string = types.add(TransferType::Optional(string_ty));
    let string_list = types.add(TransferType::List(string_ty));

    let mut capabilities = vec![
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
    ];
    if granted.env {
        capabilities.push(CapabilityDecl {
            name: "env".to_owned(),
            shape: CapabilityShape::Function {
                params: vec![string_ty],
                ret: Some(optional_string),
            },
        });
    }
    if granted.args {
        capabilities.push(CapabilityDecl {
            name: "args".to_owned(),
            shape: CapabilityShape::Function {
                params: vec![],
                ret: Some(string_list),
            },
        });
    }
    if granted.fs() || granted.stdin {
        let io_error = io_error_type(&mut types, string_ty);
        if granted.fs() {
            let read = result_of(&mut types, string_ty, io_error);
            let count = result_of(&mut types, int_ty, io_error);
            let exists = result_of(&mut types, bool_ty, io_error);
            let names = result_of(&mut types, string_list, io_error);
            capabilities.push(CapabilityDecl {
                name: "fs".to_owned(),
                shape: CapabilityShape::Namespace(vec![
                    ("read".to_owned(), vec![string_ty], Some(read)),
                    ("write".to_owned(), vec![string_ty, string_ty], Some(count)),
                    ("append".to_owned(), vec![string_ty, string_ty], Some(count)),
                    ("exists".to_owned(), vec![string_ty], Some(exists)),
                    ("list".to_owned(), vec![string_ty], Some(names)),
                    ("remove".to_owned(), vec![string_ty], Some(exists)),
                    ("mkdir".to_owned(), vec![string_ty], Some(exists)),
                ]),
            });
        }
        if granted.stdin {
            let line = result_of(&mut types, optional_string, io_error);
            let all = result_of(&mut types, string_ty, io_error);
            capabilities.push(CapabilityDecl {
                name: "stdin".to_owned(),
                shape: CapabilityShape::Namespace(vec![
                    ("readLine".to_owned(), vec![], Some(line)),
                    ("readAll".to_owned(), vec![], Some(all)),
                ]),
            });
        }
    }
    if !granted.http.is_empty() {
        let http_error = http_error_type(&mut types, string_ty);
        let headers = types.add(TransferType::Map(string_ty, string_ty));
        // Field lists are given already sorted by name, matching the order
        // `HostCall::record` builds them in.
        let http_request = types.add(TransferType::Record(vec![
            ("body".to_owned(), optional_string),
            ("headers".to_owned(), headers),
            ("method".to_owned(), string_ty),
            ("url".to_owned(), string_ty),
        ]));
        let http_response = types.add(TransferType::Record(vec![
            ("body".to_owned(), string_ty),
            ("headers".to_owned(), headers),
            ("status".to_owned(), int_ty),
        ]));
        let get_result = result_of(&mut types, http_response, http_error);
        let request_result = result_of(&mut types, http_response, http_error);
        capabilities.push(CapabilityDecl {
            name: "http".to_owned(),
            shape: CapabilityShape::Namespace(vec![
                ("get".to_owned(), vec![string_ty], Some(get_result)),
                (
                    "request".to_owned(),
                    vec![http_request],
                    Some(request_result),
                ),
            ]),
        });
    }
    CapabilityInterface {
        capabilities,
        types,
    }
}

/// `HttpError` as a transferable type (section 16.1).
fn http_error_type(types: &mut TypeTable, string_ty: TypeRef) -> TypeRef {
    types.add_enum(
        HTTP_ERROR,
        HTTP_ERROR_VARIANTS
            .iter()
            .map(|name| VariantDef {
                name: (*name).to_owned(),
                payload: vec![string_ty],
            })
            .collect(),
    )
}

/// `IoError` as a transferable type (section 16.1).
fn io_error_type(types: &mut TypeTable, string_ty: TypeRef) -> TypeRef {
    types.add_enum(
        IO_ERROR,
        IO_ERROR_VARIANTS
            .iter()
            .map(|name| VariantDef {
                name: (*name).to_owned(),
                payload: vec![string_ty],
            })
            .collect(),
    )
}

/// `Result<ok, err>` as a transferable type (section 8.2).
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
            VariantDef {
                name: hollowscript::OK.to_owned(),
                payload: vec![ok],
            },
            VariantDef {
                name: hollowscript::ERR.to_owned(),
                payload: vec![err],
            },
        ],
    })
}

/// Where `print` sends each line.
#[derive(Debug)]
pub(crate) enum PrintSink {
    /// Kept, in order, for the caller to read once the run ends: what
    /// `run --json` reports in its document (so program output never
    /// interleaves with the JSON), and what the tests read.
    Collect(std::cell::RefCell<Vec<String>>),
    /// Written straight to standard output as each `print` happens, so a
    /// question printed before the program reads standard input is seen
    /// first, and a long loop shows its progress. What plain `run` uses.
    Stream,
}

impl PrintSink {
    /// A sink that keeps every line.
    pub(crate) fn collect() -> Output {
        Rc::new(PrintSink::Collect(std::cell::RefCell::new(Vec::new())))
    }

    /// A sink that writes every line to standard output as it happens.
    pub(crate) fn stream() -> Output {
        Rc::new(PrintSink::Stream)
    }

    /// The lines kept so far: empty for [`PrintSink::Stream`], which keeps
    /// nothing.
    pub(crate) fn lines(&self) -> Vec<String> {
        match self {
            PrintSink::Collect(lines) => lines.borrow().clone(),
            PrintSink::Stream => Vec::new(),
        }
    }
}

/// The sink a run's `print` writes to.
pub(crate) type Output = Rc<PrintSink>;

/// Resolves one `(capability, function)` pair `load` asks for to the real
/// implementation. Returns `None` for anything [`capabilities_for`] does not
/// declare, which `load` then reports as a host defect rather than a panic.
///
/// `print` sends each line to `output`: kept for `run --json`'s document, or
/// written to standard output as it happens (see [`PrintSink`]).
#[allow(clippy::too_many_lines, reason = "one arm per capability function")]
pub(crate) fn bind_capability(
    name: &str,
    function: Option<&str>,
    output: &Output,
    granted: &Rc<Granted>,
) -> Option<Box<dyn HostFn>> {
    match (name, function) {
        ("print", None) => {
            let output = Rc::clone(output);
            Some(Box::new(
                move |args: &[Value], call: &mut HostCall<'_>| match args {
                    [value] => {
                        let Some(text) = call.str_of(*value) else {
                            return Err(Fault::invalid_argument("print", "expected one String"));
                        };
                        let text = text.to_owned();
                        // Section 11.6: the text's UTF-8 bytes and one LF,
                        // charged before anything is written, because a call
                        // that would pass the limit writes nothing at all.
                        let written = u64::try_from(text.len()).unwrap_or(u64::MAX);
                        call.write_output(written.saturating_add(1))?;
                        match &*output {
                            PrintSink::Collect(lines) => lines.borrow_mut().push(text),
                            // Standard output is closed or broken: nothing
                            // more this run prints can be seen, so it stops
                            // here. `main` reads `stdio::problem` and says
                            // nothing for a closed pipe, which is how a
                            // reader such as `head` says it has seen enough.
                            PrintSink::Stream => {
                                if !crate::stdio::line(&text) {
                                    return Err(Fault::cancelled());
                                }
                            }
                        }
                        // `print` declares no result (section 16.2).
                        Ok(Value::None)
                    }
                    _ => Err(Fault::invalid_argument("print", "expected one String")),
                },
            ))
        }
        ("clock", Some("now")) => Some(Box::new(|_: &[Value], call: &mut HostCall<'_>| {
            let text = crate::capabilities::clock_now();
            call.string(text)
        })),
        ("clock", Some("today")) => Some(Box::new(|_: &[Value], call: &mut HostCall<'_>| {
            let text = crate::capabilities::clock_today();
            call.string(text)
        })),
        ("clock", Some("unixMillis")) => Some(Box::new(|_: &[Value], _: &mut HostCall<'_>| {
            Ok(Value::Int(crate::capabilities::unix_millis()))
        })),
        ("clock", Some("monotonicMillis")) => {
            let origin = std::time::Instant::now();
            Some(Box::new(move |_: &[Value], _: &mut HostCall<'_>| {
                Ok(Value::Int(crate::capabilities::monotonic_millis(&origin)))
            }))
        }
        ("random", Some("int")) => {
            let mut rng = Rng::from_entropy();
            Some(Box::new(
                move |args: &[Value], _: &mut HostCall<'_>| match args {
                    [Value::Int(min), Value::Int(up_to)] => {
                        crate::capabilities::random_int(&mut rng, *min, *up_to)
                            .map(Value::Int)
                            .map_err(|reason| Fault::invalid_argument("random.int", reason))
                    }
                    _ => Err(Fault::invalid_argument("random.int", "expected two Int")),
                },
            ))
        }
        ("random", Some("float")) => {
            let mut rng = Rng::from_entropy();
            Some(Box::new(move |_: &[Value], _: &mut HostCall<'_>| {
                Ok(Value::Float(crate::capabilities::random_float(&mut rng)))
            }))
        }
        ("env", None) => {
            let granted = Rc::clone(granted);
            Some(Box::new(
                move |args: &[Value], call: &mut HostCall<'_>| match args {
                    [value] => {
                        let Some(wanted) = call.str_of(*value) else {
                            return Err(Fault::invalid_argument("env", "expected one String"));
                        };
                        let wanted = wanted.to_owned();
                        match granted.env_value(&wanted) {
                            None => Ok(Value::None),
                            Some(value) => call.string(value),
                        }
                    }
                    _ => Err(Fault::invalid_argument("env", "expected one String")),
                },
            ))
        }
        ("args", None) => {
            let granted = Rc::clone(granted);
            Some(Box::new(move |_: &[Value], call: &mut HostCall<'_>| {
                let mut items = Vec::with_capacity(granted.program_args.len());
                for word in &granted.program_args {
                    items.push(call.string(word.clone())?);
                }
                call.list(items)
            }))
        }
        ("fs", Some(operation)) => bind_fs(operation, granted),
        ("stdin", Some("readLine")) => Some(Box::new(|_: &[Value], call: &mut HostCall<'_>| {
            // Read through what is left of `maxMemoryBytes`, so a line that
            // would not fit is refused (HS1002) after that many bytes, not
            // after all of it is in memory.
            let outcome = crate::capabilities::stdin_read_line(
                &mut std::io::stdin().lock(),
                call.memory_remaining(),
            );
            match outcome {
                Ok(crate::capabilities::LineRead::End) => {
                    // `Ok(none)`: the input is exhausted. The optional's
                    // absent case is the value `none` itself.
                    call.ok(Value::None)
                }
                Ok(crate::capabilities::LineRead::Line(line)) => {
                    let text = call.string(line)?;
                    call.ok(text)
                }
                Ok(crate::capabilities::LineRead::TooLarge(size)) => too_large_to_read(call, size),
                Err(kind) => io_error(call, kind, "standard input"),
            }
        })),
        ("stdin", Some("readAll")) => Some(Box::new(|_: &[Value], call: &mut HostCall<'_>| {
            match crate::capabilities::stdin_read_all(
                &mut std::io::stdin().lock(),
                call.memory_remaining(),
            ) {
                Ok(crate::capabilities::TextRead::Text(text)) => {
                    let value = call.string(text)?;
                    call.ok(value)
                }
                Ok(crate::capabilities::TextRead::TooLarge(size)) => too_large_to_read(call, size),
                Err(kind) => io_error(call, kind, "standard input"),
            }
        })),
        ("http", Some("get")) => {
            let granted = Rc::clone(granted);
            Some(Box::new(move |args: &[Value], call: &mut HostCall<'_>| {
                let Some(url) = args.first().and_then(|v| call.str_of(*v)) else {
                    return Err(Fault::invalid_argument("http.get", "expected a String"));
                };
                let request = http::Request {
                    method: "GET".to_owned(),
                    url: url.to_owned(),
                    headers: Vec::new(),
                    body: None,
                };
                run_http(call, &request, &granted)
            }))
        }
        ("http", Some("request")) => {
            let granted = Rc::clone(granted);
            Some(Box::new(move |args: &[Value], call: &mut HostCall<'_>| {
                let Some(record) = args.first().copied() else {
                    return Err(Fault::invalid_argument(
                        "http.request",
                        "expected an HttpRequest",
                    ));
                };
                let request = read_http_request(call, record).ok_or_else(|| {
                    Fault::invalid_argument("http.request", "expected an HttpRequest")
                })?;
                run_http(call, &request, &granted)
            }))
        }
        _ => None,
    }
}

/// Performs `request` and turns the outcome into `Result<HttpResponse, HttpError>`.
fn run_http(
    call: &mut HostCall<'_>,
    request: &http::Request,
    granted: &Granted,
) -> Result<Value, Fault> {
    match http::perform(request, &granted.http, &granted.http_limits) {
        Ok(response) => {
            let headers = build_headers(call, &response.headers)?;
            let body = call.string(response.body)?;
            let record = call.record(vec![
                ("status", Value::Int(response.status)),
                ("headers", headers),
                ("body", body),
            ])?;
            call.ok(record)
        }
        Err(kind) => http_error(call, kind, &request.url),
    }
}

/// Reads an `HttpRequest` record's fields back out of the heap. `body`,
/// `headers`, `method` and `url` is the sorted field order `HostCall::record`
/// and the checker's own `Record` type agree on.
fn read_http_request(call: &mut HostCall<'_>, value: Value) -> Option<http::Request> {
    let fields = record_fields(call, value)?;
    let [body, headers, method, url] = fields.as_slice() else {
        return None;
    };
    let body = match *body {
        Value::None => None,
        text => Some(call.str_of(text)?.to_owned()),
    };
    let headers = read_headers(call, *headers)?;
    let method = call.str_of(*method)?.to_owned();
    let url = call.str_of(*url)?.to_owned();
    Some(http::Request {
        method,
        url,
        headers,
        body,
    })
}

/// A record's field values, in the sorted order `HostCall::record` builds
/// them in, read back out of the heap directly: there is no `HostCall`
/// accessor for this today because no other capability reads a record
/// argument, so this goes through the heap the same way `str_of` and
/// `list_of` do internally.
fn record_fields(call: &mut HostCall<'_>, value: Value) -> Option<Vec<Value>> {
    let Value::Heap(handle) = value else {
        return None;
    };
    match call.heap().get(handle) {
        Some(Object::Record { fields, .. }) => Some(fields.clone()),
        _ => None,
    }
}

/// A map's entries, read back the same way [`record_fields`] reads a
/// record's.
fn map_entries(call: &mut HostCall<'_>, value: Value) -> Option<Vec<MapEntry>> {
    let Value::Heap(handle) = value else {
        return None;
    };
    match call.heap().get(handle) {
        Some(Object::Map(entries)) => Some(entries.clone()),
        _ => None,
    }
}

/// `Map<String, String>` read as `(name, value)` pairs.
fn read_headers(call: &mut HostCall<'_>, value: Value) -> Option<Vec<(String, String)>> {
    let entries = map_entries(call, value)?;
    let mut result = Vec::with_capacity(entries.len());
    for entry in entries {
        let MapKey::Str(handle) = entry.key else {
            return None;
        };
        let name = call.str_of(Value::Heap(handle))?.to_owned();
        let text = call.str_of(entry.value)?.to_owned();
        result.push((name, text));
    }
    Some(result)
}

/// Builds a `Map<String, String>` value from `(name, value)` pairs.
fn build_headers(call: &mut HostCall<'_>, headers: &[(String, String)]) -> Result<Value, Fault> {
    let mut entries = Vec::with_capacity(headers.len());
    for (name, text) in headers {
        let key = call.string(name.clone())?;
        let Value::Heap(handle) = key else {
            return Err(Fault::internal("a String value was not a heap handle"));
        };
        let value = call.string(text.clone())?;
        entries.push(MapEntry {
            key: MapKey::Str(handle),
            value,
        });
    }
    call.map(entries)
}

/// `Err(HttpError.Kind(message))` (section 16.1).
fn http_error(call: &mut HostCall<'_>, kind: HttpErrorKind, about: &str) -> Result<Value, Fault> {
    let variant = match kind {
        HttpErrorKind::Denied => "Denied",
        HttpErrorKind::Invalid => "Invalid",
        HttpErrorKind::Failed => "Failed",
        HttpErrorKind::Timeout => "Timeout",
        HttpErrorKind::TooLarge => "TooLarge",
    };
    let message = call.string(about.to_owned())?;
    let error = call.enum_value(HTTP_ERROR, variant, vec![message])?;
    call.err(error)
}

/// HS1002 for input that did not fit what is left of `maxMemoryBytes`: the
/// same fault `fs.read` raises for a file that is too large, raised once
/// `size` bytes (more than the allowance) have been read.
fn too_large_to_read(call: &HostCall<'_>, size: u64) -> Result<Value, Fault> {
    call.refuse_if_too_large(size)?;
    Err(Fault::internal(
        "a read reported input too large for the allowance it was given, \
         but the same size no longer exceeds it",
    ))
}

/// One `fs` operation (section 16.8). Each takes a path first, checks it
/// against the roots the matching kind was granted, and answers a `Result`.
fn bind_fs(operation: &str, granted: &Rc<Granted>) -> Option<Box<dyn HostFn>> {
    /// Which grant kind an operation needs.
    enum Kind {
        Read,
        Write,
    }
    let kind = match operation {
        "read" | "exists" | "list" => Kind::Read,
        "write" | "append" | "remove" | "mkdir" => Kind::Write,
        _ => return None,
    };
    let operation = operation.to_owned();
    let granted = Rc::clone(granted);
    Some(Box::new(move |args: &[Value], call: &mut HostCall<'_>| {
        let Some(path) = args.first().and_then(|v| call.str_of(*v)) else {
            return Err(Fault::invalid_argument(&operation, "expected a path"));
        };
        let path = path.to_owned();
        let contents = match args.get(1) {
            None => None,
            Some(value) => match call.str_of(*value) {
                Some(text) => Some(text.to_owned()),
                None => {
                    return Err(Fault::invalid_argument(&operation, "expected a String"));
                }
            },
        };
        let roots = match kind {
            Kind::Read => &granted.fs_read,
            Kind::Write => &granted.fs_write,
        };
        if operation == "read" {
            // Split out from the generic `outcome` match below because it
            // alone needs `call` itself: to read the run's own remaining
            // memory allowance, so a file too large for it raises HS1002
            // directly rather than going through `io_error`, and to let
            // `fs_read` poll cancellation between chunks of a large read
            // (section 16.8: "a capability returns as soon as it can once
            // cancelled"; see `capabilities::fs_read`'s own doc comment).
            let outcome =
                crate::capabilities::fs_read(&path, roots, call.memory_remaining(), || {
                    call.is_cancelled()
                });
            return match outcome {
                crate::capabilities::ReadOutcome::Ok(text) => {
                    let text = call.string(text)?;
                    call.ok(text)
                }
                crate::capabilities::ReadOutcome::Io(kind) => io_error(call, kind, &path),
                crate::capabilities::ReadOutcome::TooLarge(size) => {
                    call.refuse_if_too_large(size)?;
                    Err(Fault::internal(
                        "fs.read reported a file too large for the allowance it was given, \
                         but the same size no longer exceeds it",
                    ))
                }
                crate::capabilities::ReadOutcome::Cancelled => Err(Fault::cancelled()),
            };
        }
        let outcome = match (operation.as_str(), contents) {
            ("write", Some(text)) => crate::capabilities::fs_write(&path, &text, roots)
                .map(|n| FsValue::Count(i64::try_from(n).unwrap_or(i64::MAX))),
            ("append", Some(text)) => crate::capabilities::fs_append(&path, &text, roots)
                .map(|n| FsValue::Count(i64::try_from(n).unwrap_or(i64::MAX))),
            ("write" | "append", None) => {
                return Err(Fault::invalid_argument(&operation, "expected contents"));
            }
            ("exists", _) => crate::capabilities::fs_exists(&path, roots).map(FsValue::Flag),
            ("list", _) => crate::capabilities::fs_list(&path, roots).map(FsValue::Names),
            ("remove", _) => crate::capabilities::fs_remove(&path, roots).map(FsValue::Flag),
            ("mkdir", _) => crate::capabilities::fs_mkdir(&path, roots).map(FsValue::Flag),
            _ => return Err(Fault::internal("an unknown fs operation was bound")),
        };
        match outcome {
            Ok(value) => {
                let value = value.into_value(call)?;
                call.ok(value)
            }
            Err(kind) => io_error(call, kind, &path),
        }
    }))
}

/// What one `fs` operation produced, before it is put on the heap. `read`
/// is handled separately, above, so it has no variant here (see the doc
/// comment on that `if operation == "read"` branch).
enum FsValue {
    Count(i64),
    Flag(bool),
    Names(Vec<String>),
}

impl FsValue {
    fn into_value(self, call: &mut HostCall<'_>) -> Result<Value, Fault> {
        match self {
            FsValue::Count(n) => Ok(Value::Int(n)),
            FsValue::Flag(b) => Ok(Value::Bool(b)),
            FsValue::Names(names) => {
                let mut items = Vec::with_capacity(names.len());
                for name in names {
                    items.push(call.string(name)?);
                }
                call.list(items)
            }
        }
    }
}

/// `Err(IoError.Kind(message))` (section 16.1): the variant the operation
/// reported, carrying the path or source it was about.
fn io_error(call: &mut HostCall<'_>, kind: IoErrorKind, about: &str) -> Result<Value, Fault> {
    let variant = match kind {
        IoErrorKind::NotFound => "NotFound",
        IoErrorKind::Denied => "Denied",
        IoErrorKind::AlreadyExists => "AlreadyExists",
        IoErrorKind::Invalid => "Invalid",
        IoErrorKind::Failed => "Failed",
    };
    let message = call.string(about.to_owned())?;
    let error = call.enum_value(IO_ERROR, variant, vec![message])?;
    call.err(error)
}

#[cfg(test)]
mod tests {
    use super::{PrintSink, bind_capability, capabilities_for};
    use crate::granted::Granted;
    use hollowscript::CapabilityShape;
    use std::rc::Rc;

    fn names(granted: &Granted) -> Vec<String> {
        capabilities_for(granted)
            .capabilities
            .into_iter()
            .map(|c| c.name)
            .collect()
    }

    fn everything() -> Granted {
        let mut granted = Granted::none();
        granted.env = true;
        granted.args = true;
        granted.stdin = true;
        granted.fs_read = vec![crate::capabilities::ScopeRoot::new(".").unwrap()];
        granted.http = vec![crate::http::parse_http_scope("example.com").unwrap()];
        granted
    }

    #[test]
    fn a_run_with_no_flags_grants_exactly_print_clock_and_random() {
        assert_eq!(names(&Granted::none()), ["print", "clock", "random"]);
    }

    #[test]
    fn each_capability_is_declared_only_when_it_is_granted() {
        let mut granted = Granted::none();
        granted.env = true;
        assert_eq!(names(&granted), ["print", "clock", "random", "env"]);
        granted.args = true;
        assert_eq!(names(&granted), ["print", "clock", "random", "env", "args"]);
        assert_eq!(
            names(&everything()),
            [
                "print", "clock", "random", "env", "args", "fs", "stdin", "http"
            ]
        );
    }

    /// The declarations and the bindings are two lists that must agree, and
    /// nothing but this test compares them: a function declared and not
    /// bound is a fault the first time a program calls it, and one bound and
    /// not declared can never be called at all.
    #[test]
    fn every_declared_capability_function_is_bound() {
        let granted = everything();
        let interface = capabilities_for(&granted);
        let granted = Rc::new(granted);
        let output = PrintSink::collect();
        for capability in &interface.capabilities {
            match &capability.shape {
                CapabilityShape::Function { .. } => {
                    assert!(
                        bind_capability(&capability.name, None, &output, &granted).is_some(),
                        "{} is declared but not bound",
                        capability.name
                    );
                }
                CapabilityShape::Namespace(functions) => {
                    for (function, _, _) in functions {
                        assert!(
                            bind_capability(&capability.name, Some(function), &output, &granted)
                                .is_some(),
                            "{}.{function} is declared but not bound",
                            capability.name
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_capability_that_is_not_declared_is_not_bound() {
        let granted = Rc::new(Granted::none());
        let output = PrintSink::collect();
        assert!(bind_capability("http", Some("delete"), &output, &granted).is_none());
        assert!(bind_capability("fs", Some("chmod"), &output, &granted).is_none());
        assert!(bind_capability("nonsense", None, &output, &granted).is_none());
    }
}

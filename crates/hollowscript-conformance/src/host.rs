//! The fixed host every conformance case runs against (`corpus/FORMAT.md`,
//! "The host"), plus what a case's own grants and `<name>.host.json` add to
//! it ([`crate::hostconfig`]).
//!
//! `clock` and `random` answer the same values every time, so a case's
//! expected output is a fact about the language rather than about the
//! machine it ran on. `kind` exists only for the corpus: it reports the
//! runtime kind of the value a host boundary actually received, so a case
//! can observe whether an `Int` was promoted to a `Float` on the way in.
//! `fs` and `http` are declared and bound only when a case's `grants` ask
//! for them, exactly as `hollowscript-cli::default_grants` does for a real
//! run, so that a case which does not grant them still sees `uses fs` as
//! HS0404.

use crate::hostconfig::HostConfig;
use hollowscript::{
    CapabilityDecl, CapabilityInterface, CapabilityShape, Fault, HostCall, HostFn, MapEntry,
    MapKey, Object, Value,
};
use hollowscript_core::transfer::{TransferType, TypeRef, TypeTable, VariantDef};
use std::cell::RefCell;
use std::rc::Rc;

/// The fixed answers of the corpus host.
const NOW: &str = "2026-01-01T00:00:00Z";
const TODAY: &str = "2026-01-01";
const UNIX_MILLIS: i64 = 1_767_225_600_000;
const MONOTONIC_MILLIS: i64 = 0;
const RANDOM_FLOAT: f64 = 0.5;

const IO_ERROR: &str = "IoError";
const IO_ERROR_VARIANTS: [&str; 5] = ["NotFound", "Denied", "AlreadyExists", "Invalid", "Failed"];
const HTTP_ERROR: &str = "HttpError";
const HTTP_ERROR_VARIANTS: [&str; 5] = ["Denied", "Invalid", "Failed", "Timeout", "TooLarge"];

/// The lines a case's program has printed so far.
pub(crate) type Output = Rc<RefCell<Vec<String>>>;

/// The capability interface of the corpus host, extended by `config`'s
/// grants exactly as a real host's would be (section 9.5 rule 1: nothing
/// beyond the fixed four is granted unless asked for).
#[must_use]
pub(crate) fn capabilities(config: &HostConfig) -> CapabilityInterface {
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
        CapabilityDecl {
            name: "kind".to_owned(),
            shape: CapabilityShape::Function {
                params: vec![float_ty],
                ret: Some(string_ty),
            },
        },
    ];
    if !config.fs_read.is_empty() || !config.fs_write.is_empty() {
        let io_error = io_error_type(&mut types, string_ty);
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
    if !config.http.is_empty() {
        let http_error = http_error_type(&mut types, string_ty);
        let headers = types.add(TransferType::Map(string_ty, string_ty));
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

/// Binds one `(capability, function)` pair to its implementation, `config`
/// giving `fs` and `http` what they need.
pub(crate) fn bind(
    name: &str,
    function: Option<&str>,
    output: &Output,
    config: &HostConfig,
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
                        // charged before anything is written.
                        let written = u64::try_from(text.len()).unwrap_or(u64::MAX);
                        call.write_output(written.saturating_add(1))?;
                        output.borrow_mut().push(text);
                        // `print` declares no result (section 16.2).
                        Ok(Value::None)
                    }
                    _ => Err(Fault::invalid_argument("print", "expected one String")),
                },
            ))
        }
        ("clock", Some("now")) => Some(fixed_string(NOW)),
        ("clock", Some("today")) => Some(fixed_string(TODAY)),
        ("clock", Some("unixMillis")) => Some(Box::new(|_: &[Value], _: &mut HostCall<'_>| {
            Ok(Value::Int(UNIX_MILLIS))
        })),
        ("clock", Some("monotonicMillis")) => {
            Some(Box::new(|_: &[Value], _: &mut HostCall<'_>| {
                Ok(Value::Int(MONOTONIC_MILLIS))
            }))
        }
        ("random", Some("float")) => Some(Box::new(|_: &[Value], _: &mut HostCall<'_>| {
            Ok(Value::Float(RANDOM_FLOAT))
        })),
        // `min + floor(0.5 * (upTo - min))`, computed so that the widest
        // permitted range cannot overflow on the way.
        ("random", Some("int")) => Some(Box::new(
            |args: &[Value], _: &mut HostCall<'_>| match args {
                [Value::Int(min), Value::Int(up_to)] if min < up_to => {
                    // `min + floor(0.5 * (upTo - min))`, in exact integers:
                    // the widest permitted span does not fit a `f64` without
                    // losing the last bits, and halving it is a shift.
                    let span = i128::from(*up_to) - i128::from(*min);
                    let value = i128::from(*min) + (span >> 1);
                    Ok(Value::Int(i64::try_from(value).unwrap_or(i64::MAX)))
                }
                [Value::Int(_), Value::Int(_)] => Err(Fault::invalid_argument(
                    "random.int",
                    "min must be less than upTo",
                )),
                _ => Err(Fault::invalid_argument("random.int", "expected two Int")),
            },
        )),
        ("kind", None) => Some(Box::new(
            |args: &[Value], call: &mut HostCall<'_>| match args {
                [Value::Float(_)] => call.string("Float"),
                [Value::Int(_)] => call.string("Int"),
                _ => Err(Fault::invalid_argument("kind", "expected one number")),
            },
        )),
        ("fs", Some(operation)) => bind_fs(operation, config),
        ("http", Some("get")) => {
            let config = config.clone();
            Some(Box::new(move |args: &[Value], call: &mut HostCall<'_>| {
                let Some(url) = args.first().and_then(|v| call.str_of(*v)) else {
                    return Err(Fault::invalid_argument("http.get", "expected a String"));
                };
                let url = url.to_owned();
                run_http(call, "GET", &url, &[], None, &config)
            }))
        }
        ("http", Some("request")) => {
            let config = config.clone();
            Some(Box::new(move |args: &[Value], call: &mut HostCall<'_>| {
                let Some(record) = args.first().copied() else {
                    return Err(Fault::invalid_argument(
                        "http.request",
                        "expected an HttpRequest",
                    ));
                };
                let Some(request) = read_http_request(call, record) else {
                    return Err(Fault::invalid_argument(
                        "http.request",
                        "expected an HttpRequest",
                    ));
                };
                run_http(
                    call,
                    &request.method,
                    &request.url,
                    &request.headers,
                    request.body.as_deref(),
                    &config,
                )
            }))
        }
        _ => None,
    }
}

fn fixed_string(text: &'static str) -> Box<dyn HostFn> {
    Box::new(move |_: &[Value], call: &mut HostCall<'_>| call.string(text))
}

/// One `fs` operation (section 16.8), against `config`'s granted roots and
/// canned files.
fn bind_fs(operation: &str, config: &HostConfig) -> Option<Box<dyn HostFn>> {
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
    let config = config.clone();
    Some(Box::new(move |args: &[Value], call: &mut HostCall<'_>| {
        let Some(path) = args.first().and_then(|v| call.str_of(*v)) else {
            return Err(Fault::invalid_argument(&operation, "expected a path"));
        };
        let path = path.to_owned();
        let contents = match args.get(1) {
            None => None,
            Some(value) => match call.str_of(*value) {
                Some(text) => Some(text.to_owned()),
                None => return Err(Fault::invalid_argument(&operation, "expected a String")),
            },
        };
        let roots = match kind {
            Kind::Read => &config.fs_read,
            Kind::Write => &config.fs_write,
        };
        let outcome = match (operation.as_str(), contents) {
            ("read", _) => fs_read(&path, roots, &config).map(FsValue::Text),
            ("write", Some(text)) => fs_write(&path, &text, roots, &config)
                .map(|n| FsValue::Count(i64::try_from(n).unwrap_or(i64::MAX))),
            ("append", Some(text)) => fs_append(&path, &text, roots, &config)
                .map(|n| FsValue::Count(i64::try_from(n).unwrap_or(i64::MAX))),
            ("write" | "append", None) => {
                return Err(Fault::invalid_argument(&operation, "expected contents"));
            }
            ("exists", _) => fs_exists(&path, roots, &config).map(FsValue::Flag),
            ("list", _) => fs_list(&path, roots, &config).map(FsValue::Names),
            ("remove", _) => fs_remove(&path, roots, &config).map(FsValue::Flag),
            ("mkdir", _) => fs_mkdir(&path, roots, &config).map(FsValue::Flag),
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

enum FsValue {
    Text(String),
    Count(i64),
    Flag(bool),
    Names(Vec<String>),
}

impl FsValue {
    fn into_value(self, call: &mut HostCall<'_>) -> Result<Value, Fault> {
        match self {
            FsValue::Text(text) => call.string(text),
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

/// The outcome of an `fs` or `http` operation, mirroring the language's
/// `IoError`/`HttpError` variants (section 16.1).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum ErrorKind {
    NotFound,
    Denied,
    AlreadyExists,
    Invalid,
    Failed,
}

fn fs_read(path: &str, roots: &[Vec<String>], config: &HostConfig) -> Result<String, ErrorKind> {
    let segments = check_fs_scope(path, roots)?;
    let files = config.files.borrow();
    if let Some(text) = files.get(&segments) {
        return Ok(text.clone());
    }
    if crate::hostconfig::is_directory(&files, &segments) {
        return Err(ErrorKind::Invalid);
    }
    Err(ErrorKind::NotFound)
}

fn fs_write(
    path: &str,
    contents: &str,
    roots: &[Vec<String>],
    config: &HostConfig,
) -> Result<usize, ErrorKind> {
    let segments = check_fs_scope(path, roots)?;
    let mut files = config.files.borrow_mut();
    if crate::hostconfig::is_directory(&files, &segments) {
        return Err(ErrorKind::Invalid);
    }
    let parent = segments
        .get(..segments.len().saturating_sub(1))
        .unwrap_or(&[]);
    if !parent.is_empty() && !crate::hostconfig::is_directory(&files, parent) {
        return Err(ErrorKind::NotFound);
    }
    files.insert(segments, contents.to_owned());
    Ok(contents.len())
}

fn fs_append(
    path: &str,
    contents: &str,
    roots: &[Vec<String>],
    config: &HostConfig,
) -> Result<usize, ErrorKind> {
    let segments = check_fs_scope(path, roots)?;
    let mut files = config.files.borrow_mut();
    if crate::hostconfig::is_directory(&files, &segments) {
        return Err(ErrorKind::Invalid);
    }
    let parent = segments
        .get(..segments.len().saturating_sub(1))
        .unwrap_or(&[]);
    if !parent.is_empty() && !crate::hostconfig::is_directory(&files, parent) {
        return Err(ErrorKind::NotFound);
    }
    let existing = files.entry(segments).or_default();
    existing.push_str(contents);
    Ok(contents.len())
}

fn fs_exists(path: &str, roots: &[Vec<String>], config: &HostConfig) -> Result<bool, ErrorKind> {
    let segments = check_fs_scope(path, roots)?;
    let files = config.files.borrow();
    Ok(files.contains_key(&segments) || crate::hostconfig::is_directory(&files, &segments))
}

fn fs_list(
    path: &str,
    roots: &[Vec<String>],
    config: &HostConfig,
) -> Result<Vec<String>, ErrorKind> {
    let segments = check_fs_scope(path, roots)?;
    let files = config.files.borrow();
    if !crate::hostconfig::is_directory(&files, &segments) {
        return Err(ErrorKind::NotFound);
    }
    let mut names: Vec<String> = files
        .keys()
        .filter_map(|key| {
            (key.len() > segments.len() && key.starts_with(&segments[..]))
                .then(|| key.get(segments.len()).cloned())
                .flatten()
        })
        .collect();
    names.sort();
    names.dedup();
    Ok(names)
}

fn fs_remove(path: &str, roots: &[Vec<String>], config: &HostConfig) -> Result<bool, ErrorKind> {
    let segments = check_fs_scope(path, roots)?;
    let mut files = config.files.borrow_mut();
    if crate::hostconfig::is_directory(&files, &segments) {
        return Err(ErrorKind::Failed);
    }
    Ok(files.remove(&segments).is_some())
}

fn fs_mkdir(path: &str, roots: &[Vec<String>], config: &HostConfig) -> Result<bool, ErrorKind> {
    let segments = check_fs_scope(path, roots)?;
    let mut files = config.files.borrow_mut();
    if files.contains_key(&segments) {
        return Err(ErrorKind::AlreadyExists);
    }
    if crate::hostconfig::is_directory(&files, &segments) {
        return Ok(false);
    }
    // Implies the directory by seeding an empty marker file one level
    // inside it, consistent with `corpus/FORMAT.md`'s "directories are
    // implied by their files": nothing else here can represent an empty one.
    let mut marker = segments.clone();
    marker.push(".keep".to_owned());
    files.insert(marker, String::new());
    Ok(true)
}

fn check_fs_scope(path: &str, roots: &[Vec<String>]) -> Result<Vec<String>, ErrorKind> {
    if crate::hostconfig::is_malformed_path(path) {
        return Err(ErrorKind::Invalid);
    }
    let segments = crate::hostconfig::normalize(path);
    if crate::hostconfig::contained_in(&segments, roots) {
        Ok(segments)
    } else {
        Err(ErrorKind::Denied)
    }
}

fn io_error(call: &mut HostCall<'_>, kind: ErrorKind, about: &str) -> Result<Value, Fault> {
    let variant = match kind {
        ErrorKind::NotFound => "NotFound",
        ErrorKind::Denied => "Denied",
        ErrorKind::AlreadyExists => "AlreadyExists",
        ErrorKind::Invalid => "Invalid",
        ErrorKind::Failed => "Failed",
    };
    let message = call.string(about.to_owned())?;
    let error = call.enum_value(IO_ERROR, variant, vec![message])?;
    call.err(error)
}

fn http_error(call: &mut HostCall<'_>, kind: ErrorKind, about: &str) -> Result<Value, Fault> {
    let variant = match kind {
        ErrorKind::Denied => "Denied",
        ErrorKind::Invalid => "Invalid",
        ErrorKind::NotFound | ErrorKind::AlreadyExists | ErrorKind::Failed => "Failed",
    };
    let message = call.string(about.to_owned())?;
    let error = call.enum_value(HTTP_ERROR, variant, vec![message])?;
    call.err(error)
}

/// Reads an `HttpRequest` record's fields back out of the heap, in the
/// sorted field order `HostCall::record` builds them in (`body`, `headers`,
/// `method`, `url`).
/// An `HttpRequest`'s fields, already read out of the heap.
struct HttpRequestFields {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<String>,
}

fn read_http_request(call: &mut HostCall<'_>, value: Value) -> Option<HttpRequestFields> {
    let Value::Heap(handle) = value else {
        return None;
    };
    let fields = match call.heap().get(handle) {
        Some(Object::Record { fields, .. }) => fields.clone(),
        _ => return None,
    };
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
    Some(HttpRequestFields {
        method,
        url,
        headers,
        body,
    })
}

fn read_headers(call: &mut HostCall<'_>, value: Value) -> Option<Vec<(String, String)>> {
    let Value::Heap(handle) = value else {
        return None;
    };
    let entries = match call.heap().get(handle) {
        Some(Object::Map(entries)) => entries.clone(),
        _ => return None,
    };
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

/// The seven methods section 16.9 allows, always upper case.
const ALLOWED_METHODS: [&str; 7] = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

/// Performs one `http` request against `config`'s grant and canned
/// exchanges (section 16.9; see the module doc for what is not
/// implemented: redirects and the response size and time limits, which no
/// canned exchange needs).
#[allow(clippy::too_many_arguments)]
fn run_http(
    call: &mut HostCall<'_>,
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Option<&str>,
    config: &HostConfig,
) -> Result<Value, Fault> {
    let _ = body;
    if !ALLOWED_METHODS.contains(&method) {
        return http_error(call, ErrorKind::Invalid, url);
    }
    let bad_byte = |s: &str| s.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0);
    let mut seen: Vec<String> = Vec::with_capacity(headers.len());
    for (name, value) in headers {
        if name.is_empty() || bad_byte(name) || bad_byte(value) {
            return http_error(call, ErrorKind::Invalid, url);
        }
        let lower = name.to_ascii_lowercase();
        if seen.contains(&lower) {
            return http_error(call, ErrorKind::Invalid, url);
        }
        seen.push(lower);
    }
    let Some((https, host, port)) = crate::hostconfig::parse_url(url) else {
        return http_error(call, ErrorKind::Invalid, url);
    };
    if !crate::hostconfig::http_scope_allows(&config.http, &host, https, port) {
        return http_error(call, ErrorKind::Denied, url);
    }
    let Some(exchange) = config
        .http_exchanges
        .iter()
        .find(|e| e.method == method && e.url == url)
    else {
        return http_error(call, ErrorKind::Failed, url);
    };
    let response_headers = build_headers(call, &exchange.headers)?;
    let response_body = call.string(exchange.body.clone())?;
    let record = call.record(vec![
        ("status", Value::Int(exchange.status)),
        ("headers", response_headers),
        ("body", response_body),
    ])?;
    call.ok(record)
}

#[cfg(test)]
mod tests {
    use super::{Output, bind, capabilities};
    use crate::hostconfig::HostConfig;
    use hollowscript::{HostCall, HostTypes, Value};

    #[test]
    fn the_interface_declares_exactly_the_hosts_capabilities() {
        let names: Vec<String> = capabilities(&HostConfig::default())
            .capabilities
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["print", "clock", "random", "kind"]);
    }

    #[test]
    fn fs_and_http_are_declared_only_when_granted() {
        let mut config = HostConfig::default();
        config.apply_grants(&["fs:read=./data".to_owned()]);
        let names: Vec<String> = capabilities(&config)
            .capabilities
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["print", "clock", "random", "kind", "fs"]);
    }

    #[test]
    fn random_int_is_the_midpoint_the_corpus_fixes() {
        let mut f = bind(
            "random",
            Some("int"),
            &Output::default(),
            &HostConfig::default(),
        )
        .expect("random.int is bound");
        let mut heap = hollowscript::Heap::new();
        let types = HostTypes::new();
        let mut counters = hollowscript::RunCounters::new();
        let limits = hollowscript::Limits::default();
        let mut call = HostCall::new(&mut heap, &types, &mut counters, &limits, None);
        assert!(matches!(
            f.call(&[Value::Int(1), Value::Int(7)], &mut call),
            Ok(Value::Int(4))
        ));
        assert!(matches!(
            f.call(&[Value::Int(0), Value::Int(2)], &mut call),
            Ok(Value::Int(1))
        ));
        assert!(f.call(&[Value::Int(5), Value::Int(5)], &mut call).is_err());
    }

    #[test]
    fn kind_reports_what_the_boundary_received() {
        let mut f =
            bind("kind", None, &Output::default(), &HostConfig::default()).expect("kind is bound");
        let mut heap = hollowscript::Heap::new();
        let types = HostTypes::new();
        let mut counters = hollowscript::RunCounters::new();
        let limits = hollowscript::Limits::default();
        let mut call = HostCall::new(&mut heap, &types, &mut counters, &limits, None);
        let float = f.call(&[Value::Float(1.0)], &mut call).expect("a Float");
        let int = f.call(&[Value::Int(1)], &mut call).expect("an Int");
        assert_eq!(call.str_of(float), Some("Float"));
        assert_eq!(call.str_of(int), Some("Int"));
    }

    #[test]
    fn print_collects_the_lines_in_order() {
        let output = Output::default();
        let mut f = bind("print", None, &output, &HostConfig::default()).expect("print is bound");
        let mut heap = hollowscript::Heap::new();
        let types = HostTypes::new();
        let mut counters = hollowscript::RunCounters::new();
        let limits = hollowscript::Limits::default();
        let mut call = HostCall::new(&mut heap, &types, &mut counters, &limits, None);
        for line in ["a", "b"] {
            let value = call.string(line).expect("a string");
            f.call(&[value], &mut call).expect("print succeeds");
        }
        assert_eq!(*output.borrow(), ["a", "b"]);
    }

    /// A full run, through `hollowscript::check` and `hollowscript::load`,
    /// the way `case::run` and `spec::judge_run` use this module: `fs.write`
    /// really creates a file a following `fs.read` really reads back, which
    /// the six `grants/*` denial cases and the three spec examples never
    /// exercise (they only take the `Err(Denied(_))` path).
    #[test]
    fn fs_write_then_read_round_trips_through_a_real_run() {
        let mut config = HostConfig::default();
        config.apply_grants(&["fs:write=out".to_owned(), "fs:read=out".to_owned()]);
        config
            .files
            .borrow_mut()
            .insert(crate::hostconfig::normalize("out/.keep"), String::new());
        let source = concat!(
            "uses print, fs\n",
            "match effect fs.write(\"out/a.txt\", \"hi\") {\n",
            "    Ok(n) { effect print(\"wrote ${n}\") }\n",
            "    Err(e) { effect print(\"write failed\") }\n",
            "}\n",
            "match effect fs.read(\"out/a.txt\") {\n",
            "    Ok(t) { effect print(\"read ${t}\") }\n",
            "    Err(e) { effect print(\"read failed\") }\n",
            "}\n",
            "return 0\n",
        );
        let output = run_source(source, &config);
        assert_eq!(output, ["wrote 2", "read hi"]);
    }

    /// `http.get` against a canned exchange (`<name>.host.json`'s `http`
    /// list, section 19.4) answers `Ok` with the exchange's status, headers
    /// and body, which the corpus's own denial-only `grants/*` cases and
    /// spec examples never exercise either.
    #[test]
    fn http_get_answers_a_canned_exchange() {
        let mut config = HostConfig::default();
        config.apply_grants(&["http=api.example.com".to_owned()]);
        config.http_exchanges.push(crate::hostconfig::Exchange {
            method: "GET".to_owned(),
            url: "https://api.example.com/status".to_owned(),
            status: 200,
            headers: vec![("X-Test".to_owned(), "one".to_owned())],
            body: "hello".to_owned(),
        });
        let source = concat!(
            "uses print, http\n",
            "match effect http.get(\"https://api.example.com/status\") {\n",
            "    Ok(r) { effect print(\"status ${r.status} body ${r.body}\") }\n",
            "    Err(e) { effect print(\"failed\") }\n",
            "}\n",
            "return 0\n",
        );
        let output = run_source(source, &config);
        assert_eq!(output, ["status 200 body hello"]);
    }

    /// Runs `source` against `config` through the real pipeline and returns
    /// what it printed, panicking with the diagnostics or the fault if it
    /// did not run to completion.
    /// A loader for a single-file test program: nothing it names can
    /// import anything else.
    struct NoImports;
    impl hollowscript::Loader for NoImports {
        fn load(&mut self, _: &str) -> Result<Vec<u8>, hollowscript::LoaderError> {
            Err(hollowscript::LoaderError::NotFound)
        }
    }

    fn run_source(source: &str, config: &HostConfig) -> Vec<String> {
        let output = Output::default();
        let options = hollowscript::Options {
            limits: hollowscript::Limits::default(),
            capabilities: capabilities(config),
            filter_schema: None,
            batch_capability_calls: false,
            gc_stress_mode: false,
        };
        let outcome = hollowscript::load(
            source,
            "main.hws",
            &options,
            &mut NoImports,
            &mut |name, function| bind(name, function, &output, config),
            &mut hollowscript::Control::none(),
        );
        let hollowscript::LoadOutcome::Instance(mut instance) = outcome else {
            panic!("the program did not check");
        };
        let fault = instance.fault().cloned();
        let _ = instance.dispose();
        assert!(fault.is_none(), "the program faulted: {fault:?}");
        output.borrow().clone()
    }
}

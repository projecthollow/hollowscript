//! Rendering a value as text, in the form the conformance corpus fixes for a
//! case's `result` (`corpus/FORMAT.md`, "Output and result").
//!
//! This is the only place a whole value is turned into text, so it is also
//! what a host uses to show a load result or an exported constant. It is not
//! the language's own `${}` interpolation, which section 3.9 defines for the
//! four scalar types alone; this renders every value, including a list, a
//! record, a map, a set and an enum value, and tags each with its kind.
//!
//! The walk uses an explicit stack rather than recursion, because section
//! 11.4 requires every operation on a value to work at any depth that fits
//! in `maxMemoryBytes`, and a value nested a million deep is a few lines of
//! script.

use crate::heap::Heap;
use crate::value::{MapKey, Object, Value};
use hollowscript_bytecode::module::Module;

/// What the renderer has still to do. Text is emitted as it is popped, so
/// the pieces of a container interleave with its elements. A container's
/// elements are not all turned into steps when it is met: a step holds the
/// slice and the position of the next element, so a list of 600,000 pairs
/// costs a step or two at a time on the stack and not millions (and not a
/// vector of steps for each element, as it once did), which keeps the
/// renderer's own memory a small fraction of the text it writes.
enum Step<'a> {
    /// Render this value.
    Value(Value),
    /// Emit this text unchanged.
    Text(&'a str),
    /// The elements of a list or an enum payload from `next` on, then `close`.
    Items {
        items: &'a [Value],
        next: usize,
        close: &'static str,
    },
    /// The elements of a set from `next` on, then `}`.
    Keys { keys: &'a [MapKey], next: usize },
    /// The entries of a map from `next` on, then `}`.
    Entries {
        entries: &'a [crate::value::MapEntry],
        next: usize,
    },
    /// The fields of a record from `next` on, then `}`.
    Fields {
        shape: &'a [hollowscript_bytecode::module::StringId],
        fields: &'a [Value],
        next: usize,
    },
}

/// The most [`render`] will ever write before it stops and appends
/// [`TRUNCATED_MARKER`] instead of continuing.
///
/// A shared subvalue is rendered once for every reference to it, not once
/// per object on the heap (`corpus/FORMAT.md`'s text has no way to name a
/// shared value once and refer back to it, so this is not a choice this
/// function could make differently): a local list of 200 references to one
/// 10 MB string, an ordinary value a few lines of script can build, would
/// otherwise render as roughly 2 GB of text before this existed. Bounding
/// the written length also bounds the work this function ever does past
/// that point, without having to track which heap objects have already
/// been visited to refuse only a genuine DAG.
///
/// Comfortably above anything `corpus/FORMAT.md` or a legitimate exported
/// constant renders, so no existing case is affected; this is the same
/// order of magnitude as the CLI's own default `maxOutputBytes` (section
/// 11, 16 MiB), which a load result or export read this feeds does not
/// otherwise have a limit tied to.
const MAX_RENDER_BYTES: usize = 16 * 1024 * 1024;

/// Appended in place of the rest of the text once [`MAX_RENDER_BYTES`] is
/// reached, so a caller (or a person reading the CLI's own output) can
/// tell a truncated render from a value that genuinely ends there.
const TRUNCATED_MARKER: &str = "...(truncated: rendered output exceeds the limit)";

/// Renders `value` as the corpus's `result` text. `module` supplies the
/// string table that record field names and enum variant names live in.
///
/// Bounded at `MAX_RENDER_BYTES`: once reached, the walk stops
/// and `TRUNCATED_MARKER` is appended, rather than continuing to build
/// text (and, before it, to do the work of building it) without limit.
#[must_use]
pub fn render(module: &Module, heap: &Heap, value: Value) -> String {
    let mut out = String::new();
    let mut stack = vec![Step::Value(value)];
    while let Some(step) = stack.pop() {
        match step {
            Step::Text(text) => out.push_str(text),
            Step::Value(value) => render_one(module, heap, value, &mut out, &mut stack),
            Step::Items { items, next, close } => match items.get(next) {
                None => out.push_str(close),
                Some(&item) => {
                    if next > 0 {
                        out.push_str(", ");
                    }
                    stack.push(Step::Items {
                        items,
                        next: next + 1,
                        close,
                    });
                    stack.push(Step::Value(item));
                }
            },
            Step::Keys { keys, next } => match keys.get(next) {
                None => out.push('}'),
                Some(&key) => {
                    if next > 0 {
                        out.push_str(", ");
                    }
                    stack.push(Step::Keys {
                        keys,
                        next: next + 1,
                    });
                    stack.push(Step::Value(key_value(key)));
                }
            },
            Step::Entries { entries, next } => match entries.get(next) {
                None => out.push('}'),
                Some(entry) => {
                    if next > 0 {
                        out.push_str(", ");
                    }
                    stack.push(Step::Entries {
                        entries,
                        next: next + 1,
                    });
                    stack.push(Step::Value(entry.value));
                    stack.push(Step::Text(": "));
                    stack.push(Step::Value(key_value(entry.key)));
                }
            },
            Step::Fields {
                shape,
                fields,
                next,
            } => match (shape.get(next), fields.get(next)) {
                (Some(id), Some(&field)) => {
                    if next > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(string_of(module, id.0));
                    out.push_str(": ");
                    stack.push(Step::Fields {
                        shape,
                        fields,
                        next: next + 1,
                    });
                    stack.push(Step::Value(field));
                }
                _ => out.push('}'),
            },
        }
        if out.len() > MAX_RENDER_BYTES {
            // `String::truncate` panics unless the index is a char
            // boundary; `push_json_string` can leave `out` mid multi-byte
            // character at exactly `MAX_RENDER_BYTES`, so the boundary is
            // found by walking back rather than assumed.
            let mut cut = MAX_RENDER_BYTES;
            while !out.is_char_boundary(cut) {
                cut -= 1;
            }
            out.truncate(cut);
            out.push_str(TRUNCATED_MARKER);
            return out;
        }
    }
    out
}

fn render_one<'a>(
    module: &'a Module,
    heap: &'a Heap,
    value: Value,
    out: &mut String,
    stack: &mut Vec<Step<'a>>,
) {
    match value {
        Value::None => out.push_str("None"),
        Value::Bool(b) => out.push_str(if b { "Bool(true)" } else { "Bool(false)" }),
        Value::Int(i) => {
            out.push_str("Int(");
            out.push_str(&i.to_string());
            out.push(')');
        }
        Value::Float(f) => {
            out.push_str("Float(");
            // The corpus has no negative zero, and section 3.9's own text
            // for a zero is `0.0` whichever sign it carries.
            out.push_str(&crate::interp::format_float(if f == 0.0 { 0.0 } else { f }));
            out.push(')');
        }
        Value::Heap(handle) => render_object(module, heap, handle, out, stack),
    }
}

fn render_object<'a>(
    module: &'a Module,
    heap: &'a Heap,
    handle: crate::arena::Handle,
    out: &mut String,
    stack: &mut Vec<Step<'a>>,
) {
    match heap.get(handle) {
        // Not reachable from a live value of this heap: either the object
        // was freed, or (section 12.5) `handle` was allocated by a
        // different instance's heap entirely, which `heap.get` refuses by
        // arena identity before it would otherwise, by coincidence, find a
        // live but unrelated object at the same slot.
        None => out.push_str("Unreachable"),
        Some(Object::Str(text)) => {
            out.push_str("String(");
            push_json_string(text, out);
            out.push(')');
        }
        Some(Object::Filter(text)) => {
            out.push_str("Filter(");
            push_json_string(text, out);
            out.push(')');
        }
        // A cell is the storage of one captured binding, never a value in
        // its own right; render what it holds.
        Some(Object::Cell(inner)) => stack.push(Step::Value(*inner)),
        // `corpus/FORMAT.md`: every function value renders as `Function`,
        // whichever kind it is.
        Some(Object::Builtin(_) | Object::Closure { .. }) => out.push_str("Function"),
        Some(Object::List(items)) => {
            out.push('[');
            stack.push(Step::Items {
                items,
                next: 0,
                close: "]",
            });
        }
        Some(Object::Record { shape, fields }) => {
            out.push('{');
            stack.push(Step::Fields {
                shape,
                fields,
                next: 0,
            });
        }
        Some(Object::Map(entries)) => {
            out.push_str("Map{");
            stack.push(Step::Entries { entries, next: 0 });
        }
        Some(Object::Set(keys)) => {
            out.push_str("Set{");
            stack.push(Step::Keys { keys, next: 0 });
        }
        Some(Object::Enum { variant, payload }) => {
            let (enum_name, name) = match module.variants.get(*variant as usize) {
                Some(desc) => (
                    string_of(module, desc.enum_name.0),
                    string_of(module, desc.name.0),
                ),
                None => ("Enum", "Variant"),
            };
            out.push_str(enum_name);
            out.push('.');
            out.push_str(name);
            if payload.is_empty() {
                return;
            }
            out.push('(');
            stack.push(Step::Items {
                items: payload,
                next: 0,
                close: ")",
            });
        }
    }
}

/// A map key as the value it stands for.
fn key_value(key: MapKey) -> Value {
    match key {
        MapKey::Bool(b) => Value::Bool(b),
        MapKey::Int(i) => Value::Int(i),
        MapKey::Str(h) => Value::Heap(h),
    }
}

fn string_of(module: &Module, id: u32) -> &str {
    module.strings.get(id as usize).map_or("?", String::as_str)
}

/// Appends `text` as a JSON string literal, quotes included.
fn push_json_string(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::render;
    use crate::heap::Heap;
    use crate::value::{MapEntry, MapKey, Object, Value};
    use hollowscript_bytecode::module::{Module, StringId, VariantDesc};

    fn module_with(strings: &[&str], variants: Vec<VariantDesc>) -> Module {
        let mut module = Module::new();
        module.strings = strings.iter().map(|s| (*s).to_owned()).collect();
        module.variants = variants;
        module
    }

    #[test]
    fn scalars_render_with_their_kind() {
        let module = module_with(&[], vec![]);
        let heap = Heap::new();
        assert_eq!(render(&module, &heap, Value::Int(-3)), "Int(-3)");
        assert_eq!(render(&module, &heap, Value::Bool(true)), "Bool(true)");
        assert_eq!(render(&module, &heap, Value::Float(2.0)), "Float(2.0)");
        assert_eq!(render(&module, &heap, Value::Float(-0.0)), "Float(0.0)");
        assert_eq!(render(&module, &heap, Value::Float(1e-6)), "Float(1e-6)");
        assert_eq!(render(&module, &heap, Value::None), "None");
    }

    /// A handle from a different heap renders as `"Unreachable"`,
    /// never as whatever live object happens to occupy the same slot of the
    /// heap actually passed in. Both heaps' first allocation lands at index
    /// 0, generation 0 of their own, separate arenas, which is exactly the
    /// coincidence the fix (arena identity, `crate::arena`) closes.
    #[test]
    fn a_handle_from_a_different_heap_renders_as_unreachable() {
        let module = module_with(&[], vec![]);
        let mut own_heap = Heap::new();
        let mut other_heap = Heap::new();
        let foreign = other_heap
            .alloc(Object::Str("kept secret".to_owned()))
            .unwrap();
        let _ = own_heap.alloc(Object::Str("own value".to_owned()));
        assert_eq!(render(&module, &own_heap, foreign), "Unreachable");
    }

    #[test]
    fn a_string_is_a_json_string_literal() {
        let module = module_with(&[], vec![]);
        let mut heap = Heap::new();
        let value = heap.alloc(Object::Str("a\"b\nc".to_owned())).unwrap();
        assert_eq!(render(&module, &heap, value), r#"String("a\"b\nc")"#);
    }

    #[test]
    fn a_list_a_record_a_map_and_a_set_render_in_order() {
        let module = module_with(&["x", "y"], vec![]);
        let mut heap = Heap::new();
        let list = heap
            .alloc(Object::List(vec![Value::Int(1), Value::Int(2)]))
            .unwrap();
        assert_eq!(render(&module, &heap, list), "[Int(1), Int(2)]");

        let record = heap
            .alloc(Object::Record {
                shape: vec![StringId(0), StringId(1)],
                fields: vec![Value::Int(1), Value::Bool(false)],
            })
            .unwrap();
        assert_eq!(
            render(&module, &heap, record),
            "{x: Int(1), y: Bool(false)}"
        );

        let key = heap.alloc(Object::Str("k".to_owned())).unwrap();
        let Value::Heap(key_handle) = key else {
            panic!("a string is a heap value")
        };
        let map = heap
            .alloc(Object::Map(vec![MapEntry {
                key: MapKey::Str(key_handle),
                value: Value::Int(7),
            }]))
            .unwrap();
        assert_eq!(render(&module, &heap, map), r#"Map{String("k"): Int(7)}"#);

        let set = heap
            .alloc(Object::Set(vec![MapKey::Int(1), MapKey::Int(2)]))
            .unwrap();
        assert_eq!(render(&module, &heap, set), "Set{Int(1), Int(2)}");
    }

    #[test]
    fn an_enum_renders_with_its_declared_names() {
        let module = module_with(
            &["Colour", "Red", "Shade"],
            vec![
                VariantDesc {
                    enum_name: StringId(0),
                    name: StringId(1),
                    index: 0,
                    arity: 0,
                    decl_module: 0,
                    decl_index: 0,
                },
                VariantDesc {
                    enum_name: StringId(0),
                    name: StringId(2),
                    index: 1,
                    arity: 1,
                    decl_module: 0,
                    decl_index: 0,
                },
            ],
        );
        let mut heap = Heap::new();
        let plain = heap
            .alloc(Object::Enum {
                variant: 0,
                payload: vec![],
            })
            .unwrap();
        assert_eq!(render(&module, &heap, plain), "Colour.Red");
        let with_payload = heap
            .alloc(Object::Enum {
                variant: 1,
                payload: vec![Value::Int(4)],
            })
            .unwrap();
        assert_eq!(render(&module, &heap, with_payload), "Colour.Shade(Int(4))");
    }

    #[test]
    fn the_empty_containers_render_without_a_separator() {
        let module = module_with(&[], vec![]);
        let mut heap = Heap::new();
        let list = heap.alloc(Object::List(vec![])).unwrap();
        assert_eq!(render(&module, &heap, list), "[]");
        let map = heap.alloc(Object::Map(vec![])).unwrap();
        assert_eq!(render(&module, &heap, map), "Map{}");
        let set = heap.alloc(Object::Set(vec![])).unwrap();
        assert_eq!(render(&module, &heap, set), "Set{}");
    }

    /// Section 11.4 requires every operation on a value to work at any depth
    /// that fits in the memory limit, so the renderer must not recurse.
    #[test]
    fn a_deeply_nested_value_renders_without_exhausting_the_stack() {
        let module = module_with(&[], vec![]);
        let mut heap = Heap::new();
        let mut value = heap.alloc(Object::List(vec![])).unwrap();
        for _ in 0..100_000 {
            value = heap.alloc(Object::List(vec![value])).unwrap();
        }
        let text = render(&module, &heap, value);
        assert_eq!(text.len(), 100_000 * 2 + 2);
    }

    /// A shared subvalue is rendered once per reference to it, so a
    /// small list of many references to one large string, a shape any
    /// program can build in a few lines, renders far more text than the
    /// value's own footprint on the heap. `render` must bound its own
    /// output rather than build all of it: this list of 200 references to
    /// a 100,000-byte string would otherwise render around 20,000,000
    /// bytes, past `MAX_RENDER_BYTES`.
    #[test]
    fn a_shared_subvalue_repeated_many_times_is_bounded_not_built_in_full() {
        let module = module_with(&[], vec![]);
        let mut heap = Heap::new();
        let text = heap.alloc(Object::Str("x".repeat(100_000))).unwrap();
        let value = heap.alloc(Object::List(vec![text; 200])).unwrap();
        let rendered = render(&module, &heap, value);
        assert!(
            rendered.len() <= super::MAX_RENDER_BYTES + super::TRUNCATED_MARKER.len(),
            "rendered output was not bounded: {} bytes",
            rendered.len()
        );
        assert!(
            rendered.ends_with(super::TRUNCATED_MARKER),
            "a bounded render carries an explicit marker, not a silent cut"
        );
    }
}

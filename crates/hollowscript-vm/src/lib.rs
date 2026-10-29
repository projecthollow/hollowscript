//! The HollowScript virtual machine.
//!
//! - [`arena`]: the index-based, generational arena the heap is built on;
//! - [`value`]: the value representation;
//! - [`heap`]: the mark and sweep collector and the byte accounting of chapter 11;
//! - [`limits`]: the per-run step, memory, output and call depth counters;
//! - [`natives`]: the standard library of chapters 13 to 15;
//! - [`capability`]: how the VM calls capabilities and how a host binds them,
//!   with section 9.6's rule for what a host may return applied to every
//!   result before the program sees it;
//! - [`hostvalues`]: what a capability function reads its arguments and
//!   builds its result with;
//! - [`interp`]: the interpreter that ties the above together and runs
//!   [`hollowscript_bytecode`]'s instruction set;
//! - [`render::render`]: a value as the text the conformance corpus fixes
//!   for it.
//!
//! This crate depends on the shared vocabulary and the bytecode crates only,
//! and never on the front end: that is what lets `hollowscript-wasm` embed
//! it, and only it, in the browser.

#![forbid(unsafe_code)]

mod accept;
pub mod arena;
pub mod capability;
pub mod data;
pub mod heap;
pub mod hostvalues;
pub mod interp;
pub mod json;
pub mod limits;
pub mod natives;
pub mod render;
pub mod time;
pub mod value;

pub use arena::{Arena, Handle};
pub use capability::{CapabilityTable, Command, CommandBuffer, CommandResult, HostFn};
pub use heap::Heap;
pub use hostvalues::{HostCall, HostTypes};
pub use interp::{Interpreter, Run};
pub use limits::{RunCounters, check_retained_size};
pub use render::render;
pub use value::{Builtin, MapEntry, MapKey, Object, Value};

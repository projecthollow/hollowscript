//! The HollowScript bytecode.
//!
//! This crate is the contract between the compiler, which writes bytecode, and
//! the VM, which runs it. It holds three things and depends only on the shared
//! vocabulary crate:
//!
//! - [`opcode`]: the register machine's instruction set;
//! - [`module`] and [`container`]: the in-memory program and its versioned,
//!   checksummed binary container;
//! - [`verify`](mod@verify): the verifier, and [`verify::VerifiedModule`], the only thing
//!   the VM will run.
//!
//! It is a separate crate so that the browser VM can accept bytecode, and check
//! it, without containing a parser or a checker.

#![forbid(unsafe_code)]

pub mod container;
pub mod module;
pub mod opcode;
pub mod sha256;
pub mod verify;

pub use container::{ContainerError, FORMAT_VERSION, decode, encode, fix_checksum};
pub use module::Module;
pub use opcode::{Instruction, Op};
pub use verify::{LoadError, VerifiedModule, VerifyError, verify, verify_bytes};

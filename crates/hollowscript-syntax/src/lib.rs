//! The HollowScript front end.
//!
//! - [`token`]: tokens, keywords, comments and the decoding of string escapes;
//! - [`lexer`]: source text to tokens, with the statement terminator rule;
//! - [`ast`]: the syntax tree, defined in full;
//! - [`parser`]: tokens to a tree, with error recovery;
//! - [`format`](mod@format): the canonical formatter;
//! - [`budget`]: the structural limits of section 1.10 as arithmetic.
//!
//! The crate depends only on the shared vocabulary crate. Source text, spans
//! and diagnostics are those of `hollowscript-core`.
//!
//! The lexer, the parser and the printer of [`format`](mod@format) are all
//! written.

#![forbid(unsafe_code)]

pub mod ast;
pub mod budget;
pub mod format;
pub mod lexer;
pub mod parser;
pub mod token;

pub use format::{FormatOutput, format};
pub use lexer::{LexOutput, lex};
pub use parser::{ParseOutput, parse};
pub use token::{Comment, CommentKind, Keyword, Token, TokenKind};

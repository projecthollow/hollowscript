//! Resource limits (chapter 11) and structural limits (section 1.10).
//!
//! The resource limits are options of a host. The structural limits are fixed
//! by the language and are constants here so that every crate uses one value.

use std::fmt;

/// One of the four resource limits, named as the host option.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum LimitOption {
    /// Units of work executed.
    MaxSteps,
    /// Bytes allocated by a run, and bytes retained by an instance.
    MaxMemoryBytes,
    /// Bytes written by the `print` capability.
    MaxOutputBytes,
    /// Active function and closure calls.
    MaxCallDepth,
}

impl LimitOption {
    /// The option name used by hosts and in diagnostics, for example `maxSteps`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            LimitOption::MaxSteps => "maxSteps",
            LimitOption::MaxMemoryBytes => "maxMemoryBytes",
            LimitOption::MaxOutputBytes => "maxOutputBytes",
            LimitOption::MaxCallDepth => "maxCallDepth",
        }
    }

    /// The option with this name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<LimitOption> {
        [
            LimitOption::MaxSteps,
            LimitOption::MaxMemoryBytes,
            LimitOption::MaxOutputBytes,
            LimitOption::MaxCallDepth,
        ]
        .into_iter()
        .find(|o| o.name() == name)
    }
}

/// The `data` of a limit fault (section 17.1): the option, its configured
/// value and the counter value at the fault.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct LimitData {
    /// Which limit.
    pub option: LimitOption,
    /// The configured value.
    pub limit: u64,
    /// The counter value at the fault.
    pub used: u64,
}

/// The resource limits of a run (section 11.1).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Units of work executed by one run.
    pub max_steps: u64,
    /// Bytes allocated by one run, and bytes retained by an instance.
    pub max_memory_bytes: u64,
    /// Bytes written by `print` in one run.
    pub max_output_bytes: u64,
    /// Active function and closure calls.
    pub max_call_depth: u64,
}

/// The largest value of any limit, 2^63 - 1.
pub const MAX_LIMIT: u64 = i64::MAX as u64;

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_steps: 100_000_000,
            max_memory_bytes: 67_108_864,
            max_output_bytes: 16_777_216,
            max_call_depth: 1_000,
        }
    }
}

/// A limit that is outside 1 to 2^63 - 1.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct LimitError {
    /// The option that is out of range.
    pub option: LimitOption,
    /// The value that was given.
    pub value: u64,
}

impl fmt::Display for LimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} must be from 1 to {MAX_LIMIT}, not {}",
            self.option.name(),
            self.value
        )
    }
}

impl std::error::Error for LimitError {}

impl Limits {
    /// Checks that every limit is from 1 to 2^63 - 1. A limit cannot be
    /// switched off, and 0 is not a value.
    ///
    /// # Errors
    ///
    /// Returns the first option that is out of range.
    pub fn validate(&self) -> Result<(), LimitError> {
        for (option, value) in [
            (LimitOption::MaxSteps, self.max_steps),
            (LimitOption::MaxMemoryBytes, self.max_memory_bytes),
            (LimitOption::MaxOutputBytes, self.max_output_bytes),
            (LimitOption::MaxCallDepth, self.max_call_depth),
        ] {
            if value == 0 || value > MAX_LIMIT {
                return Err(LimitError { option, value });
            }
        }
        Ok(())
    }

    /// The value of one option.
    #[must_use]
    pub fn get(&self, option: LimitOption) -> u64 {
        match option {
            LimitOption::MaxSteps => self.max_steps,
            LimitOption::MaxMemoryBytes => self.max_memory_bytes,
            LimitOption::MaxOutputBytes => self.max_output_bytes,
            LimitOption::MaxCallDepth => self.max_call_depth,
        }
    }

    /// Sets one option by value, without validating it.
    pub fn set(&mut self, option: LimitOption, value: u64) {
        match option {
            LimitOption::MaxSteps => self.max_steps = value,
            LimitOption::MaxMemoryBytes => self.max_memory_bytes = value,
            LimitOption::MaxOutputBytes => self.max_output_bytes = value,
            LimitOption::MaxCallDepth => self.max_call_depth = value,
        }
    }
}

/// The limits that the language fixes. They are not host options.
pub mod structural {
    /// Nesting of parentheses, brackets, braces, blocks, patterns, type
    /// brackets and interpolation braces, counted together (section 1.10, HS0113).
    pub const MAX_NESTING_DEPTH: u32 = 250;
    /// The weight of a token: enclosing constructs plus links of enclosing chains (HS0113).
    pub const MAX_TOKEN_WEIGHT: u32 = 1_000;
    /// Links in one chain of one binary operator level, one postfix chain or
    /// one `else if` run (HS0114).
    pub const MAX_CHAIN_LINKS: u32 = 500;
    /// Nesting of string interpolations (HS0115).
    pub const MAX_INTERPOLATION_DEPTH: u32 = 100;
    /// Diagnostics reported for one file, followed by one HS0004.
    pub const MAX_DIAGNOSTICS_PER_FILE: usize = 100;
    /// Modules in one program (HS0612).
    pub const MAX_MODULES: usize = 1_024;
    /// Total bytes of source in one program (HS0612).
    pub const MAX_PROGRAM_BYTES: usize = 67_108_864;
    /// Nodes in the expanded form of a type (HS0335).
    pub const MAX_TYPE_NODES: u32 = 10_000;
    /// Rows of one `match` (HS0511).
    pub const MAX_MATCH_ROWS: usize = 1_000;
    /// Elements of one list pattern (HS0511).
    pub const MAX_LIST_PATTERN_ELEMENTS: usize = 250;
    /// Units of work of the exhaustiveness check of one `match` (HS0511).
    pub const MAX_EXHAUSTIVENESS_WORK: u64 = 100_000;
    /// Frames reported with a fault (section 8.1).
    pub const MAX_FRAMES: usize = 32;
    /// Nesting of arrays and objects in JSON text (section 15.1).
    pub const MAX_JSON_DEPTH: usize = 128;
    /// Bytes of filter source (Appendix A, HS0704).
    pub const MAX_FILTER_BYTES: usize = 4_096;
    /// Nesting of groups and negations in a filter (HS0703).
    pub const MAX_FILTER_NESTING: u32 = 100;
    /// Days either way of a relative date in a filter (HS0706).
    pub const MAX_RELATIVE_DATE_DAYS: u32 = 5_000;
    /// A safepoint occurs whenever the step counter reaches or passes a
    /// multiple of this (section 11.7).
    pub const SAFEPOINT_INTERVAL: u64 = 1_024;
    /// No single step charge is larger than this (section 11.3).
    pub const MAX_CHARGE: u64 = 1_024;
    /// Tokens scanned between the safepoints of the front end (section 12.1).
    pub const FRONT_END_TOKEN_INTERVAL: u32 = 4_096;
    /// Nodes checked between the safepoints of the checker (section 12.1).
    pub const FRONT_END_NODE_INTERVAL: u32 = 1_024;
}

#[cfg(test)]
mod tests {
    use super::{LimitError, LimitOption, Limits, MAX_LIMIT};

    #[test]
    fn defaults_are_those_of_the_specification() {
        let d = Limits::default();
        assert_eq!(d.max_steps, 100_000_000);
        assert_eq!(d.max_memory_bytes, 64 * 1024 * 1024);
        assert_eq!(d.max_output_bytes, 16 * 1024 * 1024);
        assert_eq!(d.max_call_depth, 1_000);
        assert_eq!(d.validate(), Ok(()));
    }

    #[test]
    fn zero_and_too_large_values_are_refused() {
        let mut limits = Limits::default();
        limits.set(LimitOption::MaxCallDepth, 0);
        assert_eq!(
            limits.validate(),
            Err(LimitError {
                option: LimitOption::MaxCallDepth,
                value: 0
            })
        );
        limits.set(LimitOption::MaxCallDepth, MAX_LIMIT);
        assert_eq!(limits.validate(), Ok(()));
        limits.set(LimitOption::MaxSteps, MAX_LIMIT + 1);
        assert_eq!(limits.validate().unwrap_err().option, LimitOption::MaxSteps);
    }

    #[test]
    fn option_names_round_trip() {
        for option in [
            LimitOption::MaxSteps,
            LimitOption::MaxMemoryBytes,
            LimitOption::MaxOutputBytes,
            LimitOption::MaxCallDepth,
        ] {
            assert_eq!(LimitOption::from_name(option.name()), Some(option));
        }
        assert_eq!(LimitOption::from_name("maxTime"), None);
    }
}

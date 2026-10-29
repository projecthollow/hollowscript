//! The registry of diagnostic codes (chapter 17 of the specification).
//!
//! Every code is a constant of [`Code`], so a code that does not exist cannot
//! be written. The table below is the single source of severity, stage, message
//! template and span description. The test `tests/spec_registry.rs` compares it
//! with the specification, so the two cannot drift apart.

use std::fmt;

/// Whether a diagnostic stops a program from running.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Severity {
    /// Prevents the program from running. Every fault is an error. Errors sort first.
    Error,
    /// Reported, and the program still runs.
    Warning,
}

impl Severity {
    /// `error` or `warning`, as used in machine readable output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// The stage of processing that reports a diagnostic (section 17.2).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    /// Reading source text into tokens.
    Lex,
    /// Building the syntax tree.
    Parse,
    /// All semantic analysis, which runs before anything executes.
    Check,
    /// Loading modules.
    Load,
    /// Running a program, and host requests to an instance.
    Run,
}

impl Stage {
    /// The lower case name used in the specification.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Stage::Lex => "lex",
            Stage::Parse => "parse",
            Stage::Check => "check",
            Stage::Load => "load",
            Stage::Run => "run",
        }
    }
}

/// The families of codes (section 17.5).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Family {
    /// HS0001 to HS0099: source text and lexical errors.
    Lexical,
    /// HS0101 to HS0199: syntax.
    Syntax,
    /// HS0201 to HS0299: names and scope.
    Names,
    /// HS0301 to HS0399: types, expressions and statements.
    Types,
    /// HS0401 to HS0499: effects and capabilities.
    Effects,
    /// HS0501 to HS0599: patterns.
    Patterns,
    /// HS0601 to HS0699: modules.
    Modules,
    /// HS0701 to HS0799: filter literals.
    Filters,
    /// HS0801 to HS0899: warnings.
    Warnings,
    /// HS0901 to HS0999: limits of an implementation rather than of the
    /// language (section 1.10).
    Limits,
    /// HS1001 to HS1099: runtime faults.
    Faults,
    /// HS1201 to HS1299: host errors for instances.
    Host,
}

/// A diagnostic code such as `HS0301`. Codes are stable and never reused.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Code(u16);

/// Everything the specification says about one code.
#[derive(Copy, Clone, Debug)]
pub struct CodeInfo {
    /// The code.
    pub code: Code,
    /// Its severity.
    pub severity: Severity,
    /// The stages that report it. One code, HS0014, has two.
    pub stages: &'static [Stage],
    /// The message template. `{name}` marks a placeholder.
    pub template: &'static str,
    /// What the span of the diagnostic covers, in words.
    pub span: &'static str,
}

impl Code {
    /// The number, for example 301 for `HS0301`.
    #[must_use]
    pub const fn number(self) -> u16 {
        self.0
    }

    /// The code with this number, if the registry has one.
    #[must_use]
    pub fn from_number(number: u16) -> Option<Code> {
        let index = REGISTRY.binary_search_by_key(&number, |i| i.code.0).ok()?;
        REGISTRY.get(index).map(|info| info.code)
    }

    /// Parses text such as `HS0301`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Code> {
        let digits = text.strip_prefix("HS")?;
        if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Code::from_number(digits.parse().ok()?)
    }

    /// The registry entry of this code.
    #[must_use]
    pub fn info(self) -> &'static CodeInfo {
        // Every value of `Code` comes from the registry, so the search succeeds.
        REGISTRY
            .binary_search_by_key(&self.0, |i| i.code.0)
            .ok()
            .and_then(|i| REGISTRY.get(i))
            .unwrap_or(&FALLBACK)
    }

    /// The family of this code.
    #[must_use]
    pub const fn family(self) -> Family {
        match self.0 {
            0..=99 => Family::Lexical,
            100..=199 => Family::Syntax,
            200..=299 => Family::Names,
            300..=399 => Family::Types,
            400..=499 => Family::Effects,
            500..=599 => Family::Patterns,
            600..=699 => Family::Modules,
            700..=799 => Family::Filters,
            800..=899 => Family::Warnings,
            900..=999 => Family::Limits,
            1000..=1099 => Family::Faults,
            _ => Family::Host,
        }
    }

    /// The severity of this code.
    #[must_use]
    pub fn severity(self) -> Severity {
        self.info().severity
    }

    /// The text of this code, for example `HS0301`.
    #[must_use]
    pub fn to_text(self) -> String {
        format!("HS{:04}", self.0)
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HS{:04}", self.0)
    }
}

impl fmt::Debug for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Code(HS{:04})", self.0)
    }
}

/// Every code, ordered by number.
#[must_use]
pub fn all() -> &'static [CodeInfo] {
    REGISTRY
}

/// Returned for a code that is not in the registry, which cannot happen.
static FALLBACK: CodeInfo = CodeInfo {
    code: Code(1099),
    severity: Severity::Error,
    stages: &[Stage::Run],
    template: "Internal error: {message}.",
    span: "the operation, if known",
};

macro_rules! registry {
    ($($name:ident = $number:literal, $severity:ident, [$($stage:ident),+], $template:literal, $span:literal;)+) => {
        impl Code {
            $(
                #[doc = $template]
                pub const $name: Code = Code($number);
            )+
        }

        static REGISTRY: &[CodeInfo] = &[
            $(
                CodeInfo {
                    code: Code($number),
                    severity: Severity::$severity,
                    stages: &[$(Stage::$stage),+],
                    template: $template,
                    span: $span,
                },
            )+
        ];
    };
}

registry! {
    HS0001 = 1, Error, [Lex],
        "The source is not valid UTF-8 (invalid byte at offset {offset}).",
        "the byte";
    HS0002 = 2, Error, [Lex],
        "The source is {size} bytes; the limit is {limit}.",
        "empty, offset 0";
    HS0004 = 4, Error, [Check],
        "Too many diagnostics; only the first {limit} are shown.",
        "empty, at the start of the first suppressed diagnostic";
    HS0005 = 5, Error, [Lex],
        "'{char}' is not a character HollowScript recognises here.",
        "the code point";
    HS0006 = 6, Error, [Lex],
        "This string is missing its closing quote before the end of the line or of the file.",
        "the string from its opening quote";
    HS0007 = 7, Error, [Lex],
        "This interpolation is missing its closing '}'.",
        "the `${`";
    HS0008 = 8, Error, [Lex],
        "This is not a valid escape sequence in a string; use \\\" \\\\ \\n \\t \\r \\$ or \\u{...}.",
        "the escape";
    HS0009 = 9, Error, [Lex],
        "This unicode escape is not valid; \\u{...} needs 1 to 6 hexadecimal digits that name a Unicode scalar value.",
        "the escape";
    HS0010 = 10, Error, [Lex],
        "This filter is missing its closing backtick before the end of the line.",
        "the filter from its opening backtick";
    HS0011 = 11, Error, [Lex],
        "A number needs a digit after its decimal point, for example 2.0 instead of 2.",
        "the number and the point";
    HS0012 = 12, Error, [Lex],
        "A number needs a digit before its decimal point, for example 0.5 instead of .5.",
        "the point and digits";
    HS0013 = 13, Error, [Lex],
        "This number is followed by characters that cannot be part of it.",
        "the number and the characters";
    HS0014 = 14, Error, [Lex, Check],
        "This integer literal is too large; an Int is at most 9223372036854775807.",
        "the literal";
    HS0015 = 15, Error, [Lex],
        "This number is too large to be a Float.",
        "the literal";
    HS0016 = 16, Error, [Lex],
        "An exponent needs at least one digit.",
        "the number and its `e`";
    HS0101 = 101, Error, [Parse],
        "Expected {expected}, found {found}.",
        "the token found";
    HS0102 = 102, Error, [Parse],
        "Expected an expression, found {found}.",
        "the token found";
    HS0103 = 103, Error, [Parse],
        "Expected a statement, found {found}.",
        "the token found";
    HS0104 = 104, Error, [Parse],
        "'{word}' is reserved for a future version of HollowScript.",
        "the word";
    HS0106 = 106, Error, [Parse],
        "'uses' lines must come before every import and statement.",
        "the `uses` line";
    HS0107 = 107, Error, [Parse],
        "'import' lines must come before every statement.",
        "the `import` line";
    HS0108 = 108, Error, [Parse],
        "'export' cannot be used on '{kind}'; only fn, record, enum and constant can be exported.",
        "the declaration";
    HS0109 = 109, Error, [Parse],
        "Only a call may stand alone as a statement; an expression on its own computes a value and discards it.",
        "the expression";
    HS0110 = 110, Error, [Parse],
        "A record literal cannot appear directly here because it is ambiguous with the block that follows; wrap it in parentheses.",
        "the literal";
    HS0111 = 111, Error, [Parse],
        "A type may have only one trailing '?'; write Int?, not Int??.",
        "the second `?`";
    HS0112 = 112, Error, [Parse],
        "A {kind} needs at least one {member}.",
        "the braces";
    HS0113 = 113, Error, [Parse],
        "Nesting is too deep; the limit is {limit} levels.",
        "the construct that exceeded it";
    HS0114 = 114, Error, [Parse],
        "This chain has too many links; the limit is {limit}.",
        "the chain";
    HS0115 = 115, Error, [Lex],
        "String interpolations are nested too deeply; the limit is {limit}.",
        "the innermost `${`";
    HS0116 = 116, Error, [Parse],
        "A variant name must start with an upper-case letter.",
        "the name";
    HS0117 = 117, Error, [Parse],
        "{kind} is only allowed at the top level of a file.",
        "the declaration";
    HS0120 = 120, Error, [Parse],
        "A name bound by a pattern must start with a lower-case letter or '_'.",
        "the name";
    HS0121 = 121, Error, [Parse],
        "'..' can only be the last element of a list pattern.",
        "the `..`";
    HS0122 = 122, Error, [Parse],
        "'else' must be on the same line as the closing '}' before it.",
        "the `else`";
    HS0123 = 123, Error, [Parse],
        "A field cannot be named '_'.",
        "the name";
    HS0201 = 201, Error, [Check],
        "'{name}' is already declared.",
        "the second declaration; related: the first";
    HS0202 = 202, Error, [Check],
        "The {kind} '{name}' is already declared in this {container}.",
        "the second; related: the first";
    HS0203 = 203, Error, [Check],
        "The {kind} '{name}' is already set in this {container}.",
        "the second; related: the first";
    HS0204 = 204, Error, [Check],
        "The name '{name}' is already bound by this pattern.",
        "the second binding";
    HS0205 = 205, Error, [Check],
        "'{name}' is not defined here.",
        "the name";
    HS0206 = 206, Error, [Check],
        "'{name}' cannot be assigned; only a name declared with 'variable' can be.",
        "the assignment target";
    HS0208 = 208, Error, [Check],
        "Unknown type '{name}'.",
        "the type name";
    HS0209 = 209, Error, [Check],
        "'{name}' is a {kind} and cannot be used as a value.",
        "the name";
    HS0210 = 210, Error, [Check],
        "The function '{function}' uses '{name}', which is not initialised yet at the place the function is used (line {line}).",
        "the use of `{name}` in the function; related: the statement";
    HS0212 = 212, Error, [Check],
        "The type parameter '{name}' is declared more than once.",
        "the second declaration";
    HS0213 = 213, Error, [Check],
        "'_' cannot be used as a value.",
        "the `_`";
    HS0301 = 301, Error, [Check],
        "Expected {expected}, found {found}.",
        "the expression";
    HS0302 = 302, Error, [Check],
        "'{name}' does not take type arguments.",
        "the type";
    HS0303 = 303, Error, [Check],
        "'{name}' takes {expected} type argument(s), found {found}.",
        "the type";
    HS0304 = 304, Error, [Check],
        "The type of {what} cannot be inferred; add a type annotation.",
        "the declaration or literal";
    HS0305 = 305, Error, [Check],
        "The record '{name}' contains itself, directly or through other types; use an enum for recursive data.",
        "the declaration";
    HS0306 = 306, Error, [Check],
        "A map key must be Int, String or Bool, found {found}.",
        "the type";
    HS0307 = 307, Error, [Check],
        "A set element must be Int, String or Bool, found {found}.",
        "the type or argument";
    HS0308 = 308, Error, [Check],
        "The type parameter '{name}' of '{function}' cannot be inferred; add a type annotation.",
        "the call or name";
    HS0309 = 309, Error, [Check],
        "The type of the parameter '{name}' cannot be inferred from where this closure is used; add an annotation.",
        "the parameter";
    HS0310 = 310, Error, [Check],
        "The unary '-' requires an Int or a Float, found {found}.",
        "the operand";
    HS0311 = 311, Error, [Check],
        "'{op}' requires two numbers, found {left} and {right}.",
        "the operator expression";
    HS0312 = 312, Error, [Check],
        "'{op}' requires two numbers or two Strings, found {left} and {right}.",
        "the operator expression";
    HS0313 = 313, Error, [Check],
        "Cannot compare {left} with {right} using '{op}'.",
        "the operator expression";
    HS0314 = 314, Error, [Check],
        "Values of type {found} cannot be compared with '==' or '!='.",
        "the operator expression";
    HS0315 = 315, Error, [Check],
        "'{op}' requires Bool, found {found}.",
        "the operand";
    HS0316 = 316, Error, [Check],
        "A condition must be a Bool, found {found}.",
        "the condition";
    HS0317 = 317, Error, [Check],
        "Interpolation accepts Int, Float, Bool and String, found {found}.",
        "the interpolated expression";
    HS0318 = 318, Error, [Check],
        "This is not a function and cannot be called (its type is {found}).",
        "the callee";
    HS0319 = 319, Error, [Check],
        "Expected {expected} argument(s), found {found}.",
        "the argument list";
    HS0320 = 320, Error, [Check],
        "{type} has no method '{name}'.",
        "the method name";
    HS0321 = 321, Error, [Check],
        "This record has no field '{name}'.",
        "the field name";
    HS0322 = 322, Error, [Check],
        "Only a record has fields; '.{name}' is not valid on {found}.",
        "the field name";
    HS0323 = 323, Error, [Check],
        "Only lists and maps can be indexed, found {found}.",
        "the indexed expression";
    HS0325 = 325, Error, [Check],
        "'for' can only iterate a list, a set or a map, found {found}.",
        "the collection";
    HS0326 = 326, Error, [Check],
        "The record is missing the field '{name}'.",
        "the literal";
    HS0327 = 327, Error, [Check],
        "The record has a field '{name}' that {type} does not have.",
        "the field";
    HS0328 = 328, Error, [Check],
        "'none' can only be compared with an optional value, found {found}.",
        "the operator expression";
    HS0330 = 330, Error, [Check],
        "{operation} requires {requirement}, found {found}.",
        "the call";
    HS0331 = 331, Error, [Check],
        "The '?' operator needs a Result, found {found}.",
        "the operand";
    HS0332 = 332, Error, [Check],
        "'?' can only be used where the enclosing function returns a Result with the same error type ({expected}).",
        "the `?`";
    HS0333 = 333, Error, [Check],
        "'{enum}' has no variant '{name}'.",
        "the variant name";
    HS0334 = 334, Error, [Check],
        "'{name}' must be called; a method or a function that takes any record is not a value.",
        "the name";
    HS0335 = 335, Error, [Check],
        "This type is too large; the limit is {limit} nodes.",
        "the type";
    HS0340 = 340, Error, [Check],
        "This function returns nothing, so 'return' cannot carry a value.",
        "the `return`";
    HS0341 = 341, Error, [Check],
        "This function declares a return type, so 'return' must carry a value.",
        "the `return`";
    HS0342 = 342, Error, [Check],
        "{what} declares a return type, so every path must return a value.",
        "the function name or `fn`";
    HS0343 = 343, Error, [Check],
        "Unreachable statement: nothing after return, break or continue in the same block ever runs.",
        "the first unreachable statement";
    HS0344 = 344, Error, [Check],
        "'{word}' can only be used inside a loop in the same function.",
        "the `break` or `continue`";
    HS0401 = 401, Error, [Check],
        "This call goes through a value of type {found}, which is effectful, so it must be marked 'effect'.",
        "the call";
    HS0402 = 402, Error, [Check],
        "'effect' is only for a call through an effectful function; this callee has type {found}.",
        "the `effect`";
    HS0403 = 403, Error, [Check],
        "'effect' must be followed directly by a call.",
        "the `effect`";
    HS0404 = 404, Error, [Check],
        "The capability '{name}' is not available in this run. Ask the host to grant it, or remove it from 'uses'.",
        "the name in `uses`";
    HS0501 = 501, Error, [Check],
        "This match does not handle every value{example}.",
        "the `match` keyword";
    HS0502 = 502, Error, [Check],
        "This value is already handled by an earlier match arm.",
        "the repeated pattern";
    HS0503 = 503, Error, [Check],
        "This match arm can never run, because earlier arms already match every value it could match.",
        "the pattern";
    HS0504 = 504, Error, [Check],
        "This pattern cannot match a value of type {found}.",
        "the pattern";
    HS0505 = 505, Error, [Check],
        "'{enum}' has no variant '{name}'.",
        "the variant name";
    HS0506 = 506, Error, [Check],
        "The variant '{name}' has {expected} value(s), found {found} pattern(s).",
        "the pattern";
    HS0507 = 507, Error, [Check],
        "The record type has no field '{name}'.",
        "the field name";
    HS0508 = 508, Error, [Check],
        "The alternatives of a match arm cannot bind names.",
        "the binding";
    HS0509 = 509, Error, [Check],
        "This pattern is not allowed in a declaration or a 'for'; only names, '_', and record and list patterns made of them are. Use 'match'.",
        "the pattern";
    HS0510 = 510, Error, [Check],
        "A value of type {found} cannot be matched.",
        "the scrutinee";
    HS0511 = 511, Error, [Check],
        "This match is too complex to check; the limits are {limit}.",
        "the `match` keyword";
    HS0601 = 601, Error, [Load],
        "Cannot find the module '{path}'.",
        "the path string";
    HS0602 = 602, Error, [Check],
        "An import path must be a plain string beginning './' or '../' with valid segments ({reason}).",
        "the path string";
    HS0603 = 603, Error, [Check],
        "Import cycle: {chain}.",
        "the path string that closes the cycle";
    HS0604 = 604, Error, [Check],
        "The module '{path}' does not export '{name}'.",
        "the name in the import";
    HS0605 = 605, Error, [Check],
        "The import path climbs above the root of the program.",
        "the path string";
    HS0606 = 606, Error, [Check],
        "The exported {kind} '{name}' cannot be read or called by a host because {reason}.",
        "the name";
    HS0607 = 607, Error, [Load],
        "The module '{path}' could not be read: {reason}.",
        "the path string";
    HS0609 = 609, Error, [Check],
        "Leave out the '.hws' extension in an import path.",
        "the path string";
    HS0611 = 611, Error, [Check],
        "A top-level 'return' is only allowed in the entry module.",
        "the `return`";
    HS0612 = 612, Error, [Load],
        "The program has too many modules or too much source; the limit is {limit}.",
        "the path string";
    HS0613 = 613, Error, [Check],
        "The exported {kind} '{name}' exposes a capability: its type contains an effectful function type.",
        "the name";
    HS0701 = 701, Error, [Check],
        "Invalid filter: {detail}.",
        "the offending token, or the end of the filter";
    HS0702 = 702, Error, [Check],
        "A quoted term in this filter is missing its closing quote.",
        "from the opening quote";
    HS0703 = 703, Error, [Check],
        "This filter is nested too deeply; the limit is {limit}.",
        "the group or negation that exceeded it";
    HS0704 = 704, Error, [Check],
        "This filter is too long ({size} bytes); the limit is {limit}.",
        "the literal";
    HS0705 = 705, Error, [Check],
        "'{text}' is not a valid calendar date.",
        "the value";
    HS0706 = 706, Error, [Check],
        "The relative date offset is too large; the limit is {limit} days.",
        "the value";
    HS0707 = 707, Error, [Check],
        "The operator '{op}' cannot be used with {what}.",
        "the operator";
    HS0708 = 708, Error, [Check],
        "The filter has no field named '{field}'.",
        "the field";
    HS0709 = 709, Error, [Check],
        "'{value}' is not a valid value for the {type} field '{field}'.",
        "the value";
    HS0710 = 710, Error, [Check],
        "A filter cannot be empty.",
        "the literal";
    HS0711 = 711, Error, [Check],
        "The date range {from}..{to} ends before it starts.",
        "the value";
    HS0801 = 801, Warning, [Check],
        "Unused constant or variable '{name}'.",
        "the name in its declaration";
    HS0802 = 802, Warning, [Check],
        "Unused import '{name}'.",
        "the name in the import";
    HS0803 = 803, Warning, [Check],
        "The Result of this call is ignored; handle it, or use '?'.",
        "the call";
    HS0901 = 901, Error, [Check],
        "This exceeds a limit of this implementation: {limit}.",
        "the construct that exceeds the limit";
    HS1001 = 1001, Error, [Run],
        "The step limit of {limit} was reached.",
        "the statement, iteration or call being charged";
    HS1002 = 1002, Error, [Run],
        "The memory limit of {limit} bytes was reached by an allocation of {size} bytes.",
        "the allocating operation";
    HS1003 = 1003, Error, [Run],
        "The instance retains {size} bytes, above the limit of {limit}.",
        "the last statement executed";
    HS1004 = 1004, Error, [Run],
        "The output limit of {limit} bytes was reached.",
        "the `print` call";
    HS1005 = 1005, Error, [Run],
        "The call depth limit of {limit} was exceeded. Check for a recursive call with no base case.",
        "the call";
    HS1006 = 1006, Error, [Run],
        "The run was cancelled.",
        "the operation at the safepoint";
    HS1007 = 1007, Error, [Run],
        "This calculation overflows the range of an Int.",
        "the operation";
    HS1008 = 1008, Error, [Run],
        "Division by zero.",
        "the operation";
    HS1009 = 1009, Error, [Run],
        "This calculation is not a number.",
        "the operation";
    HS1010 = 1010, Error, [Run],
        "This calculation is infinite.",
        "the operation";
    HS1011 = 1011, Error, [Run],
        "Invalid argument to '{function}': {reason}.",
        "the call";
    HS1012 = 1012, Error, [Run],
        "The capability '{name}' failed: {message}.",
        "the call";
    HS1013 = 1013, Error, [Run],
        "The capability '{name}' returned a value of the wrong type; expected {expected}.",
        "the call";
    HS1014 = 1014, Error, [Run],
        "The list has {found} element(s) but this pattern needs {expected}.",
        "the pattern";
    HS1015 = 1015, Error, [Run],
        "A limit of this implementation was reached: {limit}.",
        "the operation";
    HS1099 = 1099, Error, [Run],
        "Internal error: {message}.",
        "the operation, if known";
    HS1201 = 1201, Error, [Run],
        "The program exports no function or constant named '{name}'.",
        "none: reported to the host";
    HS1202 = 1202, Error, [Run],
        "Cannot call '{name}': {reason}.",
        "none: reported to the host";
    HS1203 = 1203, Error, [Run],
        "The instance has faulted and cannot run again.",
        "none: reported to the host";
    HS1204 = 1204, Error, [Run],
        "The instance is already running; a call cannot start inside it.",
        "none: reported to the host";
    HS1205 = 1205, Error, [Run],
        "The instance cannot be called because it {reason}.",
        "none: reported to the host";
}

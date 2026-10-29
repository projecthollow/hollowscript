//! The instruction set.
//!
//! The machine is register based. Every function has a frame of at most
//! 65,535 registers, and an instruction is one 32 bit little endian word with
//! an 8 bit opcode and three more bytes of operands in one of four formats:
//!
//! - `Abc`: three 8 bit operands;
//! - `Abx`: an 8 bit operand `a` and a 16 bit unsigned operand `bx`;
//! - `AsBx`: an 8 bit operand `a` and a 16 bit signed operand `sbx`, which for
//!   a jump is relative to the next instruction;
//! - `Ax`: one 24 bit signed operand, used by the long jump.
//!
//! An operand that does not fit is widened by an [`Op::Ext`] word placed
//! directly before the instruction, which supplies the high bytes of its
//! operands: bits 8 to 15 of `a`, `b` and `c` for `Abc`; bits 8 to 15 of `a`
//! and bits 16 to 31 of `bx` for `Abx`; bits 8 to 15 of `a` alone for `AsBx`.
//! An `Ax` instruction takes no prefix. The pair is one instruction: a jump
//! lands on the `Ext`, never on the word after it. [`Wide`] is an
//! instruction with its prefix applied, and is what the verifier and the
//! interpreter read. Most frames need no prefix at all, so the common case
//! costs one word and one decode, as before.
//!
//! Conditional jumps reach 32,767 instructions either way. The compiler turns
//! a conditional jump that would reach further into a short inverted jump over
//! a [`Op::JumpLong`], which reaches 8,388,607.
//!
//! Where an instruction takes a run of values (arguments of a call, elements of
//! a literal) they are in the registers that follow `a`, and a result goes to
//! `a`. Steps are not counted per instruction. The language defines its own
//! step charges (section 11.3), and the compiler places explicit
//! [`Op::Charge`] instructions and instructions that charge as part of their
//! meaning, so the count is the same for every implementation.

use std::fmt;

/// How the operand bytes of an instruction are read.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Format {
    /// Operands `a`, `b`, `c`, one byte each.
    Abc,
    /// Operand `a` and the unsigned 16 bit `bx`.
    Abx,
    /// Operand `a` and the signed 16 bit `sbx`.
    AsBx,
    /// The signed 24 bit `ax`.
    Ax,
}

/// What an operand refers to. The verifier checks each kind against the
/// function and the module.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Operand {
    /// Not used. Must be zero.
    None,
    /// A register of the frame.
    Reg,
    /// An index into the constants of the function.
    Const,
    /// A number carried by the instruction. Its meaning is given by the instruction.
    Imm,
    /// A signed jump offset in instructions, relative to the next instruction.
    Jump,
    /// An index into the functions of the module.
    Func,
    /// An index into the native imports of the module.
    Native,
    /// An index into the capability calls of the module.
    Cap,
    /// An index into the record shapes of the module.
    Shape,
    /// An index into the enum variants of the module.
    Variant,
    /// An index into the global slots of the module.
    Global,
    /// An index into the upvalues of the function.
    Upval,
}

macro_rules! opcodes {
    ($($name:ident = $code:literal, $format:ident, [$a:ident, $b:ident, $c:ident], $doc:literal;)+) => {
        /// An opcode.
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub enum Op {
            $(
                #[doc = $doc]
                $name = $code,
            )+
        }

        impl Op {
            /// Every opcode, in numeric order.
            pub const ALL: &'static [Op] = &[$(Op::$name),+];

            /// The opcode with this number.
            #[must_use]
            pub fn from_u8(code: u8) -> Option<Op> {
                match code {
                    $($code => Some(Op::$name),)+
                    _ => None,
                }
            }

            /// The mnemonic.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $(Op::$name => stringify!($name),)+
                }
            }

            /// The format of the operand bytes.
            #[must_use]
            pub const fn format(self) -> Format {
                match self {
                    $(Op::$name => Format::$format,)+
                }
            }

            /// What operands `a`, `b` and `c` refer to. For the formats `Abx`
            /// and `AsBx` the second entry describes the 16 bit operand and
            /// the third is [`Operand::None`].
            #[must_use]
            pub const fn operands(self) -> [Operand; 3] {
                match self {
                    $(Op::$name => [Operand::$a, Operand::$b, Operand::$c],)+
                }
            }
        }
    };
}

opcodes! {
    Move = 0x00, Abc, [Reg, Reg, None], "`R[a]` = `R[b]`.";
    LoadK = 0x01, Abx, [Reg, Const, None], "`R[a]` = `K[bx]`.";
    LoadInt = 0x02, AsBx, [Reg, Imm, None], "`R[a]` = the Int sbx.";
    LoadTrue = 0x03, Abc, [Reg, None, None], "`R[a]` = true.";
    LoadFalse = 0x04, Abc, [Reg, None, None], "`R[a]` = false.";
    LoadNone = 0x05, Abc, [Reg, None, None], "`R[a]` = none.";
    GetGlobal = 0x06, Abx, [Reg, Global, None], "`R[a]` = the global slot bx.";
    SetGlobal = 0x07, Abx, [Reg, Global, None], "The global slot bx = `R[a]`.";
    NewCell = 0x08, Abc, [Reg, Reg, None], "`R[a]` = a new cell holding `R[b]`. A binding that a closure captures lives in a cell.";
    GetCell = 0x09, Abc, [Reg, Reg, None], "`R[a]` = the value of the cell `R[b]`.";
    SetCell = 0x0A, Abc, [Reg, Reg, None], "The value of the cell `R[a]` = `R[b]`.";
    GetUpval = 0x0B, Abc, [Reg, Upval, None], "`R[a]` = the value of the cell of upvalue b.";
    SetUpval = 0x0C, Abc, [Reg, Upval, None], "The value of the cell of upvalue b = `R[a]`.";

    IntToFloat = 0x10, Abc, [Reg, Reg, None], "`R[a]` = the Float nearest to the Int `R[b]`.";
    AddInt = 0x11, Abc, [Reg, Reg, Reg], "`R[a]` = `R[b]` + `R[c]` on Int, faulting HS1007 on overflow.";
    SubInt = 0x12, Abc, [Reg, Reg, Reg], "`R[a]` = `R[b]` - `R[c]` on Int, faulting HS1007 on overflow.";
    MulInt = 0x13, Abc, [Reg, Reg, Reg], "`R[a]` = `R[b]` * `R[c]` on Int, faulting HS1007 on overflow.";
    NegInt = 0x14, Abc, [Reg, Reg, None], "`R[a]` = -`R[b]` on Int, faulting HS1007 on overflow.";
    AddFloat = 0x15, Abc, [Reg, Reg, Reg], "`R[a]` = `R[b]` + `R[c]` on Float, with the checks of section 3.4.";
    SubFloat = 0x16, Abc, [Reg, Reg, Reg], "`R[a]` = `R[b]` - `R[c]` on Float, with the checks of section 3.4.";
    MulFloat = 0x17, Abc, [Reg, Reg, Reg], "`R[a]` = `R[b]` * `R[c]` on Float, with the checks of section 3.4.";
    NegFloat = 0x18, Abc, [Reg, Reg, None], "`R[a]` = -`R[b]` on Float.";
    Div = 0x19, Abc, [Reg, Reg, Reg], "`R[a]` = `R[b]` / `R[c]` after converting both to Float, faulting HS1008 on a zero divisor.";
    Eq = 0x1A, Abc, [Reg, Reg, Reg], "`R[a]` = whether `R[b]` equals `R[c]`, charged by tree size (section 11.3).";
    Ne = 0x1B, Abc, [Reg, Reg, Reg], "`R[a]` = whether `R[b]` differs from `R[c]`.";
    Lt = 0x1C, Abc, [Reg, Reg, Reg], "`R[a]` = whether `R[b]` < `R[c]`, on numbers or strings.";
    Le = 0x1D, Abc, [Reg, Reg, Reg], "`R[a]` = whether `R[b]` <= `R[c]`.";
    Gt = 0x1E, Abc, [Reg, Reg, Reg], "`R[a]` = whether `R[b]` > `R[c]`.";
    Ge = 0x1F, Abc, [Reg, Reg, Reg], "`R[a]` = whether `R[b]` >= `R[c]`.";
    Not = 0x20, Abc, [Reg, Reg, None], "`R[a]` = not `R[b]`.";
    ToText = 0x21, Abc, [Reg, Reg, None], "`R[a]` = the text of the Int, Float, Bool or String `R[b]` (section 3.9).";
    Interp = 0x22, Abc, [Reg, Reg, Imm], "`R[a]` = the concatenation of the texts (section 3.9) of the c values `R[b..b+c)`, charged as one allocation and only as one, so the text of an individual part costs nothing.";

    Jump = 0x30, AsBx, [None, Jump, None], "Jump by sbx.";
    JumpIf = 0x31, AsBx, [Reg, Jump, None], "Jump by sbx if `R[a]` is true.";
    JumpIfNot = 0x32, AsBx, [Reg, Jump, None], "Jump by sbx if `R[a]` is false.";
    JumpIfNone = 0x33, AsBx, [Reg, Jump, None], "Jump by sbx if `R[a]` is none.";
    JumpIfSome = 0x34, AsBx, [Reg, Jump, None], "Jump by sbx if `R[a]` is not none.";
    Charge = 0x35, Abx, [None, Imm, None], "Charge bx steps, from 1 to 1024, then apply the safepoint rule.";
    IterInit = 0x36, Abc, [Reg, Reg, None], "Start iterating the list, set or map `R[b]`: `R[a]` = the collection, `R[a+1]` = 0.";
    IterNext = 0x37, AsBx, [Reg, Jump, None], "Charge 1 for the iteration. If `R[a+1]` is at the end of `R[a]`, jump by sbx. Otherwise `R[a+2]` = the element (a `MapEntry` record for a map) and `R[a+1]` += 1.";
    JumpLong = 0x38, Ax, [Jump, None, None], "Jump by the signed 24 bit ax.";

    NewList = 0x40, Abx, [Reg, Imm, None], "`R[a]` = the list of the bx values `R[a+1..a+1+bx)`.";
    ListAppend = 0x41, Abc, [Reg, Reg, Imm], "The list `R[a]`, extended in place by the c values `R[b..b+c)`, charging 8 bytes for each (section 11.4) and no steps. Builds a long list literal after its first `NewList`, on a list no other register or value can yet see.";
    NewMap = 0x42, Abx, [Reg, Imm, None], "`R[a]` = the map of the bx key and value pairs `R[a+1..a+1+2*bx)`. A later pair replaces an earlier one with the same key.";
    MapPut = 0x43, Abc, [Reg, Reg, Imm], "The map `R[a]`, extended in place by the c key and value pairs `R[b..b+2*c)` (a later pair replaces an earlier one with the same key), charging 24 bytes for each new key (section 11.4) and one step per 64 existing entries it had to move. Builds a long map literal after its first `NewMap`, on a map no other register or value can yet see.";
    NewRecord = 0x44, Abx, [Reg, Shape, None], "`R[a]` = the record of shape bx with the field values `R[a+1..]`, in the order of the shape.";
    NewEnum = 0x45, Abx, [Reg, Variant, None], "`R[a]` = the enum value of variant bx with the payload `R[a+1..]`.";
    GetField = 0x46, Abc, [Reg, Reg, Imm], "`R[a]` = the field number c, in the order of the shape, of the record `R[b]`.";
    Index = 0x47, Abc, [Reg, Reg, Reg], "`R[a]` = the result of indexing `R[b]` with `R[c]`, for a list or map: an optional, none when absent.";
    VariantIs = 0x48, Abc, [Reg, Reg, Imm], "`R[a]` = whether the enum value `R[b]` is variant number c of its enum.";
    GetPayload = 0x49, Abc, [Reg, Reg, Imm], "`R[a]` = payload number c of the enum value `R[b]`.";
    ListLen = 0x4A, Abc, [Reg, Reg, None], "`R[a]` = the length of the list `R[b]`.";
    ListGet = 0x4B, Abc, [Reg, Reg, Imm], "`R[a]` = element c of the list `R[b]`, which the program has checked exists.";
    ListRest = 0x4C, Abc, [Reg, Reg, Imm], "`R[a]` = the list `R[b]` without its first c elements.";
    ListCheck = 0x4D, Abc, [Reg, Imm, Imm], "Fault HS1014 unless the list `R[a]` has exactly b elements, or at least b when c is 1.";
    IsNone = 0x4E, Abc, [Reg, Reg, None], "`R[a]` = whether `R[b]` is none.";

    Closure = 0x50, Abx, [Reg, Func, None], "`R[a]` = a closure of function bx, capturing the cells its upvalue list names.";
    VariantFn = 0x51, Abx, [Reg, Variant, None], "`R[a]` = a function value that builds variant bx.";
    NativeFn = 0x52, Abx, [Reg, Native, None], "`R[a]` = a function value of the native import bx.";
    CapFn = 0x53, Abx, [Reg, Cap, None], "`R[a]` = a function value of the capability call bx.";
    Call = 0x54, Abc, [Reg, Imm, None], "Call the function value `R[a]` with the b arguments `R[a+1..]`. The result, if any, is `R[a]`. Charges a call.";
    CallFn = 0x55, Abx, [Reg, Func, None], "Call function bx with the arguments `R[a+1..]`. Charges a call.";
    CallNative = 0x56, Abx, [Reg, Native, None], "Call the native import bx with the arguments `R[a+1..]`, charging its cost.";
    CallCap = 0x57, Abx, [Reg, Cap, None], "Call the capability call bx with the arguments `R[a+1..]`, charging as section 11.3 says.";
    Return = 0x58, Abc, [Reg, None, None], "Return `R[a]`.";
    ReturnNone = 0x59, Abc, [None, None, None], "Return without a value.";
    Try = 0x5A, Abc, [Reg, Reg, None], "The operator ?. If `R[b]` is Err(x) return Err(x) from the function. Otherwise `R[a]` = the Ok payload.";
    Trap = 0x5B, Abx, [None, Imm, None], "Fault HS1099. Marks code that a correct program never reaches.";

    Ext = 0x60, Abc, [Imm, Imm, Imm], "Not an instruction of its own: the high bytes of the operands of the instruction that follows, which together with it is one instruction (see the module documentation).";
}

impl Op {
    /// Whether execution can continue with the next instruction. False for
    /// returns, traps and the unconditional jump.
    #[must_use]
    pub const fn falls_through(self) -> bool {
        !matches!(
            self,
            Op::Return | Op::ReturnNone | Op::Trap | Op::Jump | Op::JumpLong
        )
    }

    /// Whether the instruction transfers control by a jump offset.
    #[must_use]
    pub const fn is_jump(self) -> bool {
        matches!(
            self,
            Op::Jump
                | Op::JumpLong
                | Op::JumpIf
                | Op::JumpIfNot
                | Op::JumpIfNone
                | Op::JumpIfSome
                | Op::IterNext
        )
    }

    /// Whether executing the instruction always charges at least one step, so
    /// that a cycle through it reaches a safepoint (section 11.7).
    ///
    /// **Every call-like opcode must stay in this list.** `verify::check_cycles`
    /// only reasons about jumps within one function's own instruction stream;
    /// the reason a recursive call cannot loop forever without ever reaching a
    /// safepoint is entirely that `Call`, `CallFn`, `CallNative` and `CallCap`
    /// are all charging instructions, so a cycle of recursive calls charges on
    /// every step around it even though `check_cycles` never looks at the
    /// call graph across functions. Removing one of these four from this list,
    /// or adding a new call-like opcode without adding it here, would silently
    /// reopen an infinite, uncancellable loop across function boundaries that
    /// no verifier test (all of which are single-function) would catch; see
    /// `tests::every_call_like_opcode_charges` below.
    #[must_use]
    pub const fn charges(self) -> bool {
        matches!(
            self,
            Op::Charge | Op::IterNext | Op::Call | Op::CallFn | Op::CallNative | Op::CallCap
        )
    }
}

/// One decoded instruction.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Instruction {
    /// The opcode.
    pub op: Op,
    /// Operand `a`.
    pub a: u8,
    /// Operand `b`, or the low byte of `bx` and `sbx`.
    pub b: u8,
    /// Operand `c`, or the high byte of `bx` and `sbx`.
    pub c: u8,
}

impl Instruction {
    /// An instruction of format `Abc`.
    #[must_use]
    pub const fn abc(op: Op, a: u8, b: u8, c: u8) -> Instruction {
        Instruction { op, a, b, c }
    }

    /// An instruction of format `Abx`.
    #[must_use]
    pub const fn abx(op: Op, a: u8, bx: u16) -> Instruction {
        let [b, c] = bx.to_le_bytes();
        Instruction { op, a, b, c }
    }

    /// An instruction of format `AsBx`.
    #[must_use]
    pub const fn asbx(op: Op, a: u8, sbx: i16) -> Instruction {
        let [b, c] = sbx.to_le_bytes();
        Instruction { op, a, b, c }
    }

    /// An instruction of format `Ax`, or `None` when `ax` does not fit in 24 bits.
    #[must_use]
    pub const fn try_ax(op: Op, ax: i32) -> Option<Instruction> {
        if ax < -(1 << 23) || ax >= (1 << 23) {
            return None;
        }
        let [a, b, c, _] = ax.to_le_bytes();
        Some(Instruction { op, a, b, c })
    }

    /// The signed 24 bit operand.
    #[must_use]
    pub const fn ax(self) -> i32 {
        let sign = if self.c & 0x80 != 0 { 0xFF } else { 0 };
        i32::from_le_bytes([self.a, self.b, self.c, sign])
    }

    /// The unsigned 16 bit operand.
    #[must_use]
    pub const fn bx(self) -> u16 {
        u16::from_le_bytes([self.b, self.c])
    }

    /// The signed 16 bit operand.
    #[must_use]
    pub const fn sbx(self) -> i16 {
        i16::from_le_bytes([self.b, self.c])
    }

    /// The instruction as a word.
    #[must_use]
    pub const fn encode(self) -> u32 {
        u32::from_le_bytes([self.op as u8, self.a, self.b, self.c])
    }

    /// The instruction of a word, or the unknown opcode.
    ///
    /// # Errors
    ///
    /// Returns the opcode byte when it is not an opcode.
    pub fn decode(word: u32) -> Result<Instruction, u8> {
        let [code, a, b, c] = word.to_le_bytes();
        Op::from_u8(code)
            .map(|op| Instruction { op, a, b, c })
            .ok_or(code)
    }
}

impl Instruction {
    /// The words of an `Abc` instruction whose operands may need more than
    /// one byte: the instruction alone, or an [`Op::Ext`] prefix and the
    /// instruction, in that order.
    #[must_use]
    pub const fn wide_abc(op: Op, a: u16, b: u16, c: u16) -> (Option<Instruction>, Instruction) {
        let [a0, a1] = a.to_le_bytes();
        let [b0, b1] = b.to_le_bytes();
        let [c0, c1] = c.to_le_bytes();
        let main = Instruction::abc(op, a0, b0, c0);
        if a1 == 0 && b1 == 0 && c1 == 0 {
            (None, main)
        } else {
            (Some(Instruction::abc(Op::Ext, a1, b1, c1)), main)
        }
    }

    /// The words of an `Abx` instruction whose `a` may need two bytes and
    /// whose `bx` may need up to four; see [`Instruction::wide_abc`].
    #[must_use]
    pub const fn wide_abx(op: Op, a: u16, bx: u32) -> (Option<Instruction>, Instruction) {
        let [a0, a1] = a.to_le_bytes();
        let [b0, c0, b1, c1] = bx.to_le_bytes();
        let main = Instruction::abc(op, a0, b0, c0);
        if a1 == 0 && b1 == 0 && c1 == 0 {
            (None, main)
        } else {
            (Some(Instruction::abc(Op::Ext, a1, b1, c1)), main)
        }
    }

    /// The words of an `AsBx` instruction whose `a` may need two bytes; the
    /// signed operand is never widened (a jump that needs more is a
    /// [`Op::JumpLong`], a number that needs more is a constant).
    #[must_use]
    pub const fn wide_asbx(op: Op, a: u16, sbx: i16) -> (Option<Instruction>, Instruction) {
        let [a0, a1] = a.to_le_bytes();
        let main = Instruction::asbx(op, a0, sbx);
        if a1 == 0 {
            (None, main)
        } else {
            (Some(Instruction::abc(Op::Ext, a1, 0, 0)), main)
        }
    }
}

/// One instruction with the high operand bytes of its [`Op::Ext`] prefix, if
/// it had one, already applied: what the verifier checks and the interpreter
/// runs. `b` and `c` hold the raw widened bytes of the word; read an `Abx`
/// operand with [`Wide::bx`], an `AsBx` one with [`Wide::sbx`] and an `Ax`
/// one with [`Wide::ax`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Wide {
    /// The opcode, never [`Op::Ext`].
    pub op: Op,
    /// Operand `a`.
    pub a: u16,
    /// Operand `b`: its low byte is the word's `b`, its high byte the prefix's.
    pub b: u16,
    /// Operand `c`: its low byte is the word's `c`, its high byte the prefix's.
    pub c: u16,
}

impl Wide {
    /// `main` with the high bytes `ext` supplies, or with none.
    #[must_use]
    #[inline]
    pub const fn new(ext: Option<Instruction>, main: Instruction) -> Wide {
        let (a1, b1, c1) = match ext {
            Some(e) => (e.a, e.b, e.c),
            None => (0, 0, 0),
        };
        Wide {
            op: main.op,
            a: u16::from_le_bytes([main.a, a1]),
            b: u16::from_le_bytes([main.b, b1]),
            c: u16::from_le_bytes([main.c, c1]),
        }
    }

    /// The unsigned `bx` of an `Abx` instruction, up to 32 bits wide.
    #[must_use]
    #[inline]
    pub const fn bx(self) -> u32 {
        let [b0, b1] = self.b.to_le_bytes();
        let [c0, c1] = self.c.to_le_bytes();
        u32::from_le_bytes([b0, c0, b1, c1])
    }

    /// The signed 16 bit `sbx` of an `AsBx` instruction.
    #[must_use]
    #[inline]
    pub const fn sbx(self) -> i16 {
        let [b0, _] = self.b.to_le_bytes();
        let [c0, _] = self.c.to_le_bytes();
        i16::from_le_bytes([b0, c0])
    }

    /// The signed 24 bit `ax` of an `Ax` instruction.
    #[must_use]
    #[inline]
    pub const fn ax(self) -> i32 {
        let [a0, _] = self.a.to_le_bytes();
        let [b0, _] = self.b.to_le_bytes();
        let [c0, _] = self.c.to_le_bytes();
        Instruction::abc(self.op, a0, b0, c0).ax()
    }
}

impl fmt::Display for Wide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:<10}", self.op.name())?;
        match self.op.format() {
            Format::Abc => write!(f, " {} {} {}", self.a, self.b, self.c),
            Format::Abx => write!(f, " {} {}", self.a, self.bx()),
            Format::AsBx => write!(f, " {} {}", self.a, self.sbx()),
            Format::Ax => write!(f, " {}", self.ax()),
        }
    }
}

impl fmt::Display for Instruction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:<10}", self.op.name())?;
        match self.op.format() {
            Format::Abc => write!(f, " {} {} {}", self.a, self.b, self.c),
            Format::Abx => write!(f, " {} {}", self.a, self.bx()),
            Format::AsBx => write!(f, " {} {}", self.a, self.sbx()),
            Format::Ax => write!(f, " {}", self.ax()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Format, Instruction, Op, Operand, Wide};
    use std::collections::BTreeSet;

    #[test]
    fn opcodes_are_distinct_and_round_trip() {
        let codes: BTreeSet<u8> = Op::ALL.iter().map(|o| *o as u8).collect();
        assert_eq!(codes.len(), Op::ALL.len());
        for op in Op::ALL {
            assert_eq!(Op::from_u8(*op as u8), Some(*op));
        }
        assert_eq!(Op::from_u8(0xFF), None);
    }

    #[test]
    fn instructions_round_trip_through_words() {
        let cases = [
            Instruction::abc(Op::AddInt, 1, 2, 3),
            Instruction::abx(Op::LoadK, 7, 0xBEEF),
            Instruction::asbx(Op::Jump, 0, -5),
            Instruction::asbx(Op::JumpIf, 200, i16::MAX),
        ];
        for i in cases {
            assert_eq!(Instruction::decode(i.encode()), Ok(i));
        }
        assert_eq!(Instruction::abx(Op::LoadK, 0, 0x1234).bx(), 0x1234);
        assert_eq!(Instruction::asbx(Op::Jump, 0, -2).sbx(), -2);
        assert_eq!(Instruction::decode(0x0000_00FF), Err(0xFF));
        for v in [0, 1, -1, 8_388_607, -8_388_608] {
            assert_eq!(
                Instruction::try_ax(Op::JumpLong, v).map(Instruction::ax),
                Some(v)
            );
        }
        assert_eq!(Instruction::try_ax(Op::JumpLong, 8_388_608), None);
        assert_eq!(Instruction::try_ax(Op::JumpLong, -8_388_609), None);
    }

    fn widened(pair: (Option<Instruction>, Instruction)) -> Wide {
        let (ext, main) = pair;
        // A prefix survives its own round trip through a word as well.
        let ext = ext.map(|e| Instruction::decode(e.encode()).unwrap());
        assert!(ext.is_none_or(|e| e.op == Op::Ext));
        Wide::new(ext, Instruction::decode(main.encode()).unwrap())
    }

    #[test]
    fn wide_operands_round_trip_through_a_prefix() {
        for (a, b, c) in [(0, 0, 0), (255, 255, 255), (256, 1, 2), (7, 65_535, 300)] {
            let (ext, _) = Instruction::wide_abc(Op::AddInt, a, b, c);
            assert_eq!(ext.is_some(), a > 255 || b > 255 || c > 255);
            let w = widened(Instruction::wide_abc(Op::AddInt, a, b, c));
            assert_eq!((w.op, w.a, w.b, w.c), (Op::AddInt, a, b, c));
        }
        for (a, bx) in [
            (0, 0),
            (255, 65_535),
            (256, 65_536),
            (65_535, u32::MAX),
            (3, 70_000),
        ] {
            let (ext, _) = Instruction::wide_abx(Op::LoadK, a, bx);
            assert_eq!(ext.is_some(), a > 255 || bx > 65_535);
            let w = widened(Instruction::wide_abx(Op::LoadK, a, bx));
            assert_eq!((w.op, w.a, w.bx()), (Op::LoadK, a, bx));
        }
        for (a, sbx) in [(0, -1), (300, i16::MIN), (65_535, i16::MAX)] {
            let (ext, _) = Instruction::wide_asbx(Op::JumpIfNot, a, sbx);
            assert_eq!(ext.is_some(), a > 255);
            let w = widened(Instruction::wide_asbx(Op::JumpIfNot, a, sbx));
            assert_eq!((w.a, w.sbx()), (a, sbx));
        }
        let long = Instruction::try_ax(Op::JumpLong, -70_000).unwrap();
        assert_eq!(Wide::new(None, long).ax(), -70_000);
        assert_eq!(
            widened(Instruction::wide_abc(Op::Move, 300, 2, 0))
                .to_string()
                .trim_end(),
            "Move       300 2 0"
        );
    }

    #[test]
    fn operand_tables_match_the_formats() {
        for op in Op::ALL {
            let [a, b, c] = op.operands();
            match op.format() {
                Format::Abc => {}
                Format::Abx | Format::AsBx => assert_eq!(c, Operand::None, "{}", op.name()),
                Format::Ax => assert_eq!((b, c), (Operand::None, Operand::None)),
            }
            if op.format() == Format::AsBx {
                assert!(matches!(b, Operand::Jump | Operand::Imm), "{}", op.name());
            }
            if op.is_jump() && op.format() == Format::AsBx {
                assert_eq!(b, Operand::Jump);
            }
            assert!(a != Operand::Jump || op.format() == Format::Ax);
        }
    }

    #[test]
    fn control_flow_properties() {
        assert!(!Op::Return.falls_through());
        assert!(!Op::Jump.falls_through());
        assert!(Op::JumpIf.falls_through());
        assert!(Op::IterNext.charges() && Op::Charge.charges() && Op::Call.charges());
        assert!(!Op::Move.charges());
        assert_eq!(
            Instruction::abc(Op::Move, 1, 2, 0).to_string().trim_end(),
            "Move       1 2 0"
        );
    }

    /// `verify::check_cycles` is sound across function boundaries only
    /// because every call-family opcode charges (see the doc comment on
    /// [`Op::charges`]): this mechanically pins the set, so a future opcode
    /// addition or a `charges()` edit cannot silently drop one of them and
    /// reopen an uncancellable cross-function loop without a test failing
    /// here, rather than resting on a reviewer remembering the invariant.
    #[test]
    fn every_call_like_opcode_charges() {
        for op in [Op::Call, Op::CallFn, Op::CallNative, Op::CallCap] {
            assert!(op.charges(), "{} must charge steps", op.name());
        }
    }
}

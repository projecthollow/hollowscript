//! The structural limits of section 1.10, as arithmetic the parser applies.
//!
//! Three numbers keep every pass over a tree within a fixed stack:
//!
//! - the *depth* of a token is the number of open constructs around it
//!   (parentheses, brackets, braces, blocks, patterns, the angle brackets of
//!   type arguments and the `${` `}` of interpolations), counted together, and
//!   a token at depth 251 is HS0113;
//! - a *chain* is a run of one binary operator level, one postfix chain or one
//!   `else if` run, and a chain of 501 links is HS0114;
//! - the *weight* of a token is its depth plus the links of every chain that
//!   encloses it as an operand, and a token of weight above 1,000 is HS0113.
//!
//! Chains lean left in the tree, so a chain is deep as well as long. The
//! weight is what bounds the recursion of every later pass at 1,000 levels.
//! The parser reads chains with a loop, so its own recursion is bounded by the
//! depth alone.

use hollowscript_core::Code;
use hollowscript_core::limits::structural::{MAX_CHAIN_LINKS, MAX_NESTING_DEPTH, MAX_TOKEN_WEIGHT};

/// The count of open constructs while parsing.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Depth(u32);

impl Depth {
    /// The current depth.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Opens a construct.
    ///
    /// # Errors
    ///
    /// Returns HS0113 when the construct would put a token at depth 251. The
    /// depth is unchanged in that case, and the construct is parsed no further.
    pub fn enter(&mut self) -> Result<(), Code> {
        if self.0 >= MAX_NESTING_DEPTH {
            return Err(Code::HS0113);
        }
        self.0 += 1;
        Ok(())
    }

    /// Closes the innermost construct.
    pub fn exit(&mut self) {
        self.0 = self.0.saturating_sub(1);
    }
}

/// Checks the number of links of a finished chain.
///
/// # Errors
///
/// Returns HS0114 when the chain has more than 500 links.
pub fn check_chain(links: u32) -> Result<(), Code> {
    if links > MAX_CHAIN_LINKS {
        Err(Code::HS0114)
    } else {
        Ok(())
    }
}

/// The weight of the operands of a chain, seen from outside the chain: the
/// heaviest operand plus the links of the chain.
#[must_use]
pub fn chain_weight(links: u32, heaviest_operand: u32) -> u32 {
    heaviest_operand.saturating_add(links)
}

/// Checks the weight of the heaviest token inside a construct that itself
/// stands at `depth`.
///
/// # Errors
///
/// Returns HS0113 when the weight is above 1,000.
pub fn check_weight(depth: Depth, weight_inside: u32) -> Result<(), Code> {
    if depth.get().saturating_add(weight_inside) > MAX_TOKEN_WEIGHT {
        Err(Code::HS0113)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Depth, chain_weight, check_chain, check_weight};
    use hollowscript_core::Code;

    #[test]
    fn depth_250_is_allowed_and_251_is_not() {
        let mut d = Depth::default();
        for _ in 0..250 {
            assert_eq!(d.enter(), Ok(()));
        }
        assert_eq!(d.get(), 250);
        assert_eq!(d.enter(), Err(Code::HS0113));
        assert_eq!(
            d.get(),
            250,
            "a refused construct does not change the depth"
        );
        d.exit();
        assert_eq!(d.enter(), Ok(()));
        for _ in 0..300 {
            d.exit();
        }
        assert_eq!(d.get(), 0);
    }

    #[test]
    fn chains_of_500_links_are_allowed() {
        assert_eq!(check_chain(500), Ok(()));
        assert_eq!(check_chain(501), Err(Code::HS0114));
        assert_eq!(check_chain(0), Ok(()));
    }

    #[test]
    fn weight_adds_depth_and_chain_links() {
        let mut d = Depth::default();
        d.enter().unwrap();
        let heaviest_operand = 499; // three operands deep inside one chain of 500 links
        let w = chain_weight(500, heaviest_operand);
        assert_eq!(w, 999);
        assert_eq!(check_weight(d, w), Ok(()));
        assert_eq!(check_weight(d, w + 1), Err(Code::HS0113));
        assert_eq!(chain_weight(u32::MAX, 5), u32::MAX);
    }
}

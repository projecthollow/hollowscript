//! Interned identifiers.
//!
//! Every name in the syntax tree, the checker and the compiler is a
//! [`Symbol`], a small integer that stands for one string of one
//! [`Interner`]. Comparing two symbols is comparing two integers. An interner
//! belongs to one program, and symbols of different interners must not be mixed.

#[allow(clippy::disallowed_types, reason = "lookup only; never iterated")]
use std::collections::HashMap;
use std::fmt;
use std::num::NonZeroU32;
use std::sync::Arc;

/// An interned string. The order of symbols is the order of first interning
/// and is not the order of the strings.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Symbol(NonZeroU32);

impl Symbol {
    /// The index of the symbol, starting at 0.
    #[must_use]
    pub fn index(self) -> u32 {
        self.0.get() - 1
    }
}

impl fmt::Debug for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Symbol({})", self.index())
    }
}

/// A table of interned strings.
#[derive(Debug, Default)]
pub struct Interner {
    /// Only looked up, never iterated, so its unstable order cannot be observed.
    #[allow(clippy::disallowed_types, reason = "lookup only; never iterated")]
    map: HashMap<Arc<str>, Symbol>,
    strings: Vec<Arc<str>>,
}

impl Interner {
    /// An empty interner.
    #[must_use]
    pub fn new() -> Interner {
        Interner::default()
    }

    /// The symbol of `text`, adding it if it is new.
    ///
    /// # Panics
    ///
    /// Never. When more than `u32::MAX - 1` strings are interned, which the
    /// source size limits make impossible, the last symbol is reused.
    pub fn intern(&mut self, text: &str) -> Symbol {
        if let Some(&symbol) = self.map.get(text) {
            return symbol;
        }
        let next = u32::try_from(self.strings.len() + 1).unwrap_or(u32::MAX);
        let symbol = Symbol(NonZeroU32::new(next).unwrap_or(NonZeroU32::MIN));
        let shared: Arc<str> = Arc::from(text);
        self.strings.push(shared.clone());
        self.map.insert(shared, symbol);
        symbol
    }

    /// The symbol of `text` if it was interned before.
    #[must_use]
    pub fn get(&self, text: &str) -> Option<Symbol> {
        self.map.get(text).copied()
    }

    /// The string of a symbol of this interner.
    #[must_use]
    pub fn resolve(&self, symbol: Symbol) -> &str {
        self.strings.get(symbol.index() as usize).map_or("", |s| s)
    }

    /// The number of interned strings.
    #[must_use]
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    /// Whether nothing is interned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::Interner;

    #[test]
    fn equal_strings_share_a_symbol() {
        let mut i = Interner::new();
        let a = i.intern("alpha");
        let b = i.intern("beta");
        assert_ne!(a, b);
        assert_eq!(i.intern("alpha"), a);
        assert_eq!(i.resolve(a), "alpha");
        assert_eq!(i.resolve(b), "beta");
        assert_eq!(i.get("gamma"), None);
        assert_eq!(i.len(), 2);
        assert_eq!(a.index(), 0);
    }

    #[test]
    fn the_empty_string_is_a_symbol() {
        let mut i = Interner::new();
        let e = i.intern("");
        assert_eq!(i.resolve(e), "");
        assert_eq!(Some(e), i.get(""));
    }
}

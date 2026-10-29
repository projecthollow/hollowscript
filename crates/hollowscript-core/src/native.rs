//! The roster of the standard library.
//!
//! Every native function of chapters 8 and 13 to 15 has one entry here, keyed
//! by name: a free function is `abs`, a method is written with the type of its
//! receiver, as in `list.map`, and a function of a namespace as in
//! `json.parse`. The roster is the one list that the three crates that deal
//! with the library share:
//!
//! - the checker gives each entry its type signature and rejects a name that
//!   is not here;
//! - the compiler writes the names it uses into the module's import table;
//! - the VM resolves each imported name at load time, refuses a name it does
//!   not implement or an arity that differs from the roster, and charges the
//!   cost that the specification gives.
//!
//! A method counts its receiver in the arity. The overloads of `sort` are the
//! two entries `list.sort` and `list.sortBy`. `success`, `failure` and the
//! constant `pi` are not natives: the compiler builds them.

/// One function of the standard library.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct NativeInfo {
    /// The roster name.
    pub name: &'static str,
    /// The number of arguments, receiver included.
    pub arity: u8,
    /// Whether the function calls script closures. The VM runs such a function
    /// as a continuation on its own frame stack instead of recursing.
    pub callbacks: bool,
}

/// Every native function, ordered by name so that a lookup is a binary search.
pub static ROSTER: &[NativeInfo] = &[
    NativeInfo {
        name: "abs",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "atan2",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "ceil",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "clamp",
        arity: 3,
        callbacks: false,
    },
    NativeInfo {
        name: "cos",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "div",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "exp",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "fixed",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "floor",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "has",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "ipow",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "json.at",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "json.field",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "json.parse",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "json.pretty",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "json.stringify",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "keys",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "list.all",
        arity: 2,
        callbacks: true,
    },
    NativeInfo {
        name: "list.any",
        arity: 2,
        callbacks: true,
    },
    NativeInfo {
        name: "list.append",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "list.concat",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "list.contains",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "list.drop",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "list.each",
        arity: 2,
        callbacks: true,
    },
    NativeInfo {
        name: "list.filter",
        arity: 2,
        callbacks: true,
    },
    NativeInfo {
        name: "list.find",
        arity: 2,
        callbacks: true,
    },
    NativeInfo {
        name: "list.first",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "list.flatMap",
        arity: 2,
        callbacks: true,
    },
    NativeInfo {
        name: "list.indexOf",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "list.isEmpty",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "list.join",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "list.last",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "list.length",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "list.map",
        arity: 2,
        callbacks: true,
    },
    NativeInfo {
        name: "list.reduce",
        arity: 3,
        callbacks: true,
    },
    NativeInfo {
        name: "list.reverse",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "list.slice",
        arity: 3,
        callbacks: false,
    },
    NativeInfo {
        name: "list.sort",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "list.sortBy",
        arity: 2,
        callbacks: true,
    },
    NativeInfo {
        name: "list.take",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "ln",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "map.each",
        arity: 2,
        callbacks: true,
    },
    NativeInfo {
        name: "map.entries",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "map.filter",
        arity: 2,
        callbacks: true,
    },
    NativeInfo {
        name: "map.get",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "map.has",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "map.isEmpty",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "map.keys",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "map.length",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "map.mapValues",
        arity: 2,
        callbacks: true,
    },
    NativeInfo {
        name: "map.merge",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "map.values",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "map.with",
        arity: 3,
        callbacks: false,
    },
    NativeInfo {
        name: "map.without",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "mapFrom",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "max",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "min",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "mod",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "parseFloat",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "parseInt",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "pow",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "range",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "remainder",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "result.andThen",
        arity: 2,
        callbacks: true,
    },
    NativeInfo {
        name: "result.err",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "result.isErr",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "result.isOk",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "result.map",
        arity: 2,
        callbacks: true,
    },
    NativeInfo {
        name: "result.mapError",
        arity: 2,
        callbacks: true,
    },
    NativeInfo {
        name: "result.ok",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "result.unwrapOr",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "round",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "set",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "set.difference",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "set.each",
        arity: 2,
        callbacks: true,
    },
    NativeInfo {
        name: "set.filter",
        arity: 2,
        callbacks: true,
    },
    NativeInfo {
        name: "set.has",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "set.intersect",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "set.isEmpty",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "set.isSubsetOf",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "set.length",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "set.toList",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "set.union",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "set.with",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "set.without",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "sign",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "sin",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "sqrt",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "string.at",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "string.byteLength",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "string.chars",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "string.contains",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "string.endsWith",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "string.indexOf",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "string.isEmpty",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "string.length",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "string.lines",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "string.padEnd",
        arity: 3,
        callbacks: false,
    },
    NativeInfo {
        name: "string.padStart",
        arity: 3,
        callbacks: false,
    },
    NativeInfo {
        name: "string.repeat",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "string.replace",
        arity: 3,
        callbacks: false,
    },
    NativeInfo {
        name: "string.split",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "string.startsWith",
        arity: 2,
        callbacks: false,
    },
    NativeInfo {
        name: "string.substring",
        arity: 3,
        callbacks: false,
    },
    NativeInfo {
        name: "string.toLower",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "string.toUpper",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "string.trim",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "string.trimEnd",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "string.trimStart",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "tan",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "time.format",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "time.formatDate",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "time.fromParts",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "time.parse",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "time.parseDate",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "time.parts",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "toFloat",
        arity: 1,
        callbacks: false,
    },
    NativeInfo {
        name: "trunc",
        arity: 1,
        callbacks: false,
    },
];

/// The entry with this name.
#[must_use]
pub fn lookup(name: &str) -> Option<&'static NativeInfo> {
    ROSTER
        .binary_search_by(|n| n.name.cmp(name))
        .ok()
        .and_then(|i| ROSTER.get(i))
}

#[cfg(test)]
mod tests {
    use super::{ROSTER, lookup};

    #[test]
    fn the_roster_is_sorted_and_free_of_duplicates() {
        assert!(ROSTER.windows(2).all(|w| w[0].name < w[1].name));
        assert!(ROSTER.iter().all(|n| n.arity >= 1 || n.name == "range"));
    }

    #[test]
    fn lookups_find_entries() {
        assert_eq!(
            lookup("list.map").map(|n| (n.arity, n.callbacks)),
            Some((2, true))
        );
        assert_eq!(lookup("string.length").map(|n| n.arity), Some(1));
        assert_eq!(lookup("clamp").map(|n| n.arity), Some(3));
        assert_eq!(lookup("nope"), None);
        assert_eq!(ROSTER.len(), 118);
    }
}

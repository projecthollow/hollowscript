//! Parses the textual grant grammar of section 9.5:
//!
//! ```text
//! grant = name [ ":" kind ] [ "=" scope { "," scope } ] ;
//! ```
//!
//! Given once per `--allow` flag. The text is split at the first `:` that
//! comes before any `=`, then at the first `=`; the scope is split at each
//! `,`. No white space is trimmed, so `fs: read=./data` (a space after the
//! colon) is a kind literally named `" read"`, which the embedder then
//! refuses as unknown, exactly as the specification requires of a malformed grant.

/// One `--allow` flag, parsed but not yet checked against a capability's own
/// rules (whether it takes a kind, whether a scope is required): that check
/// belongs to the embedder, which knows what capabilities exist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Grant {
    /// The capability name.
    pub name: String,
    /// The kind, if a `:` was written.
    pub kind: Option<String>,
    /// The scope values, if a `=` was written. Empty and absent are
    /// different: `fs:read` has no scope list at all, while `fs:read=` has
    /// one scope value that is the empty string, which every capability that
    /// takes a scope rejects (section 9.5, "an empty scope value").
    pub scopes: Option<Vec<String>>,
}

/// A grant flag that does not even have the right shape (an empty name, or
/// an empty element of the scope list). Everything else (an unknown
/// capability, an unknown kind, an empty scope value) is for the embedder to
/// reject, because only it knows the capability's own rules.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum GrantSyntaxError {
    /// The text before any `:` or `=` was empty.
    EmptyName,
    /// Two scope values were separated by `,,`, or the list started or ended with `,`.
    EmptyScopeElement,
}

/// Parses one `--allow` argument.
///
/// # Errors
///
/// Returns [`GrantSyntaxError`] for a flag with no name at all, or an empty
/// element in its scope list. An empty single scope value (`fs:read=`) is
/// not an error here: section 9.5 makes that a grant that grants nothing,
/// which the embedder reports, not a syntax error of the flag's shape.
pub(crate) fn parse_grant(text: &str) -> Result<Grant, GrantSyntaxError> {
    // The name ends at the first ':' that comes before any '=' (a ':' after
    // an '=' belongs to a scope value, per section 9.5), or else at the
    // first '=', or else at the end of the text.
    let first_eq = text.find('=');
    let colon = text.find(':').filter(|&c| first_eq.is_none_or(|e| c < e));
    let name_end = colon.or(first_eq).unwrap_or(text.len());
    let name = &text[..name_end];
    if name.is_empty() {
        return Err(GrantSyntaxError::EmptyName);
    }
    let after_name = &text[name_end..];

    let (kind, scope_text) = if let Some(after_colon) = after_name.strip_prefix(':') {
        match after_colon.find('=') {
            Some(e) => (
                Some(after_colon[..e].to_owned()),
                Some(&after_colon[e + 1..]),
            ),
            None => (Some(after_colon.to_owned()), None),
        }
    } else if let Some(after_eq) = after_name.strip_prefix('=') {
        (None, Some(after_eq))
    } else {
        (None, None)
    };

    let scopes = match scope_text {
        None => None,
        Some(s) => {
            let parts: Vec<String> = s.split(',').map(str::to_owned).collect();
            if parts.len() > 1 && parts.iter().any(String::is_empty) {
                return Err(GrantSyntaxError::EmptyScopeElement);
            }
            Some(parts)
        }
    };

    Ok(Grant {
        name: name.to_owned(),
        kind,
        scopes,
    })
}

#[cfg(test)]
mod tests {
    use super::{Grant, GrantSyntaxError, parse_grant};

    fn grant(name: &str, kind: Option<&str>, scopes: Option<&[&str]>) -> Grant {
        Grant {
            name: name.to_owned(),
            kind: kind.map(str::to_owned),
            scopes: scopes.map(|s| s.iter().map(|x| (*x).to_owned()).collect()),
        }
    }

    #[test]
    fn the_examples_of_the_specification_parse() {
        assert_eq!(parse_grant("print"), Ok(grant("print", None, None)));
        assert_eq!(
            parse_grant("fs:read=./data"),
            Ok(grant("fs", Some("read"), Some(&["./data"])))
        );
        assert_eq!(
            parse_grant("fs:read=./data,./config"),
            Ok(grant("fs", Some("read"), Some(&["./data", "./config"])))
        );
        assert_eq!(
            parse_grant("fs:write=./out"),
            Ok(grant("fs", Some("write"), Some(&["./out"])))
        );
        assert_eq!(
            parse_grant("http=api.example.com"),
            Ok(grant("http", None, Some(&["api.example.com"])))
        );
        assert_eq!(
            parse_grant("env=HOME,PATH"),
            Ok(grant("env", None, Some(&["HOME", "PATH"])))
        );
        assert_eq!(parse_grant("stdin"), Ok(grant("stdin", None, None)));
    }

    #[test]
    fn a_colon_inside_a_scope_value_does_not_start_a_kind() {
        // The kind ':' must come before any '=': "http=host:8080" has no kind.
        assert_eq!(
            parse_grant("http=host:8080"),
            Ok(grant("http", None, Some(&["host:8080"])))
        );
    }

    #[test]
    fn an_empty_scope_value_is_a_grant_that_grants_nothing_not_a_syntax_error() {
        assert_eq!(
            parse_grant("fs:read="),
            Ok(grant("fs", Some("read"), Some(&[""])))
        );
    }

    #[test]
    fn malformed_shapes_are_refused() {
        assert_eq!(parse_grant(""), Err(GrantSyntaxError::EmptyName));
        assert_eq!(parse_grant(":read=./x"), Err(GrantSyntaxError::EmptyName));
        assert!(
            parse_grant("a,,b").is_ok(),
            "not a grant with a scope list at all"
        );
        assert_eq!(
            parse_grant("env=a,,b"),
            Err(GrantSyntaxError::EmptyScopeElement)
        );
        assert_eq!(
            parse_grant("env=,"),
            Err(GrantSyntaxError::EmptyScopeElement)
        );
    }

    #[test]
    fn no_white_space_is_trimmed() {
        assert_eq!(
            parse_grant("fs: read=./data"),
            Ok(grant("fs", Some(" read"), Some(&["./data"])))
        );
        assert_eq!(
            parse_grant("print:read"),
            Ok(grant("print", Some("read"), None))
        );
    }
}

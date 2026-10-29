//! Turning the `--allow` flags of a run into real granted capabilities
//! (section 9.5 and chapter 16).
//!
//! [`Granted::from_flags`] validates the flags against each capability's own
//! rules, because only a host knows what capabilities it defines and which
//! of them take a kind or require a scope. A flag that breaks one of those
//! rules is an error in the invocation, not a language diagnostic, and is
//! reported with the text that was written.
//!
//! **What is granted here.** `print`, `clock` and `random` always, by
//! section 9.5's first rule, and `env`, `args`, `fs`, `stdin` and `http`
//! when asked for.

use crate::capabilities::ScopeRoot;
use crate::grants::Grant;
use crate::http::{HttpLimits, HttpScope, parse_http_scope};

/// What one run grants.
#[derive(Debug)]
pub(crate) struct Granted {
    /// Whether `env` is granted.
    pub env: bool,
    /// The variable names `env` may read, or `None` for every variable.
    pub env_names: Option<Vec<String>>,
    /// Whether `args` is granted.
    pub args: bool,
    /// The arguments the program itself was given.
    pub program_args: Vec<String>,
    /// Whether `stdin` is granted.
    pub stdin: bool,
    /// The directories `fs:read` may reach.
    pub fs_read: Vec<ScopeRoot>,
    /// The directories `fs:write` may reach.
    pub fs_write: Vec<ScopeRoot>,
    /// The hosts `http` may reach.
    pub http: Vec<HttpScope>,
    /// The limits `http` applies to a request (section 16.9, "Limits"):
    /// section 16.9's own stated defaults unless a host changes them, which
    /// a test does to use a short timeout and a small body cap against a
    /// local server rather than the real 30 seconds and 8 MiB.
    pub http_limits: HttpLimits,
}

impl Granted {
    /// The default grants of section 9.5's first rule and nothing else.
    pub(crate) fn none() -> Granted {
        Granted {
            env: false,
            env_names: None,
            args: false,
            program_args: Vec::new(),
            stdin: false,
            fs_read: Vec::new(),
            fs_write: Vec::new(),
            http: Vec::new(),
            http_limits: HttpLimits::default(),
        }
    }

    /// Reads the `--allow` flags of one invocation.
    ///
    /// # Errors
    ///
    /// Returns the message to print for the first flag that breaks a
    /// capability's own rules.
    pub(crate) fn from_flags(grants: &[Grant], program_args: &[String]) -> Result<Granted, String> {
        let mut granted = Granted::none();
        granted.program_args = program_args.to_vec();
        for grant in grants {
            match grant.name.as_str() {
                // Section 9.5: granted by default, so naming one again is
                // harmless, but a kind or a scope on it is not meaningful.
                "print" | "clock" | "random" => {
                    if grant.kind.is_some() {
                        return Err(format!("'{}' has no kinds", grant.name));
                    }
                    if grant.scopes.is_some() {
                        return Err(format!("'{}' takes no scope", grant.name));
                    }
                }
                "env" => {
                    if grant.kind.is_some() {
                        return Err("'env' has no kinds".to_owned());
                    }
                    granted.env = true;
                    if let Some(scopes) = &grant.scopes {
                        if scopes.iter().any(String::is_empty) {
                            return Err("'env' was given an empty variable name".to_owned());
                        }
                        // Grants are additive: several `--allow env=` flags
                        // together allow the union of their names.
                        granted
                            .env_names
                            .get_or_insert_with(Vec::new)
                            .extend(scopes.iter().cloned());
                    }
                }
                "args" => {
                    if grant.kind.is_some() {
                        return Err("'args' has no kinds".to_owned());
                    }
                    if grant.scopes.is_some() {
                        return Err("'args' takes no scope".to_owned());
                    }
                    granted.args = true;
                }
                "fs" => granted.add_fs(grant)?,
                "stdin" => {
                    if grant.kind.is_some() {
                        return Err("'stdin' has no kinds".to_owned());
                    }
                    if grant.scopes.is_some() {
                        return Err("'stdin' takes no scope".to_owned());
                    }
                    granted.stdin = true;
                }
                "http" => granted.add_http(grant)?,
                other => return Err(format!("unknown capability '{other}'")),
            }
        }
        Ok(granted)
    }

    /// Whether `fs` is granted in either kind.
    pub(crate) fn fs(&self) -> bool {
        !self.fs_read.is_empty() || !self.fs_write.is_empty()
    }

    /// Reads one `fs` grant (section 16.8): a required kind, `read` or
    /// `write`, and a required list of paths, each of which must already
    /// exist because a grant names a directory.
    fn add_fs(&mut self, grant: &Grant) -> Result<(), String> {
        let kind = match grant.kind.as_deref() {
            None => return Err("'fs' needs a kind: 'fs:read=PATH' or 'fs:write=PATH'".to_owned()),
            Some(kind @ ("read" | "write")) => kind,
            Some(kind) => return Err(format!("'fs' has no kind '{kind}'; use 'read' or 'write'")),
        };
        let Some(scopes) = &grant.scopes else {
            return Err("'fs' needs a path: 'fs:read=./data'".to_owned());
        };
        if scopes.iter().any(String::is_empty) {
            return Err("'fs' was given an empty path".to_owned());
        }
        for path in scopes {
            let root = ScopeRoot::new(path).map_err(|kind| {
                if kind == crate::capabilities::IoErrorKind::Invalid {
                    format!("'fs' was granted '{path}', which must not contain '..'")
                } else {
                    format!("'fs' was granted '{path}', which does not exist")
                }
            })?;
            // Grants are additive: several flags for the same kind together
            // allow the union of their paths.
            if kind == "read" {
                self.fs_read.push(root);
            } else {
                self.fs_write.push(root);
            }
        }
        Ok(())
    }

    /// Reads one `http` grant (section 16.9): a required list of host
    /// scope values (`http=api.example.com,*.cdn.example.com`), no kind.
    fn add_http(&mut self, grant: &Grant) -> Result<(), String> {
        if grant.kind.is_some() {
            return Err("'http' has no kinds".to_owned());
        }
        let Some(scopes) = &grant.scopes else {
            return Err("'http' needs a host: 'http=api.example.com'".to_owned());
        };
        if scopes.iter().any(String::is_empty) {
            return Err("'http' was given an empty host".to_owned());
        }
        for scope in scopes {
            self.http.push(parse_http_scope(scope)?);
        }
        Ok(())
    }

    /// The value `env(name)` gives: `none` for a name outside the grant,
    /// which section 9.5 rule 3 makes indistinguishable from an unset
    /// variable.
    pub(crate) fn env_value(&self, name: &str) -> Option<String> {
        if !self.env {
            return None;
        }
        if let Some(allowed) = &self.env_names {
            if !allowed.iter().any(|n| n == name) {
                return None;
            }
        }
        crate::capabilities::env_var(name)
    }
}

#[cfg(test)]
mod tests {
    use super::Granted;
    use crate::grants::parse_grant;

    fn flags(texts: &[&str]) -> Vec<crate::grants::Grant> {
        texts
            .iter()
            .map(|t| parse_grant(t).expect("the flag parses"))
            .collect()
    }

    #[test]
    fn nothing_is_granted_without_a_flag() {
        let granted = Granted::from_flags(&[], &[]).expect("no flags is fine");
        assert!(!granted.env);
        assert!(!granted.args);
        assert_eq!(granted.env_value("PATH"), None);
    }

    #[test]
    fn env_with_no_scope_reads_every_variable() {
        // SAFETY-free: this only reads what the test harness already set.
        let granted = Granted::from_flags(&flags(&["env"]), &[]).expect("env is grantable");
        assert!(granted.env);
        assert!(granted.env_names.is_none());
    }

    #[test]
    fn env_with_a_scope_reads_only_those_names() {
        let granted =
            Granted::from_flags(&flags(&["env=HOME,PATH"]), &[]).expect("env is grantable");
        assert_eq!(
            granted.env_names.as_deref(),
            Some(["HOME".to_owned(), "PATH".to_owned()].as_slice())
        );
        assert_eq!(granted.env_value("NOT_IN_THE_GRANT"), None);
    }

    #[test]
    fn grants_are_additive() {
        let granted =
            Granted::from_flags(&flags(&["env=HOME", "env=PATH"]), &[]).expect("both are fine");
        assert_eq!(
            granted.env_names.as_deref(),
            Some(["HOME".to_owned(), "PATH".to_owned()].as_slice())
        );
    }

    #[test]
    fn args_carries_the_words_after_the_program() {
        let words = ["one".to_owned(), "two".to_owned()];
        let granted = Granted::from_flags(&flags(&["args"]), &words).expect("args is grantable");
        assert!(granted.args);
        assert_eq!(granted.program_args, words);
    }

    #[test]
    fn a_grant_that_breaks_a_capabilitys_own_rules_is_reported() {
        for (flag, wanted) in [
            ("print:read", "has no kinds"),
            ("args=x", "takes no scope"),
            ("env:read", "has no kinds"),
            ("stdin:read", "has no kinds"),
            ("stdin=x", "takes no scope"),
            ("fs", "needs a kind"),
            ("fs:exec=./x", "has no kind 'exec'"),
            ("fs:read", "needs a path"),
            ("fs:read=", "empty path"),
            ("fs:read=/no/such/directory/anywhere", "does not exist"),
            ("http:read=example.com", "has no kinds"),
            ("http", "needs a host"),
            ("http=", "empty host"),
            ("frobnicate", "unknown capability"),
        ] {
            let error = Granted::from_flags(&flags(&[flag]), &[])
                .expect_err(&format!("{flag} should be refused"));
            assert!(
                error.contains(wanted),
                "{flag}: expected a message about {wanted}, got {error}"
            );
        }
    }

    #[test]
    fn fs_and_stdin_are_granted_for_real() {
        let granted = Granted::from_flags(&flags(&["fs:read=.", "fs:write=.", "stdin"]), &[])
            .expect("all three are valid");
        assert!(granted.fs());
        assert_eq!(granted.fs_read.len(), 1);
        assert_eq!(granted.fs_write.len(), 1);
        assert!(granted.stdin);
    }

    #[test]
    fn http_is_granted_for_real() {
        let granted = Granted::from_flags(&flags(&["http=api.example.com,*.cdn.example.com"]), &[])
            .expect("http is grantable");
        assert_eq!(granted.http.len(), 2);
    }

    #[test]
    fn http_grants_are_additive() {
        let granted =
            Granted::from_flags(&flags(&["http=a.example.com", "http=b.example.com"]), &[])
                .expect("both are fine");
        assert_eq!(granted.http.len(), 2);
    }
}

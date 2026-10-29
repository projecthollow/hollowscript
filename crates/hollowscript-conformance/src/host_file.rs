//! `<name>.host.json` (`corpus/FORMAT.md`, section 19.4): the one override
//! this runner reads from it today, `limits`, so a case that pins an exact
//! resource-limit boundary (section 11.3's own worked example, for
//! instance) can run against a small `maxSteps` rather than the corpus
//! host's default. The other members section 19.4 lists (`stdin`, `args`,
//! `env`, `files`, `http`, `filterSchema`, `yieldCancelAt`, `entryPath`) are
//! not read here; a case that needs one is out of reach of this runner
//! until they are, the same way a case needing a grant beyond the fixed
//! host already is.
//!
//! Absent unknown members rather than rejecting them, so a `host.json`
//! written for a member this runner does not yet read still parses.

use crate::json::{self, Json};
use hollowscript::Limits;
use std::path::Path;

/// The `limits` overrides of `<name>.host.json`, applied over
/// [`Limits::default`]. Returns the default limits, silently, when the file
/// does not exist or does not parse: a missing host file is the documented
/// default (section 19.4, "Unless `<name>.host.json` says otherwise"), and a
/// malformed one is caught by the case's own diagnostics instead of this
/// tool's.
#[must_use]
pub(crate) fn limits_for(program: &Path) -> Limits {
    let host_path = program.with_extension("host.json");
    let Ok(text) = std::fs::read_to_string(&host_path) else {
        return Limits::default();
    };
    let Ok(value) = json::parse(&text) else {
        return Limits::default();
    };
    let mut limits = Limits::default();
    let Some(overrides) = value.get("limits") else {
        return limits;
    };
    if let Some(v) = overrides.get("maxSteps").and_then(Json::as_i64) {
        limits.max_steps = u64::try_from(v).unwrap_or(limits.max_steps);
    }
    if let Some(v) = overrides.get("maxMemoryBytes").and_then(Json::as_i64) {
        limits.max_memory_bytes = u64::try_from(v).unwrap_or(limits.max_memory_bytes);
    }
    if let Some(v) = overrides.get("maxOutputBytes").and_then(Json::as_i64) {
        limits.max_output_bytes = u64::try_from(v).unwrap_or(limits.max_output_bytes);
    }
    if let Some(v) = overrides.get("maxCallDepth").and_then(Json::as_i64) {
        limits.max_call_depth = u64::try_from(v).unwrap_or(limits.max_call_depth);
    }
    limits
}

#[cfg(test)]
mod tests {
    use super::limits_for;
    use hollowscript::Limits;
    use std::io::Write;

    #[test]
    fn a_missing_host_file_gives_the_default_limits() {
        let dir = std::env::temp_dir().join("hollowscript-conformance-host-file-test-missing");
        let _ = std::fs::create_dir_all(&dir);
        let program = dir.join("case.hws");
        assert_eq!(limits_for(&program), Limits::default());
    }

    #[test]
    fn a_host_file_overrides_only_the_limits_it_names() {
        let dir = std::env::temp_dir().join("hollowscript-conformance-host-file-test-limits");
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let program = dir.join("case.hws");
        let host = dir.join("case.host.json");
        let mut f = std::fs::File::create(&host).expect("create host file");
        write!(f, r#"{{"limits": {{"maxSteps": 9}}}}"#).expect("write host file");
        drop(f);
        let limits = limits_for(&program);
        assert_eq!(limits.max_steps, 9);
        assert_eq!(limits.max_memory_bytes, Limits::default().max_memory_bytes);
        let _ = std::fs::remove_file(&host);
    }
}

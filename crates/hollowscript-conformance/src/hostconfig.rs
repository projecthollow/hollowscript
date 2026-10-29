//! What one case adds to the fixed corpus host (`corpus/FORMAT.md`, "The
//! host"): the grants its expected file lists (section 9.5's textual
//! grammar) and, per [section 19.4](../../../docs/spec/19-conformance.md#194-the-corpus-host),
//! the files and canned network exchanges a `<name>.host.json` gives it.
//!
//! This crate cannot depend on `hollowscript-cli` (`xtask graph`), and a
//! conformance host is meant to be an independent implementation of the
//! same specification in any case, so the grant grammar, the `fs` scope
//! check and the `http` grant match are each their own small
//! implementation here rather than shared code: two independent readings
//! of section 9.5 and chapter 16 are a better test of the specification's
//! own clarity than one shared implementation would be.
//!
//! **What this does not implement**, honestly rather than silently:
//! `host.json`'s `env`, `args`, `stdin`, `yieldCancelAt` and `entryPath`
//! members are not read here; only `files`, `http` and `filterSchema` are,
//! because those are what the corpus needed so far (the six `grants/*`
//! cases, the specification's own `fs`/`http` examples, and the `filters/`
//! cases that validate a literal against a schema, Appendix A section A.7).
//! `limits` is read, separately, by `crate::host_file`. `http.request`'s redirect
//! handling (section 16.9, "Redirects") is not implemented either: a
//! canned exchange is matched once, by method and URL, and a redirect
//! status is simply returned as `Ok`, like any other status, rather than
//! followed.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

/// One granted `fs` or `http` scope, and the case's canned file system and
/// network, built from the case's `grants` (its `.expected.json`) and its
/// `<name>.host.json` if it has one.
#[derive(Clone, Debug, Default)]
pub(crate) struct HostConfig {
    pub fs_read: Vec<Vec<String>>,
    pub fs_write: Vec<Vec<String>>,
    pub http: Vec<HttpScope>,
    /// Path segments to file content. A path is a directory exactly when
    /// some file's segments have it as a proper prefix (`corpus/FORMAT.md`:
    /// "directories are implied by their files").
    pub files: Rc<RefCell<BTreeMap<Vec<String>, String>>>,
    pub http_exchanges: Vec<Exchange>,
    /// The host's filter schema (section 19.4's `filterSchema`, section
    /// A.6), if `<name>.host.json` gave one.
    pub filter_schema: Option<hollowscript_core::filter::FilterSchema>,
}

/// One `http` grant scope value (section 16.9's grammar), simplified for a
/// host that never makes a real connection: no IP literals or address
/// safety, since a canned exchange is matched by URL text, not a socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HttpScope {
    Host { name: String, port: Option<u16> },
    Wildcard { suffix: String, port: Option<u16> },
}

/// One canned network exchange from `<name>.host.json`'s `http` list
/// (section 19.4).
#[derive(Clone, Debug)]
pub(crate) struct Exchange {
    pub method: String,
    pub url: String,
    pub status: i64,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl HostConfig {
    /// Applies every grant of `grants` (section 9.5's textual grammar,
    /// `name[:kind][=scope{,scope}]`) to this configuration. Ignores a
    /// grant this host does not implement or one whose shape it does not
    /// recognise: the corpus's own `grants/*` cases pin that a *host* must
    /// refuse such a thing, not that every reader of the grammar must.
    pub(crate) fn apply_grants(&mut self, grants: &[String]) {
        for text in grants {
            self.apply_grant(text);
        }
    }

    fn apply_grant(&mut self, text: &str) {
        let first_eq = text.find('=');
        let colon = text.find(':').filter(|&c| first_eq.is_none_or(|e| c < e));
        let name_end = colon.or(first_eq).unwrap_or(text.len());
        let name = &text[..name_end];
        let after_name = &text[name_end..];
        let kind_end_in_after_name = first_eq.map_or(after_name.len(), |eq| eq - name_end);
        let kind = colon.map(|_| &after_name[1..kind_end_in_after_name]);
        let scopes: Vec<&str> = match first_eq {
            Some(eq) => text[eq + 1..].split(',').collect(),
            None => Vec::new(),
        };
        match name {
            "fs" => {
                for scope in &scopes {
                    let root = normalize(scope);
                    match kind {
                        Some("read") => self.fs_read.push(root),
                        Some("write") => self.fs_write.push(root),
                        _ => {}
                    }
                }
            }
            "http" => {
                for scope in &scopes {
                    if let Some(parsed) = parse_http_scope(scope) {
                        self.http.push(parsed);
                    }
                }
            }
            _ => {}
        }
    }

    /// Reads `<name>.host.json`'s `files` and `http` members
    /// (section 19.4) into this configuration. Absent or unreadable is the
    /// same as an empty file: most cases have none at all.
    pub(crate) fn apply_host_json(&mut self, path: &std::path::Path) {
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        let Ok(document) = crate::json::parse(&text) else {
            return;
        };
        if let Some(crate::json::Json::Object(files)) = document.get("files") {
            let mut map = self.files.borrow_mut();
            for (path, value) in files {
                if let Some(content) = value.as_str() {
                    map.insert(normalize(path), content.to_owned());
                }
            }
        }
        if let Some(exchanges) = document.get("http").and_then(crate::json::Json::as_array) {
            for exchange in exchanges {
                let (Some(method), Some(url)) = (
                    exchange.get("method").and_then(crate::json::Json::as_str),
                    exchange.get("url").and_then(crate::json::Json::as_str),
                ) else {
                    continue;
                };
                let status = exchange
                    .get("status")
                    .and_then(crate::json::Json::as_i64)
                    .unwrap_or(200);
                let mut headers = Vec::new();
                if let Some(crate::json::Json::Object(map)) = exchange.get("headers") {
                    for (name, value) in map {
                        if let Some(text) = value.as_str() {
                            headers.push((name.clone(), text.to_owned()));
                        }
                    }
                }
                let body = exchange
                    .get("body")
                    .and_then(crate::json::Json::as_str)
                    .unwrap_or("")
                    .to_owned();
                self.http_exchanges.push(Exchange {
                    method: method.to_owned(),
                    url: url.to_owned(),
                    status,
                    headers,
                    body,
                });
            }
        }
        if let Some(schema_json) = document.get("filterSchema") {
            self.filter_schema = filter_schema_of(schema_json);
        }
    }
}

/// Reads a `filterSchema` value (section 19.4, section A.6) as
/// `{"fields": {"<name>": {"type": "enum"|"text"|"textMulti"|"date"|
/// "number"|"boolean", "aliases": {"<input>": "<canonical>"}, "values":
/// ["<canonical>", ...]}}}`. A field whose shape does not match this is
/// left out rather than failing the whole schema: the same "absent or
/// unreadable is the same as empty" spirit `apply_host_json` already has
/// for `files` and `http`.
fn filter_schema_of(json: &crate::json::Json) -> Option<hollowscript_core::filter::FilterSchema> {
    use hollowscript_core::filter::{FieldDesc, FieldType, FilterSchema};
    let crate::json::Json::Object(fields) = json.get("fields")? else {
        return None;
    };
    let mut schema = FilterSchema::default();
    for (name, desc) in fields {
        let ty = match desc.get("type").and_then(crate::json::Json::as_str) {
            Some("enum") => FieldType::Enum,
            Some("text") => FieldType::Text,
            Some("textMulti") => FieldType::TextMulti,
            Some("date") => FieldType::Date,
            Some("number") => FieldType::Number,
            Some("boolean") => FieldType::Boolean,
            _ => continue,
        };
        let mut aliases = BTreeMap::new();
        if let Some(crate::json::Json::Object(map)) = desc.get("aliases") {
            for (input, canonical) in map {
                if let Some(canonical) = canonical.as_str() {
                    aliases.insert(input.to_lowercase(), canonical.to_owned());
                }
            }
        }
        let values = desc
            .get("values")
            .and_then(crate::json::Json::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(crate::json::Json::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            });
        schema.fields.insert(
            name.to_lowercase(),
            FieldDesc {
                ty,
                aliases,
                values,
            },
        );
    }
    Some(schema)
}

/// Splits `path` into segments, collapsing `.` and resolving `..` by
/// popping, the same lexical model `hollowscript-cli::capabilities` uses to
/// decide scope membership without touching a filesystem: there is no real
/// one here to touch.
pub(crate) fn normalize(path: &str) -> Vec<String> {
    let mut segments = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other.to_owned()),
        }
    }
    segments
}

/// Whether `path` is malformed (section 16.8): empty, containing U+0000 or
/// a backslash, beginning with a drive designator, or a non-`.`/`..`
/// segment ending in `.` or a space.
pub(crate) fn is_malformed_path(path: &str) -> bool {
    if path.is_empty() || path.contains('\0') || path.contains('\\') {
        return true;
    }
    let mut bytes = path.bytes();
    if let (Some(first), Some(second)) = (bytes.next(), bytes.next()) {
        if first.is_ascii_alphabetic() && second == b':' {
            return true;
        }
    }
    path.split('/').any(|segment| {
        if segment == "." || segment == ".." || segment.is_empty() {
            return false;
        }
        segment.ends_with('.') || segment.ends_with(' ')
    })
}

/// Whether `segments` is contained in (equal to, or inside) any of `roots`,
/// a prefix at a segment boundary (section 16.8, "Scope check").
pub(crate) fn contained_in(segments: &[String], roots: &[Vec<String>]) -> bool {
    roots.iter().any(|root| segments.starts_with(root))
}

/// Whether some stored file's path has `segments` as a proper prefix, which
/// is what makes `segments` a directory (`corpus/FORMAT.md`, "directories
/// are implied by their files").
pub(crate) fn is_directory(files: &BTreeMap<Vec<String>, String>, segments: &[String]) -> bool {
    files
        .keys()
        .any(|key| key.len() > segments.len() && key.starts_with(segments))
}

/// Parses one `http` grant scope value (section 16.9's grammar), the IP
/// literal and address-safety parts aside (see the module doc).
fn parse_http_scope(text: &str) -> Option<HttpScope> {
    if text.is_empty() {
        return None;
    }
    let (host, port) = match text.rsplit_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().ok()),
        None => (text, None),
    };
    if host.is_empty() {
        return None;
    }
    let host = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    if let Some(suffix) = host.strip_prefix("*.") {
        if suffix.is_empty() {
            return None;
        }
        return Some(HttpScope::Wildcard {
            suffix: suffix.to_owned(),
            port,
        });
    }
    Some(HttpScope::Host { name: host, port })
}

/// Whether `scopes` grants a request to `host` on `port` (`https` or not,
/// for the default-port rule).
pub(crate) fn http_scope_allows(scopes: &[HttpScope], host: &str, https: bool, port: u16) -> bool {
    let default_port = if https { 443 } else { 80 };
    scopes.iter().any(|scope| match scope {
        HttpScope::Host { name, port: p } => name == host && p.unwrap_or(default_port) == port,
        HttpScope::Wildcard { suffix, port: p } => {
            host.strip_suffix(suffix.as_str())
                .is_some_and(|prefix| prefix.ends_with('.'))
                && p.unwrap_or(default_port) == port
        }
    })
}

/// A minimal absolute `http`/`https` URL: scheme, host (lower-cased) and
/// port. `None` for anything else, including a malformed URL: the caller
/// turns that into `Err(Invalid)` the same way the real capability does.
pub(crate) fn parse_url(text: &str) -> Option<(bool, String, u16)> {
    let (scheme, rest) = text.split_once("://")?;
    let https = match scheme.to_ascii_lowercase().as_str() {
        "http" => false,
        "https" => true,
        _ => return None,
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().ok()?),
        None => (authority, if https { 443 } else { 80 }),
    };
    if host.is_empty() {
        return None;
    }
    let host = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    Some((https, host, port))
}

#[cfg(test)]
mod tests {
    use super::{HostConfig, contained_in, is_directory, normalize, parse_url};

    #[test]
    fn a_read_grant_and_a_write_grant_are_independent() {
        let mut config = HostConfig::default();
        config.apply_grants(&["fs:read=./data".to_owned()]);
        assert_eq!(config.fs_read, [vec!["data".to_owned()]]);
        assert!(config.fs_write.is_empty());
    }

    #[test]
    fn a_parent_segment_normalises_out_of_the_grant() {
        let outside = normalize("./data/../secret.txt");
        assert_eq!(outside, ["secret.txt"]);
        let root = normalize("./data");
        assert!(!contained_in(&outside, &[root]));
    }

    #[test]
    fn an_absolute_path_outside_the_root_is_not_contained() {
        let outside = normalize("/outside/secret.txt");
        let root = normalize("data");
        assert!(!contained_in(&outside, &[root]));
    }

    #[test]
    fn a_path_inside_the_root_is_contained() {
        let inside = normalize("./data/out.txt");
        let root = normalize("data");
        assert!(contained_in(&inside, &[root]));
    }

    #[test]
    fn a_stored_files_ancestor_is_a_directory() {
        let mut files = std::collections::BTreeMap::new();
        files.insert(normalize("notes/list.txt"), "buy milk\n".to_owned());
        assert!(is_directory(&files, &normalize("notes")));
        assert!(!is_directory(&files, &normalize("notes/list.txt")));
        assert!(!is_directory(&files, &normalize("other")));
    }

    #[test]
    fn http_grants_are_parsed_and_matched_as_section_16_9_describes() {
        let mut config = HostConfig::default();
        config.apply_grants(&["http=api.example.com".to_owned()]);
        assert!(super::http_scope_allows(
            &config.http,
            "api.example.com",
            true,
            443
        ));
        assert!(!super::http_scope_allows(
            &config.http,
            "other.example.com",
            true,
            443
        ));
        assert!(!super::http_scope_allows(
            &config.http,
            "api.example.com.attacker.example",
            true,
            443
        ));
    }

    #[test]
    fn urls_parse_their_scheme_host_and_port() {
        assert_eq!(
            parse_url("https://api.example.com/status"),
            Some((true, "api.example.com".to_owned(), 443))
        );
        assert_eq!(parse_url("ftp://example.com/"), None);
        assert_eq!(parse_url("https://user@example.com/"), None);
    }
}

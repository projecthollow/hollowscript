//! Rule definitions for the hygiene scan.
//!
//! The scan runs over the tree that is mirrored publicly. Every forbidden
//! string is assembled from fragments at run time, so that this file does not
//! contain the strings it looks for and therefore passes its own scan.

/// A byte range of one line that a rule matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Hit {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

/// One rule of the scan.
pub(crate) struct Rule {
    /// A short stable name, printed with each finding.
    pub(crate) id: &'static str,
    /// What a finding means, in words.
    pub(crate) what: &'static str,
    matcher: Matcher,
}

enum Matcher {
    /// Any of these characters.
    Chars(Vec<char>),
    /// Any of these strings, as a substring, ignoring ASCII case.
    Substrings(Vec<String>),
    /// Any of these strings as a whole word, ignoring ASCII case.
    Words(Vec<String>),
    /// Any of these strings at the start of a word, ignoring ASCII case.
    Prefixes(Vec<String>),
    /// A hand written matcher.
    Custom(fn(&str) -> Vec<Hit>),
}

impl Rule {
    /// The ranges of `line` that this rule matches.
    pub(crate) fn find(&self, line: &str) -> Vec<Hit> {
        match &self.matcher {
            Matcher::Chars(chars) => line
                .char_indices()
                .filter(|(_, c)| chars.contains(c))
                .map(|(i, c)| Hit {
                    start: i,
                    end: i + c.len_utf8(),
                })
                .collect(),
            Matcher::Substrings(needles) => {
                let lower = line.to_ascii_lowercase();
                needles.iter().flat_map(|n| find_all(&lower, n)).collect()
            }
            Matcher::Words(needles) => {
                let lower = line.to_ascii_lowercase();
                needles
                    .iter()
                    .flat_map(|n| find_all(&lower, n))
                    .filter(|h| is_word(line, *h))
                    .collect()
            }
            Matcher::Prefixes(needles) => {
                let lower = line.to_ascii_lowercase();
                needles
                    .iter()
                    .flat_map(|n| find_all(&lower, n))
                    .filter(|h| !byte_before(line, h.start).is_some_and(is_word_byte))
                    .collect()
            }
            Matcher::Custom(f) => f(line),
        }
    }
}

fn join(parts: &[&str]) -> String {
    parts.concat()
}

fn strings(list: &[&[&str]]) -> Vec<String> {
    list.iter().map(|parts| join(parts)).collect()
}

/// The rules of the scan, in the order in which findings are reported.
pub(crate) fn rules() -> Vec<Rule> {
    vec![
        Rule {
            id: "dash",
            what: "an em or en dash",
            matcher: Matcher::Chars(vec!['\u{2014}', '\u{2013}']),
        },
        Rule {
            id: "internal-name",
            what: "an internal organisation or project name",
            matcher: Matcher::Custom(find_internal_names),
        },
        Rule {
            id: "internal-prefix",
            what: "an internal repository prefix",
            matcher: Matcher::Prefixes(strings(&[&["sst", "k-"]])),
        },
        Rule {
            id: "old-implementation",
            what: "a reference to another implementation of the language",
            matcher: Matcher::Custom(find_old_implementation),
        },
        Rule {
            id: "old-file",
            what: "a reference to a file of another repository",
            matcher: Matcher::Words(strings(&[&["spec", ".md"]])),
        },
        Rule {
            id: "authorship",
            what: "a reference to automated authorship",
            matcher: Matcher::Custom(find_authorship),
        },
        Rule {
            id: "account-number",
            what: "a twelve digit number that could be an account number",
            matcher: Matcher::Custom(find_twelve_digits),
        },
        Rule {
            id: "task-identifier",
            what: "an internal task or decision identifier",
            matcher: Matcher::Custom(find_task_identifiers),
        },
        Rule {
            id: "local-path",
            what: "an absolute local path",
            matcher: Matcher::Custom(find_local_paths),
        },
        Rule {
            id: "registry",
            what: "a private registry name or credential setting",
            matcher: Matcher::Substrings(strings(&[
                &["code", "artifact"],
                &["_auth", "token"],
                &["_pass", "word="],
            ])),
        },
        Rule {
            id: "secret",
            what: "something that looks like a secret",
            matcher: Matcher::Custom(find_secrets),
        },
        Rule {
            id: "review-identifier",
            what: "an internal security review finding identifier",
            matcher: Matcher::Custom(find_review_identifiers),
        },
    ]
}

/// The internal organisation names, ignoring ASCII case, except where one is
/// part of the public mirror's own URL (see [`is_public_mirror_url`]).
fn find_internal_names(line: &str) -> Vec<Hit> {
    let lower = line.to_ascii_lowercase();
    let mut hits = find_all(&lower, &join(&["soulstack", "-dev"]));
    hits.extend(
        find_all(&lower, &join(&["project", "hollow"]))
            .into_iter()
            .filter(|h| !is_public_mirror_url(line, *h)),
    );
    hits
}

/// Whether `hit` is the organisation segment of exactly the public mirror's
/// URL, `https://github.com/<org>/hollowscript`, or its `/issues` page,
/// written in lower case and not continued by any further path, subdomain,
/// file name or suffix such as `.git`. A trailing sentence full stop or a
/// closing bracket is not a continuation.
fn is_public_mirror_url(line: &str, hit: Hit) -> bool {
    let org = join(&["project", "hollow"]);
    let prefix = "https://github.com/";
    let Some(start) = hit.start.checked_sub(prefix.len()) else {
        return false;
    };
    if start > 0
        && byte_before(line, start)
            .is_some_and(|b| !b.is_ascii_whitespace() && !b"(<\"'`[".contains(&b))
    {
        return false;
    }
    let Some(rest) = line.get(start..) else {
        return false;
    };
    let base = format!("{prefix}{org}/hollowscript");
    ["/issues", ""].iter().any(|suffix| {
        let url = format!("{base}{suffix}");
        rest.strip_prefix(url.as_str()).is_some_and(|after| {
            let mut bytes = after.bytes();
            match bytes.next() {
                None => true,
                Some(b'.') => !bytes.next().is_some_and(|b| is_word_byte(b) || b == b'/'),
                Some(b) => !(is_word_byte(b) || b"-/.#?%&=+~:@".contains(&b)),
            }
        })
    })
}

fn find_old_implementation(line: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    let lower = line.to_ascii_lowercase();
    hits.extend(find_all(&lower, &join(&["type", "script implementation"])));
    hits.extend(
        find_all(&lower, &join(&["ora", "cle"]))
            .into_iter()
            .filter(|h| is_word(line, *h)),
    );
    hits
}

fn find_authorship(line: &str) -> Vec<Hit> {
    let lower = line.to_ascii_lowercase();
    let mut hits = Vec::new();
    let words = strings(&[
        &["cla", "ude"],
        &["anthro", "pic"],
        &["chatg", "pt"],
        &["open", "ai"],
        &["co", "pilot"],
        &["l", "lm"],
        &["l", "lms"],
        &["g", "pt"],
        &["assis", "tant"],
        &["assis", "tants"],
        &["prom", "pt"],
        &["prom", "pts"],
        &["co-authored", "-by"],
    ]);
    for word in &words {
        hits.extend(
            find_all(&lower, word)
                .into_iter()
                .filter(|h| is_word(line, *h)),
        );
    }
    let singular = join(&["age", "nt"]);
    let plural = join(&["age", "nts"]);
    for word in [&singular, &plural] {
        for hit in find_all(&lower, word)
            .into_iter()
            .filter(|h| is_word(line, *h))
        {
            let before = lower
                .get(hit.start.saturating_sub(5)..hit.start)
                .unwrap_or("");
            // "user-agent"/"user agent" (the HTTP header) and a
            // path-qualified identifier such as `ureq::Agent` (a real
            // third-party type name, not prose) are not what this rule
            // means to catch.
            if before != "user-" && before != "user " && !before.ends_with("::") {
                hits.push(hit);
            }
        }
    }
    let phrases = strings(&[
        &["generated", " by"],
        &["generated", " with"],
        &["language", " model"],
        &["machine", " learning"],
        &["age", "nts.md"],
    ]);
    for phrase in &phrases {
        hits.extend(find_all(&lower, phrase));
    }
    hits.extend(
        find_all(line, &join(&["A", "I"]))
            .into_iter()
            .filter(|h| is_word(line, *h)),
    );
    hits
}

fn find_twelve_digits(line: &str) -> Vec<Hit> {
    let bytes = line.as_bytes();
    let mut hits = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes.get(i).is_some_and(u8::is_ascii_digit) {
            let start = i;
            while bytes.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
            let joined_to_word = start > 0
                && bytes.get(start - 1).is_some_and(|b| is_word_byte(*b))
                || bytes.get(i).is_some_and(|b| is_word_byte(*b));
            if i - start == 12 && !joined_to_word {
                hits.push(Hit { start, end: i });
            }
        } else {
            i += 1;
        }
    }
    hits
}

fn find_task_identifiers(line: &str) -> Vec<Hit> {
    let bytes = line.as_bytes();
    let mut hits = Vec::new();
    for start in 0..bytes.len() {
        if start > 0 && bytes.get(start - 1).is_some_and(|b| is_word_byte(*b)) {
            continue;
        }
        let end = match bytes.get(start) {
            Some(b'P') => match_p_identifier(bytes, start),
            Some(b'D' | b'L') => match_dotted_identifier(bytes, start),
            Some(b'A') => match_decision_identifier(bytes, start),
            _ => None,
        };
        if let Some(end) = end.filter(|e| !bytes.get(*e).is_some_and(|b| is_word_byte(*b))) {
            hits.push(Hit { start, end });
        }
    }
    hits
}

/// `P`, one digit, `.`, two digits.
fn match_p_identifier(bytes: &[u8], start: usize) -> Option<usize> {
    let d = |i: usize| bytes.get(start + i).is_some_and(u8::is_ascii_digit);
    (d(1) && bytes.get(start + 2) == Some(&b'.') && d(3) && d(4)).then_some(start + 5)
}

/// `D` or `L`, digits, then one or more dot groups of digits.
fn match_dotted_identifier(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 1;
    let digits = |from: usize| {
        bytes
            .get(from..)
            .map_or(0, |s| s.iter().take_while(|b| b.is_ascii_digit()).count())
    };
    let first = digits(i);
    if first == 0 {
        return None;
    }
    i += first;
    let mut groups = 0;
    while bytes.get(i) == Some(&b'.') && digits(i + 1) > 0 {
        i += 1 + digits(i + 1);
        groups += 1;
    }
    (groups > 0).then_some(i)
}

/// The three letters of a decision record, an optional hyphen and digits.
fn match_decision_identifier(bytes: &[u8], start: usize) -> Option<usize> {
    let tag = join(&["AD", "R"]);
    if bytes.get(start..start + 3) != Some(tag.as_bytes()) {
        return None;
    }
    let mut i = start + 3;
    if bytes.get(i) == Some(&b'-') {
        i += 1;
    }
    let count = bytes
        .get(i..)
        .map_or(0, |s| s.iter().take_while(|b| b.is_ascii_digit()).count());
    (count > 0).then_some(i + count)
}

/// The review's own finding identifiers (`HSR-` followed by digits), built
/// from fragments so that this rule's own source does not contain the
/// pattern it looks for and therefore does not flag itself. Case sensitive,
/// exactly as the review spells them, and never written anywhere else in a
/// tracked file, including a commit message: this catches a stray one left
/// in a comment, a doc or a test by mistake.
fn find_review_identifiers(line: &str) -> Vec<Hit> {
    let bytes = line.as_bytes();
    let tag = join(&["HS", "R-"]);
    let tag_bytes = tag.as_bytes();
    let mut hits = Vec::new();
    for start in 0..bytes.len() {
        if start > 0 && bytes.get(start - 1).is_some_and(|b| is_word_byte(*b)) {
            continue;
        }
        if bytes.get(start..start + tag_bytes.len()) != Some(tag_bytes) {
            continue;
        }
        let digits_start = start + tag_bytes.len();
        let digits = bytes
            .get(digits_start..)
            .map_or(0, |s| s.iter().take_while(|b| b.is_ascii_digit()).count());
        if digits == 0 {
            continue;
        }
        let end = digits_start + digits;
        if bytes.get(end).is_some_and(|b| is_word_byte(*b)) {
            continue;
        }
        hits.push(Hit { start, end });
    }
    hits
}

fn find_local_paths(line: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    let needles = strings(&[
        &["/Us", "ers/"],
        &["/priv", "ate/"],
        &["/ho", "me/"],
        &["/Vol", "umes/"],
        &["/var/", "folders/"],
    ]);
    for needle in &needles {
        hits.extend(find_all(line, needle));
    }
    let bytes = line.as_bytes();
    let lower = line.to_ascii_lowercase();
    for start in 0..bytes.len() {
        let drive = bytes.get(start).is_some_and(u8::is_ascii_alphabetic)
            && bytes.get(start + 1) == Some(&b':')
            && bytes.get(start + 2) == Some(&b'\\')
            && !(start > 0 && bytes.get(start - 1).is_some_and(|b| is_word_byte(*b)));
        if !drive {
            continue;
        }
        let rest = lower
            .get(start + 3..)
            .unwrap_or("")
            .trim_start_matches('\\');
        if ["users", "windows", "program"]
            .iter()
            .any(|p| rest.starts_with(p))
        {
            hits.push(Hit {
                start,
                end: start + 3,
            });
        }
    }
    hits
}

fn find_secrets(line: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    let alnum = |b: u8| b.is_ascii_alphanumeric();
    let upper_digit = |b: u8| b.is_ascii_uppercase() || b.is_ascii_digit();
    let token = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
    for prefix in ["AKIA", "ASIA", "AGPA", "AIDA", "AROA", "ANPA"] {
        hits.extend(prefixed(line, prefix, 16, upper_digit));
    }
    for prefix in ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "npm_"] {
        hits.extend(prefixed(line, prefix, 36, alnum));
    }
    hits.extend(prefixed(line, "github_pat_", 22, |b| alnum(b) || b == b'_'));
    for prefix in ["xoxb-", "xoxa-", "xoxp-", "xoxr-", "xoxs-"] {
        hits.extend(prefixed(line, prefix, 10, token));
    }
    hits.extend(prefixed(line, "AIza", 35, token));
    hits.extend(prefixed(line, "sk-", 32, alnum));
    let lower = line.to_ascii_lowercase();
    let begin = join(&["-----be", "gin "]);
    let end = join(&["private", " key-----"]);
    if let Some((a, b)) = lower
        .find(&begin)
        .zip(lower.find(&end))
        .filter(|(a, b)| a < b)
    {
        hits.push(Hit {
            start: a,
            end: b + end.len(),
        });
    }
    hits
}

/// Occurrences of `prefix` at the start of a word, followed by at least
/// `min_len` bytes that satisfy `allowed`.
fn prefixed(line: &str, prefix: &str, min_len: usize, allowed: impl Fn(u8) -> bool) -> Vec<Hit> {
    find_all(line, prefix)
        .into_iter()
        .filter(|h| !byte_before(line, h.start).is_some_and(is_word_byte))
        .filter(|h| {
            let run = line
                .as_bytes()
                .get(h.end..)
                .map_or(0, |s| s.iter().take_while(|b| allowed(**b)).count());
            run >= min_len
        })
        .collect()
}

fn find_all(haystack: &str, needle: &str) -> Vec<Hit> {
    if needle.is_empty() {
        return Vec::new();
    }
    haystack
        .match_indices(needle)
        .map(|(i, m)| Hit {
            start: i,
            end: i + m.len(),
        })
        .collect()
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn byte_before(line: &str, index: usize) -> Option<u8> {
    index
        .checked_sub(1)
        .and_then(|i| line.as_bytes().get(i))
        .copied()
}

fn is_word(line: &str, hit: Hit) -> bool {
    !byte_before(line, hit.start).is_some_and(is_word_byte)
        && !line
            .as_bytes()
            .get(hit.end)
            .is_some_and(|b| is_word_byte(*b))
}

#[cfg(test)]
mod tests {
    use super::{Rule, join, rules};

    fn rule(id: &str) -> Rule {
        rules()
            .into_iter()
            .find(|r| r.id == id)
            .expect("rule exists")
    }

    fn count(id: &str, line: &str) -> usize {
        rule(id).find(line).len()
    }

    #[test]
    fn dashes() {
        assert_eq!(count("dash", "a \u{2014} b \u{2013} c"), 2);
        assert_eq!(count("dash", "a - b -- c"), 0);
    }

    #[test]
    fn internal_names_ignore_case() {
        let name = join(&["Soulstack", "-Dev"]);
        assert_eq!(count("internal-name", &format!("see {name}/x")), 1);
        assert_eq!(count("internal-name", "Copyright (c) 2026 Soulstack"), 0);
        let prefix = join(&["sst", "k-tools"]);
        assert_eq!(count("internal-prefix", &format!("use {prefix}")), 1);
        assert_eq!(count("internal-prefix", "task-list and desk-top"), 0);
    }

    #[test]
    fn only_the_public_mirror_url_is_exempt_from_internal_names() {
        let org = join(&["project", "hollow"]);
        let url = format!("https://github.com/{org}/hollowscript");
        for allowed in [
            format!("\"url\": \"{url}\","),
            format!("report it at {url}/issues."),
            format!("see [the repository]({url})"),
            format!("<{url}/issues>"),
            url.clone(),
        ] {
            assert_eq!(count("internal-name", &allowed), 0, "{allowed}");
        }
        for flagged in [
            org.clone(),
            format!("the {org} team"),
            format!("{url}.git"),
            format!("{url}-private"),
            format!("{url}/pulls"),
            format!("{url}/issues/12"),
            format!("{url}/tree/main"),
            format!("https://github.com/{org}/other"),
            format!("https://github.com/{org}"),
            format!("http://github.com/{org}/hollowscript"),
            format!("https://evil.example/github.com/{org}/hollowscript"),
            format!(
                "https://github.com/{}/hollowscript",
                org.to_ascii_uppercase()
            ),
            format!("{org}.com"),
            format!("\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}{org}"),
        ] {
            assert_eq!(count("internal-name", &flagged), 1, "{flagged}");
        }
        assert_eq!(count("internal-name", &format!("{url} and {org}")), 1);
    }

    #[test]
    fn twelve_digit_numbers_need_exact_length() {
        let twelve = join(&["12345", "6789012"]);
        assert_eq!(count("account-number", &format!("id {twelve}.")), 1);
        assert_eq!(count("account-number", "1767225600000"), 0);
        assert_eq!(count("account-number", &format!("x{twelve}")), 0);
        assert_eq!(count("account-number", "12345678901"), 0);
    }

    #[test]
    fn task_identifiers() {
        let line = format!(
            "see {} and {} and {}",
            join(&["P3", ".04"]),
            join(&["D3", ".3.2"]),
            join(&["L5", ".4.3"])
        );
        assert_eq!(count("task-identifier", &line), 3);
        assert_eq!(
            count("task-identifier", &format!("{}-0014", join(&["AD", "R"]))),
            1
        );
        assert_eq!(
            count(
                "task-identifier",
                "section A.11 and HS0301 and Unicode 16.0"
            ),
            0
        );
    }

    #[test]
    fn local_paths() {
        let unix = join(&["at /Us", "ers/someone/x"]);
        assert_eq!(count("local-path", &unix), 1);
        let windows = join(&["at C:", "\\Us", "ers\\someone"]);
        assert_eq!(count("local-path", &windows), 1);
        assert_eq!(count("local-path", "at /outside/secret.txt and a:\\n"), 0);
    }

    #[test]
    fn authorship_words() {
        let name = join(&["Cla", "ude"]);
        assert_eq!(count("authorship", &format!("by {name}")), 1);
        let initials = join(&["A", "I"]);
        assert_eq!(count("authorship", &format!("the {initials} helped")), 1);
        assert_eq!(count("authorship", "said, then FAIL and maintain"), 0);
        let noun = join(&["age", "nt"]);
        assert_eq!(count("authorship", &format!("an {noun} ran")), 1);
        assert_eq!(count("authorship", &format!("the User-{noun} header")), 0);
        // A path-qualified third-party type name is a real identifier this
        // code must spell out to compile against, not the word this rule
        // means to catch.
        assert_eq!(count("authorship", &format!("ureq::{noun}::new()")), 0);
        let phrase = join(&["generated", " by"]);
        assert_eq!(count("authorship", &format!("{phrase} hand")), 1);
    }

    #[test]
    fn secrets() {
        let key = format!("{}{}", "AKIA", "ABCDEFGHIJKLMNOP");
        assert_eq!(count("secret", &format!("key = {key}")), 1);
        assert_eq!(count("secret", "AKIA is a prefix"), 0);
        let block = format!(
            "{}{}",
            join(&["-----BE", "GIN RSA "]),
            join(&["PRIVATE", " KEY-----"])
        );
        assert_eq!(count("secret", &block), 1);
        let token = format!("{}{}", "ghp_", "a".repeat(36));
        assert_eq!(count("secret", &token), 1);
    }

    #[test]
    fn review_identifiers() {
        let one = join(&["HS", "R-19"]);
        assert_eq!(count("review-identifier", &format!("see {one} above")), 1);
        let two = format!("{} and {}", join(&["HS", "R-3"]), join(&["HS", "R-30"]));
        assert_eq!(count("review-identifier", &two), 2);
        // Not a match without digits, or joined onto a longer word either side.
        assert_eq!(count("review-identifier", &join(&["HS", "R-"])), 0);
        assert_eq!(count("review-identifier", &join(&["xHS", "R-19"])), 0);
        assert_eq!(count("review-identifier", &join(&["HS", "R-19x"])), 0);
    }

    #[test]
    fn registry_and_old_references() {
        assert_eq!(count("registry", &join(&["a.code", "artifact.example"])), 1);
        assert_eq!(
            count("old-implementation", &join(&["the Ora", "cle here"])),
            1
        );
        assert_eq!(count("old-implementation", "an oracled word"), 0);
    }
}

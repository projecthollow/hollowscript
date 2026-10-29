//! `http` (section 16.9): URL parsing, host-scoped grants (section 9.5),
//! address safety, redirects and the request itself, kept apart from the
//! VM-facing binding in `default_grants.rs` the same way `capabilities.rs`
//! keeps `fs`'s logic apart from its binding.
//!
//! **What is deliberately not exact.** Two corners of the specification are
//! approximated rather than implemented in full, and are recorded here
//! rather than silently left out:
//!
//! - **Internationalised host names.** Section 16.9 says a scope value and a
//!   request host are compared "in its `xn--` form", i.e. Punycode-decoded
//!   internationalised names compare equal to their ASCII `xn--` encoding.
//!   This implementation compares host text as lower-cased ASCII only, with
//!   no IDNA normalisation: an already-`xn--`-encoded host matches an
//!   `xn--`-encoded grant, but a request written with the literal Unicode
//!   host does not match a grant written in `xn--` form, or vice versa.
//!   Correcting this needs an IDNA implementation, which is a dependency
//!   this change does not add.
//! - **Disguised numeric hosts.** The "Addresses" paragraph requires
//!   refusing a host written in a non-canonical numeric form (a bare
//!   decimal integer, octal or hex octets, a short-form dotted address, a
//!   zone identifier). [`looks_like_disguised_numeric_host`] catches the
//!   forms the specification names as examples (a bare decimal integer, an
//!   `0x`-prefixed or octal-looking dotted segment, fewer than four decimal
//!   segments) but does not attempt to replicate every historical `inet_aton`
//!   acceptance rule.
//!
//! Response decompression is limited to `gzip` (the `ureq` feature this
//! crate enables); a request that sets `Accept-Encoding: br` and receives a
//! Brotli body will fail to decode it as UTF-8 rather than decompressing it,
//! since Brotli support is not built in. The host itself never sends its own
//! `Accept-Encoding` (`.accept_encoding(AutoHeaderValue::None)` below), so
//! decompression only ever happens because the program's own request or the
//! server's own choice put a `Content-Encoding: gzip` on the response.
//!
//! **What every request is refused regardless of the grant.** A program's own
//! headers are `Err(Invalid)` ([`validate_headers`]) if they name `Host`,
//! because section 16.9 requires the granted name, not anything a program
//! writes, as both the `Host` header and the TLS name; `Content-Length`,
//! `Transfer-Encoding`, `Connection`, `Upgrade`, `TE`, `Trailer`, `Expect`,
//! `Keep-Alive` and `Proxy-Connection`, because they describe framing and
//! connection handling, which this client decides for itself from the body
//! it is given, and a value that disagrees with that is exactly how one
//! request is smuggled inside another on a connection a later request
//! reuses (a program supplying its own can never do anything but lie about
//! them); `Proxy-Authorization`, because this client uses no proxy and a
//! credential for one would only be sent to the origin; and the headers a
//! reverse proxy sets to tell the origin who really asked or what was really
//! requested (`Forwarded`, `X-Forwarded-For`, `-Host`, `-Proto`, `-Port` and
//! `-Server`, `X-Real-IP`, `X-Original-URL`, `X-Rewrite-URL`, and the three
//! method override headers), which an origin behind such a proxy may trust
//! over the request line, so a program must not be able to forge them. The
//! list is this host's own policy; the specification names only `Host`.
//!
//! **Every status is `Ok`.** `http_status_as_error(false)` below turns off
//! `ureq`'s default of turning a 4xx or 5xx status into an `Err`, matching
//! section 16.9's "a response with any status is returned as `Ok`".
//!
//! **No proxy.** `.proxy(None)` below stops `ureq` reading `HTTP_PROXY`,
//! `HTTPS_PROXY` or `ALL_PROXY` from the process environment: a proxy
//! resolves and connects to the *target* address on the host's own behalf,
//! which is exactly the address-safety check this module performs, done
//! before it, so relying on the environment would let a proxy such an
//! environment variable named silently take that decision away from the
//! grant.
//!
//! **Headers on a redirect.** Before a redirect target on a different origin
//! (scheme, host or port) is requested, every header the program supplied is
//! dropped except an allowlist of ones that carry no credential
//! ([`CROSS_ORIGIN_HEADERS`]: `Accept`, `Accept-Language`, `Cache-Control`,
//! `Content-Type`, `If-Modified-Since`, `If-None-Match`, `Pragma` and
//! `User-Agent`). Nothing in section 16.9 requires this, but a program that
//! addressed `Authorization`, `Cookie`, `X-Api-Key` or any other secret
//! header to one origin does not mean it for whatever origin a redirect
//! names, and a list of the headers that carry secrets is a list that is
//! always one name short; an allowlist fails the other way. A redirect to the
//! same origin keeps every header.
//!
//! **A response body is limited after it is decoded.** The limit
//! (`HttpLimits::max_body_bytes`, 8 MiB by default) applies to the bytes of
//! the body the program would receive, not the bytes that cross the wire:
//! `ureq` decompresses a gzip body on top of a limit that counts the
//! compressed bytes, so a few hundred kilobytes of gzip could become hundreds
//! of megabytes. The body is therefore read through the decoder, a chunk at a
//! time, and refused once it passes the limit; the compressed side is held to
//! twice the limit as well, so an incompressible body just under the limit is
//! not refused for the framing around it.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;
use std::time::{Duration, Instant};

/// `HttpError`'s variants (section 16.1), the shape every fallible `http`
/// call reports through.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum HttpErrorKind {
    /// The grant does not allow the host, or the resolved address is one
    /// the "Addresses" paragraph refuses.
    Denied,
    /// The URL, method or headers do not meet section 16.9's shape.
    Invalid,
    /// The response body is not valid UTF-8, or the request could not be
    /// completed for another reason.
    Failed,
    /// The request's total time limit passed.
    Timeout,
    /// The response body is larger than the host's limit.
    TooLarge,
}

/// The host's own limits on a request (section 16.9, "Limits"), configurable
/// so a test can use a short timeout and a small body cap.
#[derive(Copy, Clone, Debug)]
pub(crate) struct HttpLimits {
    /// The total time allowed for one request, including every redirect.
    pub total_time: Duration,
    /// The largest response body accepted, in bytes, per request in the
    /// chain (checked on the final response, after redirects).
    pub max_body_bytes: u64,
}

impl Default for HttpLimits {
    /// Section 16.9's own stated defaults: 30 seconds, 8 MiB.
    fn default() -> HttpLimits {
        HttpLimits {
            total_time: Duration::from_secs(30),
            max_body_bytes: 8 * 1024 * 1024,
        }
    }
}

/// At most five redirects are followed (section 16.9, "Redirects").
const MAX_REDIRECTS: u32 = 5;

/// One granted `http` scope value (section 9.5, section 16.9's own grammar
/// for a scope value).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum HttpScope {
    /// An exact host name, already lower-cased with a trailing dot removed.
    Host { name: String, port: Option<u16> },
    /// `*.name`: any host with `name` as a proper suffix after a dot.
    Wildcard { suffix: String, port: Option<u16> },
    /// A literal address.
    Ip { addr: IpAddr, port: Option<u16> },
}

/// Parses one `http` grant scope value.
///
/// # Errors
///
/// Returns a message for a value with an empty host or an unparseable port.
pub(crate) fn parse_http_scope(text: &str) -> Result<HttpScope, String> {
    if text.is_empty() {
        return Err("'http' was given an empty host".to_owned());
    }
    if let Some(rest) = text.strip_prefix('[') {
        // A bracketed IPv6 literal, optionally followed by ":port".
        let Some(end) = rest.find(']') else {
            return Err(format!("'{text}' is missing a closing ']'"));
        };
        let (addr_text, after) = rest.split_at(end);
        let addr = Ipv6Addr::from_str(addr_text)
            .map_err(|_| format!("'{addr_text}' is not a literal IPv6 address"))?;
        let port = parse_optional_port(after.strip_prefix(']').unwrap_or(after))?;
        return Ok(HttpScope::Ip {
            addr: IpAddr::V6(addr),
            port,
        });
    }
    let (host_text, port) = match text.rsplit_once(':') {
        Some((host, port)) => (host, Some(parse_port(port)?)),
        None => (text, None),
    };
    if host_text.is_empty() {
        return Err("'http' was given an empty host".to_owned());
    }
    if let Some(suffix) = host_text.strip_prefix("*.") {
        if suffix.is_empty() {
            return Err(format!("'{text}' has nothing after '*.'"));
        }
        return Ok(HttpScope::Wildcard {
            suffix: normalise_host(suffix),
            port,
        });
    }
    if let Ok(addr) = IpAddr::from_str(host_text) {
        return Ok(HttpScope::Ip { addr, port });
    }
    Ok(HttpScope::Host {
        name: normalise_host(host_text),
        port,
    })
}

fn parse_port(text: &str) -> Result<u16, String> {
    text.parse::<u16>()
        .map_err(|_| format!("'{text}' is not a port number"))
}

fn parse_optional_port(after_bracket: &str) -> Result<Option<u16>, String> {
    match after_bracket.strip_prefix(':') {
        Some(port) => Ok(Some(parse_port(port)?)),
        None if after_bracket.is_empty() => Ok(None),
        None => Err(format!("'{after_bracket}' is not ':port'")),
    }
}

/// Lower-cases `host` (ASCII only; see the module doc's IDNA note) and
/// removes one trailing dot.
fn normalise_host(host: &str) -> String {
    host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase()
}

/// The default port for a scheme (section 16.9).
fn default_port(https: bool) -> u16 {
    if https { 443 } else { 80 }
}

/// Whether `scopes` grants a request to `host` (already normalised) on
/// `port`, for a request whose URL wrote `host` as a name (section 16.9,
/// "Grants").
pub(crate) fn host_scope_allows(scopes: &[HttpScope], host: &str, https: bool, port: u16) -> bool {
    scopes.iter().any(|scope| match scope {
        HttpScope::Host { name, port: p } => name == host && port_allowed(*p, https, port),
        HttpScope::Wildcard { suffix, port: p } => {
            host.strip_suffix(suffix)
                .is_some_and(|prefix| prefix.ends_with('.'))
                && port_allowed(*p, https, port)
        }
        HttpScope::Ip { .. } => false,
    })
}

/// Whether `scopes` grants a request whose URL wrote a literal address
/// directly (section 16.9: "a host name never matches an address it
/// resolves to", so only an `Ip` scope value matches a literal address URL).
pub(crate) fn ip_scope_allows(scopes: &[HttpScope], addr: IpAddr, https: bool, port: u16) -> bool {
    scopes.iter().any(|scope| match scope {
        HttpScope::Ip { addr: a, port: p } => *a == addr && port_allowed(*p, https, port),
        _ => false,
    })
}

fn port_allowed(scope_port: Option<u16>, https: bool, request_port: u16) -> bool {
    match scope_port {
        Some(p) => p == request_port,
        None => request_port == default_port(https),
    }
}

/// A parsed, validated absolute `http` or `https` URL (section 16.9's
/// "Request" paragraph and "Addresses" paragraph).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Url {
    pub https: bool,
    /// Lower-cased, trailing dot removed; the literal text of an IP address
    /// when `host_is_ip` (an IPv6 literal without brackets).
    pub host: String,
    pub host_is_ip: bool,
    pub port: u16,
    /// Everything from the path onward, always starting with `/`.
    pub path_and_query: String,
}

impl Url {
    /// Parses `text` as an absolute `http` or `https` URL with a host and no
    /// user information (section 16.9).
    ///
    /// # Errors
    ///
    /// Returns `()` for anything that does not meet that shape; the caller
    /// turns this into `Err(Invalid(_))` or, when parsing a redirect target,
    /// returns the response as `Ok` per the "Redirects" paragraph.
    pub(crate) fn parse(text: &str) -> Result<Url, ()> {
        let (scheme, rest) = text.split_once("://").ok_or(())?;
        let https = match scheme.to_ascii_lowercase().as_str() {
            "http" => false,
            "https" => true,
            _ => return Err(()),
        };
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(authority_end);
        if authority.contains('@') {
            // Section 16.9: "no user information".
            return Err(());
        }
        if authority.is_empty() {
            return Err(());
        }
        let (host_text, host_is_ip, port_text) =
            if let Some(after_bracket) = authority.strip_prefix('[') {
                let end = after_bracket.find(']').ok_or(())?;
                let (addr, after) = after_bracket.split_at(end);
                Ipv6Addr::from_str(addr).map_err(|_| ())?;
                let after = after.strip_prefix(']').unwrap_or(after);
                (addr.to_owned(), true, after.strip_prefix(':'))
            } else {
                match authority.rsplit_once(':') {
                    Some((host, port)) => (host.to_owned(), false, Some(port)),
                    None => (authority.to_owned(), false, None),
                }
            };
        if host_text.is_empty() {
            return Err(());
        }
        let host_is_ip = host_is_ip || IpAddr::from_str(&host_text).is_ok();
        if !host_is_ip && looks_like_disguised_numeric_host(&host_text) {
            return Err(());
        }
        let port = match port_text {
            Some(p) => p.parse::<u16>().map_err(|_| ())?,
            None => default_port(https),
        };
        let host = if host_is_ip {
            host_text.to_ascii_lowercase()
        } else {
            normalise_host(&host_text)
        };
        let path_and_query = if tail.is_empty() {
            "/".to_owned()
        } else if tail.starts_with('/') {
            drop_fragment(tail)
        } else {
            // `?query` or `#fragment` with no path: the path is still `/`.
            format!("/{}", drop_fragment(tail))
        };
        Ok(Url {
            https,
            host,
            host_is_ip,
            port,
            path_and_query,
        })
    }

    /// This URL, without its query, for reconstructing an absolute URL
    /// string (`ureq` needs one to build its request).
    pub(crate) fn to_text(&self) -> String {
        let scheme = if self.https { "https" } else { "http" };
        let host = if self.host_is_ip && self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.port == default_port(self.https) {
            format!("{scheme}://{host}{}", self.path_and_query)
        } else {
            format!("{scheme}://{host}:{}{}", self.port, self.path_and_query)
        }
    }
}

fn drop_fragment(text: &str) -> String {
    text.split('#').next().unwrap_or(text).to_owned()
}

/// Whether `host` is a numeric host written in a form the "Addresses"
/// paragraph refuses: see the module doc for what this does and does not
/// catch.
fn looks_like_disguised_numeric_host(host: &str) -> bool {
    if host.contains('%') {
        // A zone identifier.
        return true;
    }
    // A single trailing dot, if present, is removed before the shape is
    // judged, but remembered: a dotted-decimal host that is a genuinely
    // canonical, in-range IPv4 address once the dot is gone is still a
    // disguise, because a trailing dot on a numeric address is itself
    // non-canonical (the "Addresses" paragraph's own numeric forms never
    // have one), even though the same trailing dot on an ordinary host name
    // is perfectly normal and handled elsewhere (`normalise_host`).
    let core = host
        .strip_suffix('.')
        .filter(|s| !s.is_empty())
        .unwrap_or(host);
    let had_trailing_dot = core.len() != host.len();
    if !core.contains('.') {
        // A bare number, or an unadorned hex/octal literal: never a name a
        // trailing dot could legitimately apply to either.
        return (!core.is_empty() && core.bytes().all(|b| b.is_ascii_digit()))
            || is_hex_literal(core);
    }
    let segments: Vec<&str> = core.split('.').collect();
    if segments.iter().any(|s| s.is_empty()) {
        return false;
    }
    let all_decimal = segments
        .iter()
        .all(|s| s.bytes().all(|b| b.is_ascii_digit()));
    if !all_decimal {
        // A hex segment (`0x7f`) inside an otherwise dotted host.
        return segments.iter().any(|s| is_hex_literal(s));
    }
    // All-decimal and dotted: canonical IPv4 is exactly four segments, each
    // in range 0-255 with no leading zero (other than "0" itself), and, per
    // the comment above, without a trailing dot.
    let canonical = segments.len() == 4
        && segments.iter().all(|s| {
            !(s.len() > 1 && s.starts_with('0')) && s.parse::<u16>().is_ok_and(|v| v <= 255)
        });
    !canonical || had_trailing_dot
}

/// Whether `segment` is a bare hexadecimal literal (`0x7f`, `0X7F`): an `0x`
/// or `0X` prefix followed by at least one hex digit and nothing else.
fn is_hex_literal(segment: &str) -> bool {
    let digits = segment
        .strip_prefix("0x")
        .or_else(|| segment.strip_prefix("0X"));
    digits.is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Whether `addr` is one the "Addresses" paragraph refuses by default:
/// loopback, private, link-local, multicast, or a cloud metadata address,
/// classified against the full IANA special-purpose registries for IPv4
/// (`iana-ipv4-special-registry`) and IPv6 (`iana-ipv6-special-registry`),
/// not only the four `std::net::Ipv4Addr`/`Ipv6Addr` predicates the
/// specification's own wording happens to name. An IPv6 address that embeds
/// an IPv4 one (an IPv4-mapped or IPv4-compatible address, the NAT64
/// well-known prefixes, 6to4 or Teredo) is refused whenever the embedded
/// address on its own would be, in addition to the ordinary IPv6 checks: a
/// program cannot reach a refused IPv4 destination just by writing it inside
/// one of these wrapper forms instead.
pub(crate) fn is_disallowed_address(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => is_disallowed_v4(v4),
        IpAddr::V6(v6) => {
            embedded_ipv4_candidates(v6)
                .into_iter()
                .any(is_disallowed_v4)
                || is_disallowed_v6(v6)
        }
    }
}

/// IPv4 addresses the IANA special-purpose registry marks as not globally
/// reachable, or (100.64.0.0/10, 192.0.0.0/24) reachable only within a
/// provider's own network: loopback, private, link-local and multicast are
/// `std::net::Ipv4Addr`'s own predicates; the rest are checked directly.
fn is_disallowed_v4(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    v4.is_loopback()      // 127.0.0.0/8
        || v4.is_private() // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
        || v4.is_link_local() // 169.254.0.0/16
        || v4.is_multicast()  // 224.0.0.0/4
        || v4.is_broadcast()  // 255.255.255.255
        || o[0] == 0           // 0.0.0.0/8 ("this network", includes 0.0.0.0 itself)
        || (o[0] == 100 && (o[1] & 0b1100_0000) == 64) // 100.64.0.0/10, CGNAT shared address space
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)     // 192.0.0.0/24, IETF protocol assignments
        || (o[0] == 192 && o[1] == 88 && o[2] == 99)   // 192.88.99.0/24, deprecated 6to4 relay anycast
        || (o[0] == 192 && o[1] == 0 && o[2] == 2)     // 192.0.2.0/24, documentation (TEST-NET-1)
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19)) // 198.18.0.0/15, benchmarking
        || (o[0] == 198 && o[1] == 51 && o[2] == 100)  // 198.51.100.0/24, documentation (TEST-NET-2)
        || (o[0] == 203 && o[1] == 0 && o[2] == 113)   // 203.0.113.0/24, documentation (TEST-NET-3)
        || o[0] >= 240 // 240.0.0.0/4, reserved (255.255.255.255 is also caught above)
}

/// IPv6 addresses the IANA special-purpose registry marks as not globally
/// reachable, beyond the embedded-IPv4 forms [`embedded_ipv4_candidates`]
/// already checks.
fn is_disallowed_v6(v6: Ipv6Addr) -> bool {
    v6.is_loopback() // ::1/128
        || v6.is_unspecified() // ::/128
        || v6.is_multicast() // ff00::/8
        || is_unique_local(v6) // fc00::/7 (RFC 4193)
        || is_unicast_link_local(v6) // fe80::/10 (RFC 4291)
        || is_deprecated_site_local(v6) // fec0::/10 (RFC 3879, deprecated)
        || is_ipv6_documentation(v6) // 2001:db8::/32, 3fff::/20
        || is_discard_only(v6) // 100::/64
        || is_benchmarking_v6(v6) // 2001:2::/48
        || is_orchid(v6) // 2001:10::/28 and 2001:20::/28
        || is_srv6_sid(v6) // 5f00::/16
        || is_ipv4_translated(v6) // ::ffff:0:0:0/96
}

/// `fc00::/7` (RFC 4193): not exposed as a stable `std` method at this
/// workspace's `rust-version`, so checked directly on the first segment.
fn is_unique_local(addr: Ipv6Addr) -> bool {
    (addr.segments()[0] & 0xfe00) == 0xfc00
}

/// `fe80::/10` (RFC 4291), likewise checked directly.
fn is_unicast_link_local(addr: Ipv6Addr) -> bool {
    (addr.segments()[0] & 0xffc0) == 0xfe80
}

/// `fec0::/10` (RFC 3879), the deprecated IPv6 site-local block: obsolete,
/// but still routable on a network that has not removed it, and the review's
/// own fix direction names it explicitly.
fn is_deprecated_site_local(addr: Ipv6Addr) -> bool {
    (addr.segments()[0] & 0xffc0) == 0xfec0
}

/// The IPv6 documentation prefixes: `2001:db8::/32` (RFC 3849) and
/// `3fff::/20` (RFC 9637, the first 16 bits `3fff` and the next 4 zero).
fn is_ipv6_documentation(addr: Ipv6Addr) -> bool {
    let s = addr.segments();
    (s[0] == 0x2001 && s[1] == 0x0db8) || (s[0] == 0x3fff && (s[1] & 0xf000) == 0)
}

/// `100::/64` (RFC 6666), the discard-only prefix.
fn is_discard_only(addr: Ipv6Addr) -> bool {
    let s = addr.segments();
    s[0] == 0x0100 && s[1] == 0 && s[2] == 0 && s[3] == 0
}

/// `2001:2::/48` (RFC 5180), the IPv6 benchmarking prefix.
fn is_benchmarking_v6(addr: Ipv6Addr) -> bool {
    let s = addr.segments();
    s[0] == 0x2001 && s[1] == 0x0002 && s[2] == 0
}

/// `2001:10::/28` (RFC 4843, the deprecated `ORCHID`) and `2001:20::/28`
/// (RFC 7343, `ORCHIDv2`): hashes used as identifiers, never routed.
fn is_orchid(addr: Ipv6Addr) -> bool {
    let s = addr.segments();
    s[0] == 0x2001 && ((s[1] & 0xfff0) == 0x0010 || (s[1] & 0xfff0) == 0x0020)
}

/// `5f00::/16` (RFC 9602), segment routing SIDs: for use inside a provider's
/// own network.
fn is_srv6_sid(addr: Ipv6Addr) -> bool {
    addr.segments()[0] == 0x5f00
}

/// `::ffff:0:0:0/96` (RFC 2765, "IPv4-translated", with the 16 bits of ones
/// *before* the zeros that the IPv4-mapped `::ffff:0:0/96` has after them):
/// not reachable as such, and the embedded IPv4 address is checked as well
/// (see [`embedded_ipv4_candidates`]).
fn is_ipv4_translated(addr: Ipv6Addr) -> bool {
    let s = addr.segments();
    s[0] == 0 && s[1] == 0 && s[2] == 0 && s[3] == 0 && s[4] == 0xffff && s[5] == 0
}

/// Every IPv4 address `addr` carries embedded in one of the forms section
/// 16.9's "Addresses" paragraph and this module's own doc comment name:
/// IPv4-mapped and the deprecated IPv4-compatible `::/96` forms, the two
/// NAT64 well-known prefixes, 6to4, and Teredo (both the server address in
/// the middle of the address and the client address the last 32 bits
/// obfuscate by XOR, per RFC 4380). Usually empty or one address; Teredo can
/// contribute two.
fn embedded_ipv4_candidates(addr: Ipv6Addr) -> Vec<Ipv4Addr> {
    let s = addr.segments();
    let mut found = Vec::new();
    if let Some(v4) = addr.to_ipv4_mapped() {
        // ::ffff:0:0/96.
        found.push(v4);
    } else if s[0] == 0 && s[1] == 0 && s[2] == 0 && s[3] == 0 && s[4] == 0 && s[5] == 0 {
        // ::/96, the deprecated "IPv4-compatible" form; excludes ::ffff:0:0/96
        // (handled above) since that has s[5] == 0xffff, not 0.
        found.push(last_32_as_ipv4(s));
    }
    if is_ipv4_translated(addr) {
        // ::ffff:0:0:0/96 (RFC 2765): the embedded address is the last 32 bits.
        found.push(last_32_as_ipv4(s));
    }
    if s[0] == 0x0064 && s[1] == 0xff9b && s[2] == 0 && s[3] == 0 && s[4] == 0 && s[5] == 0 {
        // 64:ff9b::/96, the NAT64 well-known prefix.
        found.push(last_32_as_ipv4(s));
    }
    if s[0] == 0x0064 && s[1] == 0xff9b && s[2] == 1 {
        // 64:ff9b:1::/48, the NAT64 local-use prefix (RFC 8215). The exact
        // embedding also depends on a prefix length beyond /48 that is not
        // recoverable from the address alone; the last 32 bits are checked
        // as an approximation, which is exact for a /96 embedding and, for
        // any other prefix length in this block, is still an address this
        // host would otherwise resolve to, not a false positive.
        found.push(last_32_as_ipv4(s));
    }
    if s[0] == 0x2002 {
        // 2002::/16, 6to4: the embedded IPv4 address is the next 32 bits.
        found.push(two_segments_as_ipv4(s[1], s[2]));
    }
    if s[0] == 0x2001 && s[1] == 0 {
        // 2001::/32, Teredo (RFC 4380): the server's own IPv4 address is
        // segments 2 and 3 unobfuscated, and the client's is the last 32
        // bits, each bit inverted.
        found.push(two_segments_as_ipv4(s[2], s[3]));
        let obfuscated = last_32_as_ipv4(s);
        let o = obfuscated.octets();
        found.push(Ipv4Addr::new(!o[0], !o[1], !o[2], !o[3]));
    }
    found
}

/// Two consecutive 16-bit segments of an IPv6 address, read as a big-endian
/// IPv4 address (`u16::to_be_bytes`, not a truncating cast: each segment
/// contributes both of its bytes, none discarded).
fn two_segments_as_ipv4(high: u16, low: u16) -> Ipv4Addr {
    let [a, b] = high.to_be_bytes();
    let [c, d] = low.to_be_bytes();
    Ipv4Addr::new(a, b, c, d)
}

/// The last two 16-bit segments of an IPv6 address's segment array, read as
/// a big-endian IPv4 address.
fn last_32_as_ipv4(segments: [u16; 8]) -> Ipv4Addr {
    two_segments_as_ipv4(segments[6], segments[7])
}

/// Resolves `reference` (a `Location` header's value) against `base`
/// (section 16.9: "resolved against the request URL by RFC 3986"), for
/// following a redirect.
///
/// # Errors
///
/// Returns `()` if `reference` cannot be parsed, or the result is not a
/// valid `http` or `https` URL with a host: the caller then returns the
/// response as `Ok`, per the "Redirects" paragraph, rather than treating
/// this as `Err(Invalid(_))`.
pub(crate) fn resolve_reference(base: &Url, reference: &str) -> Result<Url, ()> {
    if reference.is_empty() {
        return Err(());
    }
    // An absolute reference is parsed directly; this also matches the
    // ordinary case of a `Location` giving a full URL.
    if let Ok(url) = Url::parse(reference) {
        return Ok(url);
    }
    // A scheme-relative reference ("//host/path").
    if let Some(rest) = reference.strip_prefix("//") {
        let scheme = if base.https { "https" } else { "http" };
        return Url::parse(&format!("{scheme}://{rest}"));
    }
    // An absolute-path or relative-path reference, resolved against the
    // base URL's own authority (RFC 3986 section 5.3). A colon in the first
    // path segment means the reference names its own scheme
    // (`javascript:...`, `mailto:...`), which this only-`http`/`https`
    // resolver refuses rather than guesses at.
    if reference.starts_with('#') {
        return Err(());
    }
    let first_segment = reference.split(['/', '?', '#']).next().unwrap_or("");
    if first_segment.contains(':') {
        return Err(());
    }
    let (path_part, query) = match reference.split_once('?') {
        Some((p, q)) => (p.to_owned(), Some(drop_fragment(q))),
        None => (drop_fragment(reference), None),
    };
    let new_path = if path_part.starts_with('/') {
        path_part
    } else {
        merge_paths(&base.path_and_query, &path_part)
    };
    let path_and_query = match query {
        Some(query) if !query.is_empty() => format!("{new_path}?{query}"),
        _ => new_path,
    };
    Ok(Url {
        https: base.https,
        host: base.host.clone(),
        host_is_ip: base.host_is_ip,
        port: base.port,
        path_and_query: remove_dot_segments(&path_and_query),
    })
}

/// Combines a relative path onto `base_path_and_query`'s path (RFC 3986
/// section 5.3, "merge"): everything up to and including the last `/`,
/// with the reference's path appended.
fn merge_paths(base_path_and_query: &str, reference_path: &str) -> String {
    let base_path = base_path_and_query
        .split('?')
        .next()
        .unwrap_or(base_path_and_query);
    match base_path.rfind('/') {
        Some(i) => format!("{}{reference_path}", &base_path[..=i]),
        None => format!("/{reference_path}"),
    }
}

/// RFC 3986 section 5.2.4: removes `.` and `..` segments from a path.
fn remove_dot_segments(path: &str) -> String {
    let mut output: Vec<&str> = Vec::new();
    let trailing_slash = path.ends_with('/') || path.ends_with("/.") || path.ends_with("/..");
    for segment in path.split('/') {
        match segment {
            "." => {}
            ".." => {
                output.pop();
            }
            _ => output.push(segment),
        }
    }
    let mut result = output.join("/");
    if !result.starts_with('/') {
        result.insert(0, '/');
    }
    if trailing_slash && !result.ends_with('/') {
        result.push('/');
    }
    result
}

impl Url {
    /// This URL's host, parsed as an address, when it was written as one
    /// (section 16.9: "a host name never matches an address it resolves
    /// to", so this is what tells a grant check which kind of scope
    /// applies).
    fn ip(&self) -> Option<IpAddr> {
        if self.host_is_ip {
            IpAddr::from_str(&self.host).ok()
        } else {
            None
        }
    }
}

/// One `http` request (section 16.9's `HttpRequest`, already read out of the
/// language's own record).
#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

/// One `http` response (`HttpResponse`), ready to be written back as a
/// record.
#[derive(Clone, Debug)]
pub(crate) struct Response {
    pub status: i64,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// The seven methods section 16.9 allows, always upper case.
const ALLOWED_METHODS: [&str; 7] = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

/// Whether a status is one of the five redirect statuses section 16.9 names.
fn is_redirect_status(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// Header names a program's own `headers` may never set (compared
/// case-insensitively), and why, by group:
///
/// - `host`: section 16.9 requires it to be the granted name, never anything
///   a program writes.
/// - Framing and connection handling, which this client decides from the
///   body it is actually given and a program can only lie about: a value
///   that disagrees is how a request gets smuggled inside another one on a
///   connection a later request reuses. `content-length`,
///   `transfer-encoding`, `connection`, `upgrade`, `te`, `trailer`,
///   `expect`, `keep-alive` and `proxy-connection`.
/// - `proxy-authorization`: no proxy is used (see the module documentation),
///   so a credential for one could only be handed to the origin.
/// - What a reverse proxy sets to tell an origin who really asked or what was
///   really requested, which an origin may trust over the request line:
///   `forwarded`, the `x-forwarded-*` family, `x-real-ip`, `x-original-url`,
///   `x-rewrite-url`, and the method overrides `x-http-method`,
///   `x-http-method-override` and `x-method-override` (which would let a
///   `POST`, the one method a grant might allow, act as a `DELETE`).
const FORBIDDEN_HEADERS: [&str; 24] = [
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "upgrade",
    "te",
    "trailer",
    "expect",
    "keep-alive",
    "proxy-connection",
    "proxy-authorization",
    "forwarded",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "x-forwarded-port",
    "x-forwarded-server",
    "x-forwarded-prefix",
    "x-real-ip",
    "x-original-url",
    "x-rewrite-url",
    "x-http-method",
    "x-http-method-override",
    "x-method-override",
];

/// The only request headers a redirect to a different origin carries on
/// (see the module documentation): none of them is a credential.
const CROSS_ORIGIN_HEADERS: [&str; 8] = [
    "accept",
    "accept-language",
    "cache-control",
    "content-type",
    "if-modified-since",
    "if-none-match",
    "pragma",
    "user-agent",
];

/// Section 16.9: "Two request header names that are the same after
/// lower-casing... make the request `Err(Invalid)`", as does a name or
/// value containing CR, LF or U+0000; a name in [`FORBIDDEN_HEADERS`] is
/// `Err(Invalid)` for the same reason (see the module doc).
fn validate_headers(headers: &[(String, String)]) -> Result<(), HttpErrorKind> {
    let bad_byte = |s: &str| s.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0);
    let mut seen: Vec<String> = Vec::with_capacity(headers.len());
    for (name, value) in headers {
        if name.is_empty() || bad_byte(name) || bad_byte(value) {
            return Err(HttpErrorKind::Invalid);
        }
        let lower = name.to_ascii_lowercase();
        if seen.contains(&lower) || FORBIDDEN_HEADERS.contains(&lower.as_str()) {
            return Err(HttpErrorKind::Invalid);
        }
        seen.push(lower);
    }
    Ok(())
}

/// Whether `scopes` allows a request to `url` (section 16.9's "Grants" and
/// "Addresses" paragraphs, the part of the check that does not need a
/// connection: the host-scope match. Address safety for a resolved name is
/// checked again, per connection, by [`SafeResolver`]).
///
/// A literal-address URL is granted only by an `Ip` scope value naming that
/// exact address and port (`ip_scope_allows`); section 16.9 says as much
/// directly ("a host name never matches an address it resolves to"), so an
/// `Ip` scope granting the address is, on its own, both the grant and the
/// "Addresses" paragraph's literal-address exception for a request that
/// names the address itself: there is nothing further to check here, and in
/// particular no separate, port-blind "is this address named literally
/// anywhere in the grant" question, which is what let one `Ip` scope's
/// address stand in for a different scope's, and a different port, in the
/// resolver below before this was fixed.
fn grant_allows(scopes: &[HttpScope], url: &Url) -> bool {
    match url.ip() {
        Some(addr) => ip_scope_allows(scopes, addr, url.https, url.port),
        None => host_scope_allows(scopes, &url.host, url.https, url.port),
    }
}

/// Performs `request` (section 16.9), following redirects, checking every
/// target against `scopes` and the "Addresses" paragraph as if it had been
/// requested directly, and applying `limits`.
///
/// `limits.total_time` is one deadline for the whole call: computed
/// once, here, before the first attempt, and passed down as an
/// [`Instant`] so that every redirect hop and DNS lookup spends only what is
/// left of it rather than restarting its own fresh `limits.total_time`, which
/// is what let a slow enough redirect chain run for a multiple of the
/// configured limit before this was fixed.
///
/// # Errors
///
/// The `HttpError` variant section 16.9 or 9.5 rule 3 calls for.
pub(crate) fn perform(
    request: &Request,
    scopes: &[HttpScope],
    limits: &HttpLimits,
) -> Result<Response, HttpErrorKind> {
    let method = request.method.as_str();
    if !ALLOWED_METHODS.contains(&method) {
        return Err(HttpErrorKind::Invalid);
    }
    validate_headers(&request.headers)?;
    let mut url = Url::parse(&request.url).map_err(|()| HttpErrorKind::Invalid)?;
    let mut method = method.to_owned();
    let mut headers = request.headers.clone();
    let mut body = request.body.clone();
    let deadline = Instant::now() + limits.total_time;

    for attempt in 0..=MAX_REDIRECTS {
        if !grant_allows(scopes, &url) {
            return Err(HttpErrorKind::Denied);
        }
        if Instant::now() >= deadline {
            return Err(HttpErrorKind::Timeout);
        }
        let raw = send_once(&method, &url, &headers, body.as_deref(), scopes, deadline)?;
        if !is_redirect_status(raw.status) {
            return finish(raw, limits);
        }
        if attempt == MAX_REDIRECTS {
            // A sixth redirect in a row: "more than five redirects".
            return Err(HttpErrorKind::Failed);
        }
        let Some(location) = header_value(&raw.headers, "location") else {
            // No `Location`: the response is returned as `Ok`.
            return finish(raw, limits);
        };
        let Ok(next) = resolve_reference(&url, &location) else {
            return finish(raw, limits);
        };
        if matches!(raw.status, 301..=303) && method != "GET" && method != "HEAD" {
            "GET".clone_into(&mut method);
            body = None;
        }
        // 307 and 308 repeat the request unchanged; everything else in the
        // loop already carries `method` and `body` forward as they are.
        if !same_origin(&url, &next) {
            // Whatever a program addressed to one origin is dropped before a
            // redirect hands the request to a different one (scheme, host
            // or port), except the few headers that carry no credential
            // (`CROSS_ORIGIN_HEADERS`): section 16.9 does not require this,
            // but sending a `Cookie` or an API key on regardless is exactly
            // the cross-origin credential leak browsers and `curl` both
            // refuse by default, and a list of the names that carry
            // credentials is always one name short.
            headers.retain(|(name, _)| {
                CROSS_ORIGIN_HEADERS
                    .iter()
                    .any(|allowed| name.eq_ignore_ascii_case(allowed))
            });
        }
        url = next;
    }
    // Unreachable: the loop above always returns by `attempt == MAX_REDIRECTS`.
    Err(HttpErrorKind::Failed)
}

/// Whether `a` and `b` are the same origin: scheme, host and port all equal.
fn same_origin(a: &Url, b: &Url) -> bool {
    a.https == b.https && a.host == b.host && a.port == b.port
}

fn header_value(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
}

/// A response as read off the wire, before the body size limit is charged
/// (section 16.9's own body limit, checked once the redirect chain has
/// settled on a final response).
struct RawResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body_reader: ureq::Body,
}

/// Reads `raw`'s body under `limits.max_body_bytes`, turning it into the
/// [`Response`] the language sees.
///
/// The limit is on the *decoded* body: `ureq`'s own `limit` counts the bytes
/// before a gzip decoder, so it alone let a few hundred kilobytes of gzip be
/// decoded into hundreds of megabytes (see the module documentation). The
/// body is read through the decoder, and through `Read::take`, a chunk at a
/// time; one byte more than the limit is as far as it is ever read. The
/// compressed side keeps a limit of its own, twice the cap plus 64 KiB.
fn finish(raw: RawResponse, limits: &HttpLimits) -> Result<Response, HttpErrorKind> {
    use std::io::Read as _;

    let mut body_reader = raw.body_reader;
    let wire_limit = limits
        .max_body_bytes
        .saturating_mul(2)
        .saturating_add(64 * 1024);
    let mut decoded = body_reader.with_config().limit(wire_limit).reader();
    let cap = limits.max_body_bytes.saturating_add(1);
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    let mut reader = (&mut decoded).take(cap);
    loop {
        let read = reader.read(&mut chunk).map_err(|e| map_read_error(&e))?;
        if read == 0 {
            break;
        }
        let Some(filled) = chunk.get(..read) else {
            return Err(HttpErrorKind::Failed);
        };
        bytes.extend_from_slice(filled);
    }
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limits.max_body_bytes {
        return Err(HttpErrorKind::TooLarge);
    }
    let body = String::from_utf8(bytes).map_err(|_| HttpErrorKind::Failed)?;
    Ok(Response {
        status: i64::from(raw.status),
        headers: raw.headers,
        body,
    })
}

/// The `HttpError` for a failed read of a body: `ureq` reports its own
/// errors (a timeout, its limit) inside the `io::Error` the `Read` trait
/// has room for, so they are taken back out.
fn map_read_error(error: &std::io::Error) -> HttpErrorKind {
    match error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<ureq::Error>())
    {
        Some(ureq::Error::Timeout(_)) => HttpErrorKind::Timeout,
        Some(ureq::Error::BodyExceedsLimit(_)) => HttpErrorKind::TooLarge,
        _ => match error.kind() {
            std::io::ErrorKind::TimedOut => HttpErrorKind::Timeout,
            _ => HttpErrorKind::Failed,
        },
    }
}

/// Sends one request (no redirect handling: that is the caller's own loop),
/// through a fresh HTTP client whose resolver enforces the "Addresses"
/// paragraph for this one connection. `deadline` is the one total-time
/// deadline `perform` computed for the whole call; this hop gets
/// only what remains of it, and a request that arrives here with none left
/// is refused before it opens a connection at all.
fn send_once(
    method: &str,
    url: &Url,
    headers: &[(String, String)],
    body: Option<&str>,
    scopes: &[HttpScope],
    deadline: Instant,
) -> Result<RawResponse, HttpErrorKind> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(HttpErrorKind::Timeout);
    }
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(remaining))
        .max_redirects(0)
        // Every status is `Ok` to the language (section 16.9): `ureq`'s own
        // default of turning a 4xx or 5xx into an `Err` is switched off, so
        // a 404 or a 500 reaches `finish` like any other response.
        .http_status_as_error(false)
        // No proxy: `HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY` are read
        // from the process environment by `ureq`'s own default, and a
        // CONNECT proxy resolves and connects to the target on the host's
        // behalf, bypassing `SafeResolver` below entirely.
        .proxy(None)
        // The host sends no `Accept-Encoding` of its own (section 16.9);
        // decompression (the `gzip` feature) still happens whenever a
        // response actually carries `Content-Encoding: gzip`, whether that
        // is because the program's own request asked for it or because the
        // server sent one anyway.
        .accept_encoding(ureq::config::AutoHeaderValue::None)
        .build();
    let client = ureq::Agent::with_parts(
        config,
        ureq::unversioned::transport::DefaultConnector::default(),
        SafeResolver {
            scopes: scopes.to_vec(),
            deadline,
        },
    );
    let http_method =
        ureq::http::Method::from_bytes(method.as_bytes()).map_err(|_| HttpErrorKind::Invalid)?;
    let mut builder = ureq::http::Request::builder()
        .method(http_method)
        .uri(url.to_text());
    for (name, value) in headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    let outcome = if let Some(text) = body {
        let request = builder
            .body(text.to_owned())
            .map_err(|_| HttpErrorKind::Invalid)?;
        client.run(request)
    } else {
        let request = builder.body(()).map_err(|_| HttpErrorKind::Invalid)?;
        client.run(request)
    };
    let response = outcome.map_err(map_error)?;
    let status = response.status().as_u16();
    let headers = response_headers(response.headers());
    let body_reader = response.into_body();
    Ok(RawResponse {
        status,
        headers,
        body_reader,
    })
}

/// Groups `headers` by lower-cased name, values in encounter order joined
/// by `", "` (section 16.9's `HttpResponse.headers`).
fn response_headers(headers: &ureq::http::HeaderMap) -> Vec<(String, String)> {
    let mut seen: Vec<String> = Vec::new();
    let mut result = Vec::new();
    for name in headers.keys() {
        let lower = name.as_str().to_ascii_lowercase();
        if seen.contains(&lower) {
            continue;
        }
        seen.push(lower.clone());
        let joined = headers
            .get_all(name)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect::<Vec<_>>()
            .join(", ");
        result.push((lower, joined));
    }
    result
}

fn map_error(error: ureq::Error) -> HttpErrorKind {
    match error {
        ureq::Error::Timeout(_) => HttpErrorKind::Timeout,
        ureq::Error::BodyExceedsLimit(_) => HttpErrorKind::TooLarge,
        ureq::Error::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            HttpErrorKind::Denied
        }
        _ => HttpErrorKind::Failed,
    }
}

/// A `Resolver` (`ureq::unversioned::resolver`) that performs the one DNS
/// lookup a connection is allowed (section 16.9: "it never resolves again
/// between the check and the connection") and applies the "Addresses"
/// paragraph to what comes back.
///
/// **The literal-address exception is scoped to this one connection's own
/// request, port included, never to a resolved name.** When the
/// URI's own host is itself a literal address, `grant_allows` has already
/// required an `Ip` scope naming that exact address and port before
/// `send_once` was ever called (see its own doc comment), so re-checking
/// `ip_scope_allows` here with the same address and port is simply
/// confirming the connection is going where the grant that authorised it
/// said it would, not a second, independent exception. When the host is a
/// name, there is no such thing: a name's own grant (`host_scope_allows`)
/// says nothing about which address it may resolve to, so a disallowed
/// address here is refused unconditionally, with no `Ip` scope, on any
/// port, ever standing in as an excuse. That is also what closes DNS
/// rebinding between the grant check and the connection: nothing here
/// treats an address a name resolved to as pre-approved just because some
/// unrelated literal grant happens to name it.
///
/// This uses `ureq`'s `unversioned` API, which the crate does not consider
/// covered by semver (see its own module documentation): a minor version
/// bump of `ureq` could change or remove it. `Cargo.toml` therefore pins an
/// exact `ureq` version rather than a range, so a bump is a deliberate,
/// reviewed step rather than something `cargo update` could silently break.
#[derive(Debug)]
struct SafeResolver {
    scopes: Vec<HttpScope>,
    /// The one deadline `perform` computed for the whole call: a
    /// name resolution that would still be running past it is abandoned
    /// rather than left to run unbounded, since `std::net::ToSocketAddrs`
    /// has no timeout of its own.
    deadline: Instant,
}

impl ureq::unversioned::resolver::Resolver for SafeResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        _config: &ureq::config::Config,
        _timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs};

        let host = uri.host().ok_or(ureq::Error::HostNotFound)?;
        let bare_host = host.strip_prefix('[').unwrap_or(host);
        let bare_host = bare_host.strip_suffix(']').unwrap_or(bare_host);
        let https = uri.scheme_str() == Some("https");
        let port = uri.port_u16().unwrap_or(if https { 443 } else { 80 });

        let mut result = ureq::unversioned::resolver::ResolvedSocketAddrs::from_fn(|_| {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
        });

        if let Ok(ip) = IpAddr::from_str(bare_host) {
            // A literal address: the exception, if the address is one the
            // "Addresses" paragraph would otherwise refuse, belongs to this
            // exact address and port (see the doc comment above).
            if !is_disallowed_address(ip) || ip_scope_allows(&self.scopes, ip, https, port) {
                result.push(SocketAddr::new(ip, port));
            }
        } else {
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ureq::Error::Timeout(ureq::Timeout::Resolve));
            }
            // `to_socket_addrs` blocks with no timeout of its own; bounding
            // it to what is left of the call's one total-time deadline
            // means resolving it on another thread and giving up on that
            // thread (not on the underlying OS call, which has no
            // cancellation of its own either) once the deadline passes.
            let owned_host = host.to_owned();
            let (tx, rx) = std::sync::mpsc::channel();
            let _ = std::thread::spawn(move || {
                let _ = tx.send(
                    (owned_host, port)
                        .to_socket_addrs()
                        .map(|it| it.map(|s| s.ip()).collect::<Vec<_>>()),
                );
            });
            let candidates = match rx.recv_timeout(remaining) {
                Ok(Ok(addrs)) => addrs,
                Ok(Err(e)) => return Err(ureq::Error::Io(e)),
                Err(_) => return Err(ureq::Error::Timeout(ureq::Timeout::Resolve)),
            };
            for candidate in candidates {
                // No exception for a name: see the doc comment above.
                if is_disallowed_address(candidate) {
                    continue;
                }
                result.push(SocketAddr::new(candidate, port));
                // Pinned to exactly one address of the one resolution.
                break;
            }
        }
        if result.is_empty() {
            return Err(ureq::Error::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "the http capability's address safety check refused every resolved address",
            )));
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HttpErrorKind, HttpScope, Url, grant_allows, host_scope_allows, ip_scope_allows,
        is_disallowed_address, looks_like_disguised_numeric_host, parse_http_scope,
        remove_dot_segments, resolve_reference, validate_headers,
    };
    use std::net::IpAddr;
    use std::str::FromStr;

    #[test]
    fn a_url_needs_a_scheme_authority_and_no_user_information() {
        assert!(Url::parse("https://api.example.com/status").is_ok());
        assert!(
            Url::parse("ftp://example.com/").is_err(),
            "not http or https"
        );
        assert!(Url::parse("https:///path").is_err(), "no authority");
        assert!(
            Url::parse("https://user@example.com/").is_err(),
            "user information"
        );
        assert!(Url::parse("not a url").is_err());
    }

    #[test]
    fn a_url_with_no_path_defaults_to_the_root() {
        let url = Url::parse("http://example.com").unwrap();
        assert_eq!(url.path_and_query, "/");
        assert_eq!(url.port, 80);
        assert!(!url.https);
    }

    #[test]
    fn an_explicit_default_port_is_the_same_as_none() {
        let with = Url::parse("https://example.com:443/x").unwrap();
        let without = Url::parse("https://example.com/x").unwrap();
        assert_eq!(with.port, without.port);
    }

    #[test]
    fn a_bracketed_ipv6_host_is_recognised_as_an_address() {
        let url = Url::parse("http://[::1]:8080/x").unwrap();
        assert!(url.host_is_ip);
        assert_eq!(url.host, "::1");
        assert_eq!(url.port, 8080);
    }

    #[test]
    fn host_names_are_lower_cased_and_a_trailing_dot_is_removed() {
        let url = Url::parse("http://API.Example.com./x").unwrap();
        assert_eq!(url.host, "api.example.com");
    }

    /// The "Addresses" paragraph's own named examples of a disguised
    /// numeric host.
    #[test]
    fn disguised_numeric_hosts_are_refused() {
        for host in [
            "2130706433",
            "0x7f.1",
            "127.1",
            "127.0.0.1%eth0",
            // A bare hex literal with no dots at all.
            "0x7f000001",
            // A trailing dot on an otherwise-canonical dotted-decimal
            // IPv4 host is itself a non-canonical disguise of the address.
            "127.0.0.1.",
            "0x7f000001.",
        ] {
            assert!(
                looks_like_disguised_numeric_host(host),
                "{host} should be refused"
            );
        }
        for host in [
            "example.com",
            "127.0.0.1",
            "a.b.c.example.com",
            "0.0.0.0",
            // An ordinary name's own trailing dot is not a numeric disguise.
            "example.com.",
        ] {
            assert!(
                !looks_like_disguised_numeric_host(host),
                "{host} should be accepted"
            );
        }
    }

    #[test]
    fn a_disguised_numeric_host_makes_the_url_invalid() {
        assert!(Url::parse("http://2130706433/").is_err());
        assert!(Url::parse("http://127.1/").is_err());
    }

    #[test]
    fn grant_scope_values_parse_as_the_specification_shows() {
        assert_eq!(
            parse_http_scope("api.example.com").unwrap(),
            HttpScope::Host {
                name: "api.example.com".to_owned(),
                port: None
            }
        );
        assert_eq!(
            parse_http_scope("*.cdn.example.com").unwrap(),
            HttpScope::Wildcard {
                suffix: "cdn.example.com".to_owned(),
                port: None
            }
        );
        assert_eq!(
            parse_http_scope("127.0.0.1:8080").unwrap(),
            HttpScope::Ip {
                addr: IpAddr::from_str("127.0.0.1").unwrap(),
                port: Some(8080)
            }
        );
        assert_eq!(
            parse_http_scope("[::1]:8080").unwrap(),
            HttpScope::Ip {
                addr: IpAddr::from_str("::1").unwrap(),
                port: Some(8080)
            }
        );
        assert!(parse_http_scope("").is_err());
        assert!(parse_http_scope("*.").is_err());
    }

    #[test]
    fn an_exact_host_scope_matches_only_that_host() {
        let scopes = [parse_http_scope("api.example.com").unwrap()];
        assert!(host_scope_allows(&scopes, "api.example.com", true, 443));
        assert!(!host_scope_allows(&scopes, "other.example.com", true, 443));
        // No port in the scope: only the scheme's own default port matches.
        assert!(!host_scope_allows(&scopes, "api.example.com", true, 8443));
        // http's default port (80) is a different port from https's (443),
        // but both are "the scheme's own default port", so both match a
        // scope with no port of its own.
        assert!(host_scope_allows(&scopes, "api.example.com", false, 80));
        assert!(!host_scope_allows(&scopes, "api.example.com", false, 8080));
    }

    #[test]
    fn a_wildcard_scope_matches_a_proper_suffix_only() {
        let scopes = [parse_http_scope("*.example.com").unwrap()];
        assert!(host_scope_allows(&scopes, "a.example.com", true, 443));
        assert!(host_scope_allows(&scopes, "a.b.example.com", true, 443));
        assert!(
            !host_scope_allows(&scopes, "example.com", true, 443),
            "the bare domain is not matched by *.example.com"
        );
        assert!(!host_scope_allows(&scopes, "evilexample.com", true, 443));
    }

    #[test]
    fn a_host_name_never_matches_an_ip_scope_or_vice_versa() {
        let ip_scope = [parse_http_scope("127.0.0.1").unwrap()];
        assert!(!host_scope_allows(&ip_scope, "127.0.0.1", false, 80));
        let host_scope = [parse_http_scope("example.com").unwrap()];
        assert!(!ip_scope_allows(
            &host_scope,
            IpAddr::from_str("127.0.0.1").unwrap(),
            false,
            80
        ));
    }

    #[test]
    fn address_safety_refuses_loopback_private_link_local_and_multicast() {
        for addr in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "224.0.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "ff02::1",
        ] {
            let ip = IpAddr::from_str(addr).unwrap();
            assert!(is_disallowed_address(ip), "{addr} should be refused");
        }
        for addr in ["93.184.216.34", "2001:4860:4860::8888"] {
            let ip = IpAddr::from_str(addr).unwrap();
            assert!(!is_disallowed_address(ip), "{addr} should be allowed");
        }
    }

    /// The address filter used to miss the unspecified address,
    /// CGNAT, a cloud metadata address, broadcast, benchmarking,
    /// documentation and the rest of the reserved block; every one of the
    /// review's own reproduction addresses that should be refused now is.
    #[test]
    fn address_safety_covers_the_full_iana_special_purpose_registry() {
        for addr in [
            "0.0.0.0",         // 0.0.0.0/8
            "100.64.0.1",      // 100.64.0.0/10, CGNAT (also 100.100.100.200, cloud metadata)
            "100.100.100.200", // Alibaba Cloud's metadata address
            "255.255.255.255", // broadcast
            "198.18.0.1",      // 198.18.0.0/15, benchmarking
            "240.0.0.1",       // 240.0.0.0/4, reserved
            "192.0.0.1",       // 192.0.0.0/24, IETF protocol assignments
            "192.0.2.1",       // 192.0.2.0/24, documentation (TEST-NET-1)
            "198.51.100.1",    // 198.51.100.0/24, documentation (TEST-NET-2)
            "203.0.113.1",     // 203.0.113.0/24, documentation (TEST-NET-3)
            "::",              // the unspecified IPv6 address
            "fec0::1",         // fec0::/10, deprecated site-local
            "2001:db8::1",     // 2001:db8::/32, IPv6 documentation
        ] {
            let ip = IpAddr::from_str(addr).unwrap();
            assert!(is_disallowed_address(ip), "{addr} should be refused");
        }
    }

    #[test]
    fn an_ipv4_mapped_ipv6_address_is_reduced_before_matching() {
        let mapped = IpAddr::from_str("::ffff:127.0.0.1").unwrap();
        assert!(is_disallowed_address(mapped));
    }

    /// Every embedded-IPv4 form named below reaches loopback
    /// (127.0.0.1) once decoded, and each must be refused exactly as the
    /// literal address would be.
    #[test]
    fn embedded_ipv4_forms_are_reduced_before_matching() {
        for addr in [
            "::127.0.0.1",     // deprecated "IPv4-compatible" ::/96
            "64:ff9b::7f00:1", // NAT64 well-known prefix, 64:ff9b::/96
            "2002:7f00:1::",   // 6to4, 2002::/16
        ] {
            let ip = IpAddr::from_str(addr).unwrap();
            assert!(
                is_disallowed_address(ip),
                "{addr} should embed a refused IPv4 address"
            );
        }
        // A public IPv4-mapped or 6to4 address must not be refused just for
        // being wrapped: only what it embeds decides.
        let public_6to4 = IpAddr::from_str("2002:5db8:d800::").unwrap(); // embeds 93.184.216.0
        assert!(!is_disallowed_address(public_6to4));
    }

    #[test]
    fn a_literal_ip_grant_is_an_exception_to_address_safety() {
        let addr = IpAddr::from_str("127.0.0.1").unwrap();
        assert!(is_disallowed_address(addr));
        let scopes = [parse_http_scope("127.0.0.1").unwrap()];
        assert!(ip_scope_allows(&scopes, addr, false, 80));
        let url = Url::parse("http://127.0.0.1/status").unwrap();
        assert!(grant_allows(&scopes, &url));
    }

    /// The literal-address exception belongs to the scope and port
    /// that actually match this request, not to any `Ip` scope naming that
    /// address anywhere in the grant on any port.
    #[test]
    fn a_literal_ip_grant_on_a_different_port_is_not_an_exception() {
        let addr = IpAddr::from_str("127.0.0.1").unwrap();
        let scopes = [parse_http_scope("127.0.0.1:9").unwrap()];
        let url = Url::parse("http://127.0.0.1:18091/status").unwrap();
        assert!(
            !grant_allows(&scopes, &url),
            "the grant names this address only on a different port"
        );
        assert!(!ip_scope_allows(&scopes, addr, false, 18091));
    }

    #[test]
    fn grant_allows_refuses_an_address_scope_does_not_name() {
        let scopes = [parse_http_scope("10.0.0.5").unwrap()];
        let url = Url::parse("http://127.0.0.1/status").unwrap();
        assert!(!grant_allows(&scopes, &url), "different literal address");
    }

    #[test]
    fn redirects_resolve_absolute_and_relative_locations() {
        let base = Url::parse("https://example.com/a/b?x=1").unwrap();

        let absolute = resolve_reference(&base, "https://other.example.com/c").unwrap();
        assert_eq!(absolute.host, "other.example.com");
        assert_eq!(absolute.path_and_query, "/c");

        let absolute_path = resolve_reference(&base, "/c/d").unwrap();
        assert_eq!(absolute_path.host, "example.com");
        assert_eq!(absolute_path.path_and_query, "/c/d");

        let relative = resolve_reference(&base, "c").unwrap();
        assert_eq!(relative.path_and_query, "/a/c");

        let scheme_relative = resolve_reference(&base, "//cdn.example.com/x").unwrap();
        assert_eq!(scheme_relative.host, "cdn.example.com");
        assert!(scheme_relative.https, "the base scheme is kept");

        let with_query = resolve_reference(&base, "/c?y=2").unwrap();
        assert_eq!(with_query.path_and_query, "/c?y=2");
    }

    #[test]
    fn an_unparseable_or_non_http_redirect_target_is_refused() {
        let base = Url::parse("https://example.com/a/b").unwrap();
        assert!(resolve_reference(&base, "").is_err());
        assert!(resolve_reference(&base, "javascript:alert(1)").is_err());
        assert!(resolve_reference(&base, "mailto:a@example.com").is_err());
        assert!(resolve_reference(&base, "#fragment").is_err());
    }

    #[test]
    fn remove_dot_segments_matches_rfc_3986s_own_examples() {
        assert_eq!(remove_dot_segments("/a/b/c/./../../g"), "/a/g");
        assert_eq!(remove_dot_segments("mid/content=5/../6"), "/mid/6");
    }

    /// A program cannot set the `Host` header, or set headers that could be
    /// used to smuggle a second request: `Host`,
    /// `Content-Length`, `Transfer-Encoding` and `Connection` are refused in
    /// a program's own headers, compared case-insensitively.
    #[test]
    fn program_headers_cannot_set_host_or_framing_headers() {
        for name in [
            "Host",
            "host",
            "HOST",
            "Content-Length",
            "content-length",
            "Transfer-Encoding",
            "Connection",
            "Upgrade",
            "te",
            "Expect",
            "Keep-Alive",
            "Proxy-Authorization",
            "X-Forwarded-Host",
        ] {
            assert_eq!(
                validate_headers(&[(name.to_owned(), "x".to_owned())]),
                Err(HttpErrorKind::Invalid),
                "{name} should be refused"
            );
        }
        assert_eq!(
            validate_headers(&[("X-Custom".to_owned(), "x".to_owned())]),
            Ok(())
        );
    }

    /// The ranges the IANA special-purpose registries mark as not globally
    /// reachable that an earlier version let through, each as written and, where it embeds
    /// an IPv4 address, with that address as well.
    #[test]
    fn address_safety_covers_the_ranges_that_were_missing() {
        for text in [
            // IPv4-translated, with a loopback address embedded.
            "::ffff:0:7f00:1",
            "::ffff:0:0:0",
            // Deprecated 6to4 relay anycast.
            "192.88.99.0",
            "192.88.99.255",
            // Documentation (RFC 9637).
            "3fff::",
            "3fff:fff:ffff::1",
            // Benchmarking.
            "2001:2::1",
            "2001:2:0:ffff::1",
            // ORCHID and ORCHIDv2.
            "2001:10::1",
            "2001:1f::1",
            "2001:20::1",
            // Segment routing SIDs.
            "5f00::1",
            "5f00:ffff::1",
        ] {
            let addr = IpAddr::from_str(text).unwrap();
            assert!(is_disallowed_address(addr), "{text} must be refused");
        }
        // Neighbours of those ranges are ordinary global addresses.
        for text in [
            "192.88.98.1",
            "192.88.100.1",
            "3ffe::1",
            "2001:3::1",
            "2001:2:1::1",
            "2001:30::1",
            "5f01::1",
            "2606:4700::1111",
            "1.1.1.1",
        ] {
            let addr = IpAddr::from_str(text).unwrap();
            assert!(!is_disallowed_address(addr), "{text} must be allowed");
        }
    }
}

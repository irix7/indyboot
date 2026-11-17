// SPDX-License-Identifier: GPL-3.0-only
//! `http://` URLs: split into host, port and path, and resolve references.

use heapless::String;

/// Longest URL indyboot stores.
pub const URL_MAX: usize = 200;

/// A URL held in a fixed buffer.
pub type UrlBuf = String<URL_MAX>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlError {
    /// The scheme is `https`. indyboot has no TLS.
    Https,
    /// The scheme is something other than `http`.
    Scheme,
    /// The host is empty or the port is not a number from 1 to 65535.
    Authority,
    /// The result does not fit in [`URL_MAX`] bytes.
    TooLong,
}

impl UrlError {
    pub fn message(self) -> &'static str {
        match self {
            UrlError::Https => "https is not supported; publish over plain http",
            UrlError::Scheme => "URL must start with http://",
            UrlError::Authority => "bad host or port in URL",
            UrlError::TooLong => "URL is longer than 200 bytes",
        }
    }
}

/// The parts of an absolute `http://` URL. `path` starts with `/` and keeps
/// the query string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Url<'a> {
    pub host: &'a str,
    pub port: u16,
    pub path: &'a str,
}

impl<'a> Url<'a> {
    pub fn parse(text: &'a str) -> Result<Self, UrlError> {
        let rest = if let Some(rest) = strip_prefix_ignore_case(text, "http://") {
            rest
        } else if strip_prefix_ignore_case(text, "https://").is_some() {
            return Err(UrlError::Https);
        } else {
            return Err(UrlError::Scheme);
        };
        let (authority, path) = match rest.find(['/', '?']) {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        let (host, port) = match authority.rfind(':') {
            Some(i) => {
                let port: u16 = authority[i + 1..]
                    .parse()
                    .map_err(|_| UrlError::Authority)?;
                (&authority[..i], port)
            }
            None => (authority, 80),
        };
        if host.is_empty() || port == 0 || host.contains(['@', '[', ' ']) {
            return Err(UrlError::Authority);
        }
        // "http://host?q" has a query and no path; the request line still
        // needs a leading slash, which `request_path` adds.
        Ok(Url { host, port, path })
    }

    /// The host as dotted-quad octets, when it is an IPv4 literal.
    pub fn ipv4(&self) -> Option<[u8; 4]> {
        parse_ipv4(self.host)
    }
}

/// Parse `a.b.c.d`.
pub fn parse_ipv4(text: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut parts = text.split('.');
    for slot in &mut out {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(out)
}

/// Resolve `reference` against the absolute URL `base`.
///
/// Three forms are accepted: an absolute URL, a path starting with `/` (same
/// host), and a relative name (replaces the last path segment of `base`).
/// `..` segments are not interpreted.
pub fn resolve(base: &str, reference: &str) -> Result<UrlBuf, UrlError> {
    let mut out = UrlBuf::new();
    if reference.contains("://") {
        Url::parse(reference)?;
        out.push_str(reference).map_err(|_| UrlError::TooLong)?;
        return Ok(out);
    }
    let parsed = Url::parse(base)?;
    // Length of "http://authority".
    let origin_len = base.len() - parsed.path.len();
    let origin = &base[..origin_len];
    out.push_str(origin).map_err(|_| UrlError::TooLong)?;
    if reference.starts_with('/') {
        out.push_str(reference).map_err(|_| UrlError::TooLong)?;
        return Ok(out);
    }
    let path = match parsed.path.find('?') {
        Some(i) => &parsed.path[..i],
        None => parsed.path,
    };
    let dir = match path.rfind('/') {
        Some(i) => &path[..=i],
        None => "/",
    };
    out.push_str(dir).map_err(|_| UrlError::TooLong)?;
    out.push_str(reference).map_err(|_| UrlError::TooLong)?;
    Ok(out)
}

fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    if text.len() >= prefix.len()
        && text.is_char_boundary(prefix.len())
        && text[..prefix.len()].eq_ignore_ascii_case(prefix)
    {
        Some(&text[prefix.len()..])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_host_port_and_path() {
        let url = Url::parse("http://boot.example.org:8080/indy/boot.cfg?x=1").unwrap();
        assert_eq!(url.host, "boot.example.org");
        assert_eq!(url.port, 8080);
        assert_eq!(url.path, "/indy/boot.cfg?x=1");
    }

    #[test]
    fn defaults_port_and_allows_missing_path() {
        let url = Url::parse("HTTP://192.168.0.1").unwrap();
        assert_eq!(url.host, "192.168.0.1");
        assert_eq!(url.port, 80);
        assert_eq!(url.path, "");
        assert_eq!(url.ipv4(), Some([192, 168, 0, 1]));
    }

    #[test]
    fn rejects_other_schemes_and_bad_authorities() {
        assert_eq!(Url::parse("https://example.org/x"), Err(UrlError::Https));
        assert_eq!(Url::parse("tftp://example.org/x"), Err(UrlError::Scheme));
        assert_eq!(Url::parse("http:///x"), Err(UrlError::Authority));
        assert_eq!(Url::parse("http://host:0/x"), Err(UrlError::Authority));
        assert_eq!(Url::parse("http://host:99999/x"), Err(UrlError::Authority));
        assert_eq!(Url::parse("http://u@host/x"), Err(UrlError::Authority));
    }

    #[test]
    fn ipv4_literals() {
        assert_eq!(parse_ipv4("10.1.1.112"), Some([10, 1, 1, 112]));
        assert_eq!(parse_ipv4("10.1.1"), None);
        assert_eq!(parse_ipv4("10.1.1.256"), None);
        assert_eq!(parse_ipv4("example.org"), None);
        assert_eq!(parse_ipv4("1.2.3.4.5"), None);
    }

    #[test]
    fn resolves_the_three_reference_forms() {
        let base = "http://h.example:81/a/b/boot.cfg?v=2";
        assert_eq!(
            resolve(base, "http://other/x").unwrap().as_str(),
            "http://other/x"
        );
        assert_eq!(
            resolve(base, "/img/irix7").unwrap().as_str(),
            "http://h.example:81/img/irix7"
        );
        assert_eq!(
            resolve(base, "irix7").unwrap().as_str(),
            "http://h.example:81/a/b/irix7"
        );
        assert_eq!(
            resolve("http://h", "irix7").unwrap().as_str(),
            "http://h/irix7"
        );
        assert_eq!(resolve(base, "https://other/x"), Err(UrlError::Https));
    }

    #[test]
    fn resolve_reports_overflow() {
        let long = "x".repeat(URL_MAX);
        assert_eq!(resolve("http://h/", &long), Err(UrlError::TooLong));
    }
}

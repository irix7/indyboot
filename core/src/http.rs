// SPDX-License-Identifier: GPL-3.0-only
//! HTTP/1.0 request text and response-head parsing.
//!
//! indyboot sends HTTP/1.0 with a `Host` header and `Connection: close`. A
//! server answers that with an unchunked body that ends at `Content-Length`
//! or at the close of the connection, so no chunked decoder is needed.

use crate::url::Url;
use heapless::String;

/// Largest response head (status line and headers) indyboot accepts.
pub const HEAD_MAX: usize = 2048;

/// Largest request indyboot builds.
pub const REQUEST_MAX: usize = 512;

/// Build the GET request for `url`.
pub fn request(url: &Url<'_>) -> Option<String<REQUEST_MAX>> {
    let mut out = String::new();
    out.push_str("GET ").ok()?;
    if !url.path.starts_with('/') {
        out.push('/').ok()?;
    }
    out.push_str(url.path).ok()?;
    out.push_str(" HTTP/1.0\r\nHost: ").ok()?;
    out.push_str(url.host).ok()?;
    if url.port != 80 {
        out.push(':').ok()?;
        push_dec(&mut out, url.port as u32)?;
    }
    out.push_str("\r\nUser-Agent: indyboot/").ok()?;
    out.push_str(env!("CARGO_PKG_VERSION")).ok()?;
    out.push_str("\r\nAccept: */*\r\nConnection: close\r\n\r\n")
        .ok()?;
    Some(out)
}

fn push_dec<const N: usize>(out: &mut String<N>, mut value: u32) -> Option<()> {
    let mut digits = [0u8; 10];
    let mut count = 0;
    loop {
        digits[count] = b'0' + (value % 10) as u8;
        count += 1;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    while count > 0 {
        count -= 1;
        out.push(digits[count] as char).ok()?;
    }
    Some(())
}

/// The parts of a response head that indyboot uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Head<'a> {
    pub status: u16,
    pub content_length: Option<u32>,
    /// `Location` header, for a 3xx status.
    pub location: Option<&'a str>,
    /// Bytes of the buffer the head occupies, including the blank line. The
    /// body starts here.
    pub len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadError {
    /// The blank line that ends the head has not arrived yet.
    Incomplete,
    /// The bytes are not an HTTP response head.
    Malformed,
}

/// Parse a response head from the start of `buf`.
pub fn parse_head(buf: &[u8]) -> Result<Head<'_>, HeadError> {
    let end = find(buf, b"\r\n\r\n").ok_or(HeadError::Incomplete)?;
    let head = core::str::from_utf8(&buf[..end]).map_err(|_| HeadError::Malformed)?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().ok_or(HeadError::Malformed)?;
    let mut words = status_line.split(' ');
    let version = words.next().ok_or(HeadError::Malformed)?;
    if !version.starts_with("HTTP/1.") {
        return Err(HeadError::Malformed);
    }
    let code = words.next().ok_or(HeadError::Malformed)?;
    if code.len() != 3 {
        return Err(HeadError::Malformed);
    }
    let status: u16 = code.parse().map_err(|_| HeadError::Malformed)?;

    let mut content_length = None;
    let mut location = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = Some(value.parse().map_err(|_| HeadError::Malformed)?);
        } else if name.eq_ignore_ascii_case("location") {
            location = Some(value);
        }
    }
    Ok(Head {
        status,
        content_length,
        location,
        len: end + 4,
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_has_host_and_closes() {
        let url = Url::parse("http://boot.example.org:8080/indy/boot.cfg").unwrap();
        let text = request(&url).unwrap();
        assert!(text.starts_with("GET /indy/boot.cfg HTTP/1.0\r\nHost: boot.example.org:8080\r\n"));
        assert!(text.ends_with("Connection: close\r\n\r\n"));
    }

    #[test]
    fn request_adds_slash_and_omits_default_port() {
        let url = Url::parse("http://h?x=1").unwrap();
        let text = request(&url).unwrap();
        assert!(text.starts_with("GET /?x=1 HTTP/1.0\r\nHost: h\r\n"));
    }

    #[test]
    fn head_with_length() {
        let raw = b"HTTP/1.1 200 OK\r\nServer: x\r\ncontent-length: 344\r\n\r\nBODY";
        let head = parse_head(raw).unwrap();
        assert_eq!(head.status, 200);
        assert_eq!(head.content_length, Some(344));
        assert_eq!(head.location, None);
        assert_eq!(&raw[head.len..], b"BODY");
    }

    #[test]
    fn head_redirect() {
        let raw = b"HTTP/1.0 302 Found\r\nLocation:  http://mirror/x \r\n\r\n";
        let head = parse_head(raw).unwrap();
        assert_eq!(head.status, 302);
        assert_eq!(head.location, Some("http://mirror/x"));
        assert_eq!(head.content_length, None);
    }

    #[test]
    fn head_incomplete_and_malformed() {
        assert_eq!(
            parse_head(b"HTTP/1.1 200 OK\r\nServer: x\r\n"),
            Err(HeadError::Incomplete)
        );
        assert_eq!(
            parse_head(b"SSH-2.0-OpenSSH\r\n\r\n"),
            Err(HeadError::Malformed)
        );
        assert_eq!(
            parse_head(b"HTTP/1.1 20 OK\r\n\r\n"),
            Err(HeadError::Malformed)
        );
        assert_eq!(
            parse_head(b"HTTP/1.1 200 OK\r\nContent-Length: lots\r\n\r\n"),
            Err(HeadError::Malformed)
        );
    }
}

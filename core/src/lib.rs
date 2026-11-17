// SPDX-License-Identifier: GPL-3.0-only
//! Parsing and loading logic for indyboot.
//!
//! Everything here is `no_std` and has no hardware access, so `cargo test`
//! runs it on the build host. The `loader` crate supplies the ARCS calls, the
//! Ethernet driver and the TCP/IP stack.

#![cfg_attr(not(test), no_std)]

pub mod bootp;
pub mod ecoff;
pub mod http;
pub mod manifest;
pub mod url;

pub use sha2;

/// Decode 64 hexadecimal digits into a SHA-256 digest.
pub fn parse_sha256(text: &str) -> Option<[u8; 32]> {
    let bytes = text.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(bytes.as_chunks::<2>().0) {
        *slot = (hex_digit(pair[0])? << 4) | hex_digit(pair[1])?;
    }
    Some(out)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_round_trip() {
        let hex = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let digest = parse_sha256(hex).unwrap();
        assert_eq!(digest[0], 0xe3);
        assert_eq!(digest[31], 0x55);
        assert!(parse_sha256(&hex[1..]).is_none());
        assert!(parse_sha256(&hex.replace('e', "g")).is_none());
    }
}

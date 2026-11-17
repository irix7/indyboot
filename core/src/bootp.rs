// SPDX-License-Identifier: GPL-3.0-only
//! BOOTP (RFC 951) request and reply frames, with RFC 1497 vendor options.
//!
//! The PROM that loaded indyboot found its server with plain BOOTP, so a
//! BOOTP server is known to exist on the segment. Some of them ignore DHCP
//! from the Indy's address, which is why indyboot can speak BOOTP itself.

/// Length of the request frame: Ethernet, IPv4, UDP and a 300-byte BOOTP body.
pub const REQUEST_LEN: usize = ETH + IP + UDP + BOOTP;

const ETH: usize = 14;
const IP: usize = 20;
const UDP: usize = 8;
const BOOTP: usize = 300;
/// Offset of the vendor area in the BOOTP body.
const VEND: usize = 236;
const COOKIE: [u8; 4] = [99, 130, 83, 99];

/// What a server's reply configures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reply {
    pub address: [u8; 4],
    /// Prefix length from the subnet-mask option, or the address class.
    pub prefix: u8,
    pub router: Option<[u8; 4]>,
    pub dns: Option<[u8; 4]>,
}

/// Fill `frame` with a broadcast BOOTREQUEST from `mac`.
pub fn build_request(frame: &mut [u8; REQUEST_LEN], mac: [u8; 6], xid: u32) {
    frame.fill(0);
    // Ethernet: broadcast, IPv4.
    frame[0..6].fill(0xFF);
    frame[6..12].copy_from_slice(&mac);
    frame[12..14].copy_from_slice(&[0x08, 0x00]);
    // IPv4: 0.0.0.0 to 255.255.255.255, UDP, no options. The Don't Fragment
    // bit stays clear.
    let ip = &mut frame[ETH..ETH + IP];
    ip[0] = 0x45;
    ip[2..4].copy_from_slice(&((IP + UDP + BOOTP) as u16).to_be_bytes());
    ip[8] = 64;
    ip[9] = 17;
    ip[16..20].fill(0xFF);
    let sum = ip_checksum(ip);
    ip[10..12].copy_from_slice(&sum.to_be_bytes());
    // UDP: port 68 to 67. A zero checksum means "not computed".
    let udp = &mut frame[ETH + IP..ETH + IP + UDP];
    udp[0..2].copy_from_slice(&68u16.to_be_bytes());
    udp[2..4].copy_from_slice(&67u16.to_be_bytes());
    udp[4..6].copy_from_slice(&((UDP + BOOTP) as u16).to_be_bytes());
    // BOOTP body.
    let body = &mut frame[ETH + IP + UDP..];
    body[0] = 1; // BOOTREQUEST
    body[1] = 1; // Ethernet
    body[2] = 6; // hardware address length
    body[4..8].copy_from_slice(&xid.to_be_bytes());
    body[10] = 0x80; // ask for a broadcast reply
    body[28..34].copy_from_slice(&mac);
    body[VEND..VEND + 4].copy_from_slice(&COOKIE);
    body[VEND + 4] = 255;
}

fn ip_checksum(header: &[u8]) -> u16 {
    let mut sum = 0u32;
    for pair in header.as_chunks::<2>().0 {
        sum += u16::from_be_bytes([pair[0], pair[1]]) as u32;
    }
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Read a BOOTREPLY for `mac` and `xid` from a received Ethernet frame.
///
/// Returns `None` for any other frame, including DHCP messages.
pub fn parse_reply(frame: &[u8], mac: [u8; 6], xid: u32) -> Option<Reply> {
    if frame.len() < ETH + IP + UDP + VEND || frame[12..14] != [0x08, 0x00] {
        return None;
    }
    let ip = &frame[ETH..];
    let header_len = ((ip[0] & 0x0F) as usize) * 4;
    if ip[0] >> 4 != 4 || header_len < IP || ip[9] != 17 {
        return None;
    }
    let udp = ip.get(header_len..)?;
    if udp.len() < UDP + VEND
        || udp[0..2] != 67u16.to_be_bytes()
        || udp[2..4] != 68u16.to_be_bytes()
    {
        return None;
    }
    let body = &udp[UDP..];
    if body[0] != 2 || body[4..8] != xid.to_be_bytes() || body[28..34] != mac {
        return None;
    }
    let address: [u8; 4] = body[16..20].try_into().ok()?;
    if address == [0; 4] {
        return None;
    }
    let mut reply = Reply {
        address,
        prefix: class_prefix(address),
        router: None,
        dns: None,
    };
    let vend = &body[VEND..];
    if vend.len() >= 4 && vend[..4] == COOKIE {
        let mut options = &vend[4..];
        while let Some((&tag, rest)) = options.split_first() {
            match tag {
                0 => {
                    options = rest;
                    continue;
                }
                255 => break,
                _ => {}
            }
            let (&len, rest) = rest.split_first()?;
            let value = rest.get(..len as usize)?;
            options = &rest[len as usize..];
            let first4 = || -> Option<[u8; 4]> { value.get(..4)?.try_into().ok() };
            match tag {
                1 => {
                    if let Some(mask) = first4() {
                        reply.prefix = u32::from_be_bytes(mask).leading_ones() as u8;
                    }
                }
                3 => reply.router = first4(),
                6 => reply.dns = first4(),
                // A DHCP message type: this is not a BOOTP reply.
                53 => return None,
                _ => {}
            }
        }
    }
    Some(reply)
}

fn class_prefix(address: [u8; 4]) -> u8 {
    match address[0] {
        0..=127 => 8,
        128..=191 => 16,
        _ => 24,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: [u8; 6] = [0x08, 0x00, 0x69, 0x12, 0x34, 0x56];
    const XID: u32 = 0xCAFE_F00D;

    fn reply_frame(options: &[u8]) -> std::vec::Vec<u8> {
        let mut request = [0u8; REQUEST_LEN];
        build_request(&mut request, MAC, XID);
        let mut frame = request.to_vec();
        frame[ETH + IP..ETH + IP + 2].copy_from_slice(&67u16.to_be_bytes());
        frame[ETH + IP + 2..ETH + IP + 4].copy_from_slice(&68u16.to_be_bytes());
        let body = ETH + IP + UDP;
        frame[body] = 2;
        frame[body + 16..body + 20].copy_from_slice(&[192, 168, 0, 2]);
        frame[body + VEND + 4..body + VEND + 4 + options.len()].copy_from_slice(options);
        frame
    }

    #[test]
    fn request_layout() {
        let mut frame = [0u8; REQUEST_LEN];
        build_request(&mut frame, MAC, XID);
        assert_eq!(frame.len(), 342);
        assert_eq!(&frame[0..6], &[0xFF; 6]);
        assert_eq!(&frame[6..12], &MAC);
        // The header checksum verifies: summing the header gives 0xFFFF.
        assert_eq!(ip_checksum(&frame[ETH..ETH + IP]), 0);
        // Don't Fragment is clear.
        assert_eq!(frame[ETH + 6] & 0x40, 0);
        let body = &frame[ETH + IP + UDP..];
        assert_eq!(body[0], 1);
        assert_eq!(&body[4..8], &XID.to_be_bytes());
        assert_eq!(&body[28..34], &MAC);
        assert_eq!(&body[VEND..VEND + 5], &[99, 130, 83, 99, 255]);
    }

    #[test]
    fn reply_with_options() {
        let frame = reply_frame(&[
            1, 4, 255, 255, 255, 0, // mask
            0, // pad
            3, 4, 192, 168, 0, 1, // router
            6, 8, 8, 8, 8, 8, 1, 1, 1, 1, // two DNS servers
            255,
        ]);
        assert_eq!(
            parse_reply(&frame, MAC, XID),
            Some(Reply {
                address: [192, 168, 0, 2],
                prefix: 24,
                router: Some([192, 168, 0, 1]),
                dns: Some([8, 8, 8, 8]),
            })
        );
    }

    #[test]
    fn reply_without_options_uses_the_address_class() {
        let mut frame = reply_frame(&[255]);
        let body = ETH + IP + UDP;
        frame[body + 16..body + 20].copy_from_slice(&[10, 1, 1, 112]);
        let reply = parse_reply(&frame, MAC, XID).unwrap();
        assert_eq!(reply.prefix, 8);
        assert_eq!(reply.router, None);
        assert_eq!(reply.dns, None);
    }

    #[test]
    fn other_frames_are_ignored() {
        let good = reply_frame(&[255]);
        assert!(parse_reply(&good, MAC, XID).is_some());
        assert!(parse_reply(&good, MAC, XID + 1).is_none());
        assert!(parse_reply(&good, [2, 0, 0, 0, 0, 1], XID).is_none());
        assert!(parse_reply(&good[..100], MAC, XID).is_none());
        // Our own request (op 1) is not a reply.
        let mut request = [0u8; REQUEST_LEN];
        build_request(&mut request, MAC, XID);
        assert!(parse_reply(&request, MAC, XID).is_none());
        // A DHCP offer carries option 53.
        let offer = reply_frame(&[53, 1, 2, 255]);
        assert!(parse_reply(&offer, MAC, XID).is_none());
        // A truncated option list does not panic.
        let cut = reply_frame(&[3, 200, 1, 2]);
        let _ = parse_reply(&cut, MAC, XID);
    }
}

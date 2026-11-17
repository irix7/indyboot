// SPDX-License-Identifier: GPL-3.0-only
//! Settings from the boot command line and the PROM environment.
//!
//! ```text
//! boot -f bootp()indyboot url=http://10.1.1.3/boot.cfg ip=10.1.1.112/24 gw=10.1.1.1 dns=10.1.1.1
//! ```
//!
//! | Word | Meaning | Default |
//! |---|---|---|
//! | `url=URL` | Manifest to fetch | PROM variable `indyboot_url`, then the URL compiled in |
//! | `ip=bootp` | Use BOOTP only | BOOTP, then DHCP if BOOTP gave no router or DNS |
//! | `ip=dhcp` | Use DHCP only | |
//! | `ip=A.B.C.D/N` | Use this address and prefix length | |
//! | `gw=A.B.C.D` | Default router; overrides BOOTP and DHCP | |
//! | `dns=A.B.C.D` | DNS server; overrides BOOTP and DHCP | |

use crate::arcs;
use core::ffi::c_char;
use core::net::Ipv4Addr;
use indyboot_core::url::parse_ipv4;

#[derive(Clone, Copy)]
pub enum Address {
    Auto,
    Bootp,
    Dhcp,
    Static { address: Ipv4Addr, prefix: u8 },
}

pub struct Options {
    pub url: Option<&'static str>,
    pub address: Address,
    pub router: Option<Ipv4Addr>,
    pub dns: Option<Ipv4Addr>,
}

impl Options {
    pub fn read(argc: i32, argv: *const *const c_char, builtin_url: Option<&'static str>) -> Self {
        let mut options = Options {
            url: arcs::environment(b"indyboot_url\0")
                .filter(|url| !url.is_empty())
                .or(builtin_url),
            address: Address::Auto,
            router: None,
            dns: None,
        };
        if argv.is_null() {
            return options;
        }
        for i in 0..argc.clamp(0, 64) as usize {
            // SAFETY: the PROM passes `argc` NUL-terminated strings that it
            // keeps for the life of the program.
            let Some(word) = (unsafe { arcs::c_str(*argv.add(i)) }) else {
                continue;
            };
            if let Some(value) = word.strip_prefix("url=") {
                options.url = Some(value);
            } else if let Some(value) = word.strip_prefix("gw=") {
                options.router = parse_ipv4(value).map(Ipv4Addr::from).or(options.router);
            } else if let Some(value) = word.strip_prefix("dns=") {
                options.dns = parse_ipv4(value).map(Ipv4Addr::from).or(options.dns);
            } else if let Some(value) = word.strip_prefix("ip=") {
                match value {
                    "bootp" => options.address = Address::Bootp,
                    "dhcp" => options.address = Address::Dhcp,
                    _ => match parse_cidr(value) {
                        Some((address, prefix)) => {
                            options.address = Address::Static { address, prefix }
                        }
                        None => crate::println!(
                            "Ignoring {}: expected ip=bootp, ip=dhcp or ip=A.B.C.D/N",
                            word
                        ),
                    },
                }
            }
        }
        options
    }
}

fn parse_cidr(text: &str) -> Option<(Ipv4Addr, u8)> {
    let (address, prefix) = text.split_once('/')?;
    let prefix: u8 = prefix.parse().ok()?;
    (prefix <= 32).then_some(())?;
    Some((Ipv4Addr::from(parse_ipv4(address)?), prefix))
}

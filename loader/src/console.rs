// SPDX-License-Identifier: GPL-3.0-only
//! Text console on the PROM's standard output and input.
//!
//! Output goes through ARCS `Write`, so it appears wherever the PROM's own
//! text does: the graphics textport, or serial port 1 when `console=d`.

use crate::arcs;
use core::fmt;

/// `core::fmt` sink that sends `\n` as CR LF.
pub struct Console;

impl fmt::Write for Console {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let mut buffer = [0u8; 96];
        let mut used = 0;
        for &byte in text.as_bytes() {
            if used + 2 > buffer.len() {
                arcs::write(&buffer[..used]);
                used = 0;
            }
            if byte == b'\n' {
                buffer[used] = b'\r';
                used += 1;
            }
            buffer[used] = byte;
            used += 1;
        }
        arcs::write(&buffer[..used]);
        Ok(())
    }
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        let _ = write!($crate::console::Console, $($arg)*);
    }};
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => {{
        $crate::print!($($arg)*);
        $crate::print!("\n");
    }};
}

/// Read a line with echo and backspace. Returns the bytes typed before Enter.
pub fn read_line(buffer: &mut [u8]) -> &str {
    let mut len = 0;
    loop {
        let Some(byte) = arcs::read_byte() else {
            continue;
        };
        match byte {
            b'\r' | b'\n' => {
                arcs::write(b"\r\n");
                break;
            }
            0x08 | 0x7f => {
                if len > 0 {
                    len -= 1;
                    arcs::write(b"\x08 \x08");
                }
            }
            0x20..=0x7e if len < buffer.len() => {
                buffer[len] = byte;
                len += 1;
                arcs::write(&[byte]);
            }
            _ => {}
        }
    }
    core::str::from_utf8(&buffer[..len]).unwrap_or("")
}

/// Wait for one key.
pub fn wait_key() {
    while arcs::read_byte().is_none() {}
}

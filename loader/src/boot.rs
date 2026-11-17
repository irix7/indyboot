// SPDX-License-Identifier: GPL-3.0-only
//! Download an image into place, verify it and start it.
//!
//! The handoff matches what the PROM does for `boot -f bootp()NAME`: the
//! ECOFF sections are at their addresses, `.bss` is zero, the caches are
//! flushed, `$sp` is the PROM's stack, `$a0`..`$a2` hold argc, argv and envp,
//! and the ARCS firmware vector is still usable. CP0 Status is untouched.

use crate::mem::{ImageMemory, Map};
use crate::net::{FetchError, Net};
use crate::{arcs, print, println};
use core::ffi::c_char;
use indyboot_core::ecoff::{EcoffError, Loader};
use indyboot_core::manifest::{Entry, ARGS_MAX};
use indyboot_core::sha2::{Digest, Sha256};
use indyboot_core::url::URL_MAX;

extern "C" {
    fn indyboot_handoff(
        entry: u32,
        argc: i32,
        argv: *const *const c_char,
        envp: *const *const c_char,
    ) -> !;
}

/// Most words of `args` passed on. Later words are dropped.
const MAX_ARGS: usize = 15;

static mut ARG_TEXT: [u8; URL_MAX + ARGS_MAX + 2] = [0; URL_MAX + ARGS_MAX + 2];
static mut ARGV: [*const c_char; MAX_ARGS + 2] = [core::ptr::null(); MAX_ARGS + 2];

/// Print a dot for each of these many bytes received.
const DOT_BYTES: u32 = 128 * 1024;

/// Load and start `entry`. Returns only when the image could not be started;
/// the reason has been printed.
pub fn boot(net: &mut Net, map: &Map, entry: &Entry, envp: *const *const c_char) -> Result<(), ()> {
    println!("Loading {}", entry.title);
    println!("  {}", entry.image);

    let mut loader = Loader::new();
    let mut memory = ImageMemory { map };
    let mut hasher = Sha256::new();
    let mut rejected: Option<EcoffError> = None;
    let mut received = 0u32;
    print!("  ");
    let result = net.get(&entry.image, &mut |data| {
        hasher.update(data);
        if let Err(error) = loader.feed(data, &mut memory) {
            rejected = Some(error);
            return Err(FetchError::Rejected);
        }
        let before = received / DOT_BYTES;
        received += data.len() as u32;
        if received / DOT_BYTES != before {
            print!(".");
        }
        Ok(())
    });
    println!();

    if let Some(error) = rejected {
        match error {
            EcoffError::Range(vaddr, size) => println!(
                "  The image needs {:#010x}..{:#010x}, which is not free memory. Type i at the menu for the map.",
                vaddr,
                vaddr.wrapping_add(size)
            ),
            EcoffError::Entry(address) => println!("  {}: {:#010x}", error.message(), address),
            _ => println!("  {}", error.message()),
        }
        return Err(());
    }
    let bytes = match result {
        Ok(bytes) => bytes,
        Err(error) => {
            print!("  Download failed: ");
            crate::fetch_error(error);
            return Err(());
        }
    };
    if let Some(size) = entry.size {
        if size != bytes {
            println!(
                "  Received {} bytes; the manifest says size = {}.",
                bytes, size
            );
            return Err(());
        }
    }
    let start = match loader.finish() {
        Ok(start) => start,
        Err(error) => {
            println!("  {}", error.message());
            return Err(());
        }
    };
    let digest = hasher.finalize();
    if digest.as_slice() != entry.sha256 {
        print!("  SHA-256 mismatch. Received ");
        for byte in digest.iter() {
            print!("{:02x}", byte);
        }
        println!();
        return Err(());
    }
    let (low, high) = loader.plan().map(|plan| plan.extent()).unwrap_or((0, 0));
    println!(
        "  {} bytes, SHA-256 verified, loaded at {:#010x}..{:#010x}, entry {:#010x}",
        bytes, low, high, start
    );

    let (argc, argv) = build_argv(entry);
    net.stop();
    arcs::flush_all_caches();
    // SAFETY: the image is in place and verified; this does not return.
    unsafe { indyboot_handoff(start, argc, argv, envp) }
}

/// Build `argv`: the image URL, then the words of the entry's `args`.
fn build_argv(entry: &Entry) -> (i32, *const *const c_char) {
    // SAFETY: single-threaded, and nothing else borrows these statics.
    let (text, argv) = unsafe {
        (
            &mut *core::ptr::addr_of_mut!(ARG_TEXT),
            &mut *core::ptr::addr_of_mut!(ARGV),
        )
    };
    let mut used = 0;
    let mut argc = 0;
    let words = core::iter::once(entry.image.as_str()).chain(entry.args.split_ascii_whitespace());
    for word in words.take(MAX_ARGS + 1) {
        argv[argc] = text[used..].as_ptr() as *const c_char;
        text[used..used + word.len()].copy_from_slice(word.as_bytes());
        text[used + word.len()] = 0;
        used += word.len() + 1;
        argc += 1;
    }
    argv[argc] = core::ptr::null();
    (argc as i32, argv.as_ptr())
}

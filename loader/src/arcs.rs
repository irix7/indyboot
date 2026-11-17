// SPDX-License-Identifier: GPL-3.0-only
//! Calls into the ARCS firmware the PROM leaves in memory.
//!
//! The system parameter block is at kseg1 0xA0001000. Word 8 points at the
//! firmware vector, a table of 32-bit function addresses. The PROM is o32
//! code and so is indyboot, so each entry is called as a plain C function.
//! Interrupts stay off, as the PROM left them: ARCS is not re-entrant.

use core::ffi::c_char;

const SPB: usize = 0xA000_1000;
/// "ARCS", read as a big-endian word.
const SPB_SIGNATURE: u32 = 0x5343_5241;
/// Byte offset of the firmware vector pointer in the parameter block.
const SPB_FIRMWARE_VECTOR: usize = 32;

/// Firmware vector slots.
const ENTER_INTERACTIVE_MODE: usize = 7;
const GET_MEMORY_DESCRIPTOR: usize = 18;
const GET_RELATIVE_TIME: usize = 21;
const READ: usize = 25;
const GET_READ_STATUS: usize = 26;
const WRITE: usize = 27;
const GET_ENVIRONMENT_VARIABLE: usize = 30;
const FLUSH_ALL_CACHES: usize = 34;

/// File identifiers the PROM opens before it starts a program.
const STDIN: u32 = 0;
const STDOUT: u32 = 1;

/// One range of physical memory, in 4 KiB pages.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MemoryDescriptor {
    pub kind: u32,
    pub base_page: u32,
    pub page_count: u32,
}

/// Memory types on SGI firmware that a program may overwrite.
pub const FREE_CONTIGUOUS: u32 = 2;
pub const FREE_MEMORY: u32 = 3;

/// Whether the parameter block carries the ARCS signature.
pub fn present() -> bool {
    // SAFETY: the page at physical 0x1000 is RAM on every Indy.
    unsafe { core::ptr::read_volatile(SPB as *const u32) == SPB_SIGNATURE }
}

fn slot(index: usize) -> usize {
    // SAFETY: `present()` was checked at start; the vector lives in PROM
    // memory that indyboot never writes.
    unsafe {
        let vector = core::ptr::read_volatile((SPB + SPB_FIRMWARE_VECTOR) as *const u32);
        core::ptr::read_volatile((vector as usize + 4 * index) as *const u32) as usize
    }
}

/// Write bytes to the PROM console.
pub fn write(bytes: &[u8]) {
    type Write = extern "C" fn(u32, *const u8, u32, *mut u32) -> i32;
    // SAFETY: slot 27 is `Write(fileid, buffer, length, *count)`.
    let call: Write = unsafe { core::mem::transmute(slot(WRITE)) };
    let mut done = 0;
    let mut rest = bytes;
    while !rest.is_empty() {
        let mut count = 0u32;
        let status = call(STDOUT, rest.as_ptr(), rest.len() as u32, &mut count);
        if status != 0 || count == 0 {
            break;
        }
        done += count as usize;
        rest = &bytes[done.min(bytes.len())..];
    }
}

/// Block until the console has a byte and return it.
pub fn read_byte() -> Option<u8> {
    type Read = extern "C" fn(u32, *mut u8, u32, *mut u32) -> i32;
    // SAFETY: slot 25 is `Read(fileid, buffer, length, *count)`.
    let call: Read = unsafe { core::mem::transmute(slot(READ)) };
    let mut byte = 0u8;
    let mut count = 0u32;
    let status = call(STDIN, &mut byte, 1, &mut count);
    (status == 0 && count == 1).then_some(byte)
}

/// Whether `read_byte` would return without waiting.
pub fn key_waiting() -> bool {
    type GetReadStatus = extern "C" fn(u32) -> i32;
    // SAFETY: slot 26 is `GetReadStatus(fileid)`; 0 means a byte is ready.
    let call: GetReadStatus = unsafe { core::mem::transmute(slot(GET_READ_STATUS)) };
    call(STDIN) == 0
}

/// Seconds since the PROM started, from the real-time clock.
pub fn relative_time() -> u32 {
    type GetRelativeTime = extern "C" fn() -> u32;
    // SAFETY: slot 21 is `GetRelativeTime()`.
    let call: GetRelativeTime = unsafe { core::mem::transmute(slot(GET_RELATIVE_TIME)) };
    call()
}

/// The descriptor after `previous`, or the first one for `None`.
pub fn memory_descriptor(
    previous: Option<&'static MemoryDescriptor>,
) -> Option<&'static MemoryDescriptor> {
    type GetMemoryDescriptor = extern "C" fn(*const MemoryDescriptor) -> *const MemoryDescriptor;
    // SAFETY: slot 18 is `GetMemoryDescriptor(previous)`. The list is PROM
    // data that lives until the PROM is replaced.
    let call: GetMemoryDescriptor = unsafe { core::mem::transmute(slot(GET_MEMORY_DESCRIPTOR)) };
    let previous = previous.map_or(core::ptr::null(), |d| d as *const MemoryDescriptor);
    unsafe { call(previous).as_ref() }
}

/// Value of a PROM environment variable. `name` must end with a NUL byte.
pub fn environment(name: &'static [u8]) -> Option<&'static str> {
    debug_assert!(name.last() == Some(&0));
    type GetEnvironmentVariable = extern "C" fn(*const c_char) -> *const c_char;
    // SAFETY: slot 30 is `GetEnvironmentVariable(name)`.
    let call: GetEnvironmentVariable =
        unsafe { core::mem::transmute(slot(GET_ENVIRONMENT_VARIABLE)) };
    let value = call(name.as_ptr() as *const c_char);
    // SAFETY: the PROM returns NULL or a NUL-terminated string it owns.
    unsafe { c_str(value) }
}

/// Write back and invalidate both caches, so code copied into RAM can run.
pub fn flush_all_caches() {
    type FlushAllCaches = extern "C" fn();
    // SAFETY: slot 34 is `FlushAllCaches()`.
    let call: FlushAllCaches = unsafe { core::mem::transmute(slot(FLUSH_ALL_CACHES)) };
    call();
}

/// Return to the PROM's menu.
pub fn enter_interactive_mode() -> ! {
    type EnterInteractiveMode = extern "C" fn() -> !;
    // SAFETY: slot 7 is `EnterInteractiveMode()`.
    let call: EnterInteractiveMode = unsafe { core::mem::transmute(slot(ENTER_INTERACTIVE_MODE)) };
    call()
}

/// Borrow a NUL-terminated string. Returns `None` for NULL or non-UTF-8.
///
/// # Safety
/// `ptr` is NULL or points at a NUL-terminated string that is never freed.
pub unsafe fn c_str(ptr: *const c_char) -> Option<&'static str> {
    if ptr.is_null() {
        return None;
    }
    let mut len = 0;
    while *ptr.add(len) != 0 {
        len += 1;
        if len > 1024 {
            return None;
        }
    }
    core::str::from_utf8(core::slice::from_raw_parts(ptr as *const u8, len)).ok()
}

// SPDX-License-Identifier: GPL-3.0-only
//! Millisecond clock from the CP0 Count register.
//!
//! Count runs at half the CPU pipeline clock, which differs between an
//! R4600, an R5000 and an R4400. `calibrate` measures it against one second
//! of the PROM's real-time clock. Count wraps every 2^32 ticks (43 s at
//! 100 MHz), so `now_ms` adds up the difference since its previous call and
//! must run at least once per wrap. Time spent blocked in a PROM `Read` at
//! the menu is not counted, and nothing there depends on it.

use crate::arcs;

/// Used when the real-time clock does not advance: an R4600 at 100 MHz.
const FALLBACK_TICKS_PER_MS: u32 = 50_000;

struct State {
    ticks_per_ms: u32,
    last_count: u32,
    total_ticks: u64,
}

static mut STATE: State = State {
    ticks_per_ms: FALLBACK_TICKS_PER_MS,
    last_count: 0,
    total_ticks: 0,
};

fn state() -> &'static mut State {
    // SAFETY: indyboot is single-threaded and takes no interrupts.
    unsafe { &mut *core::ptr::addr_of_mut!(STATE) }
}

/// Raw CP0 Count.
pub fn count() -> u32 {
    let value: u32;
    // SAFETY: reading CP0 register 9 has no side effects.
    unsafe { core::arch::asm!("mfc0 $8, $9", out("$8") value, options(nomem, nostack)) };
    value
}

/// Measure Count against the real-time clock. Takes one to two seconds.
/// Returns the Count frequency in Hz, or `None` when the fallback is in use.
pub fn calibrate() -> Option<u32> {
    // Bound each wait in Count ticks: 2^32 ticks is at least 17 s on any
    // Indy CPU, and a working clock changes within one second.
    let wait_for_change = |from: u32| -> Option<u32> {
        let begin = count();
        loop {
            let seconds = arcs::relative_time();
            if seconds != from {
                return Some(seconds);
            }
            if count().wrapping_sub(begin) > 0xF000_0000 {
                return None;
            }
        }
    };
    let state = state();
    state.last_count = count();
    let first = wait_for_change(arcs::relative_time())?;
    let start = count();
    wait_for_change(first)?;
    let ticks = count().wrapping_sub(start);
    state.last_count = count();
    // Below 1 MHz or above 1 GHz the measurement is not a CPU clock.
    if !(1_000_000..=1_000_000_000).contains(&ticks) {
        return None;
    }
    state.ticks_per_ms = ticks / 1000;
    Some(ticks)
}

/// Milliseconds since the first call.
pub fn now_ms() -> u64 {
    let state = state();
    let count = count();
    state.total_ticks += count.wrapping_sub(state.last_count) as u64;
    state.last_count = count;
    state.total_ticks / state.ticks_per_ms as u64
}

pub fn now() -> smoltcp::time::Instant {
    smoltcp::time::Instant::from_millis(now_ms() as i64)
}

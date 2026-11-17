// SPDX-License-Identifier: GPL-3.0-only
//! The PROM's memory map, and the checks made before an image is written.

use crate::arcs::{self, FREE_CONTIGUOUS, FREE_MEMORY};
use heapless::Vec;
use indyboot_core::ecoff::Memory;

const PAGE: u32 = 4096;

/// Physical range of the Ethernet DMA buffers; see `net.rs`.
pub const DMA_PHYS: u32 = 0x09BE_0000;
pub const DMA_SIZE: u32 = 0x0002_0000;

/// A physical range the PROM reports as free.
#[derive(Clone, Copy)]
pub struct Range {
    pub start: u32,
    pub end: u32,
}

/// Free memory, read once from the ARCS descriptors.
pub struct Map {
    free: Vec<Range, 16>,
    /// Every descriptor, for the menu's `i` command.
    all: Vec<(u32, Range), 24>,
}

impl Map {
    pub fn read() -> Self {
        let mut map = Map {
            free: Vec::new(),
            all: Vec::new(),
        };
        let mut cursor = None;
        while let Some(descriptor) = arcs::memory_descriptor(cursor) {
            cursor = Some(descriptor);
            let start = descriptor.base_page.saturating_mul(PAGE);
            let end = start.saturating_add(descriptor.page_count.saturating_mul(PAGE));
            let range = Range { start, end };
            let _ = map.all.push((descriptor.kind, range));
            if matches!(descriptor.kind, FREE_CONTIGUOUS | FREE_MEMORY) {
                let _ = map.free.push(range);
            }
        }
        map
    }

    pub fn descriptors(&self) -> &[(u32, Range)] {
        &self.all
    }

    pub fn free_bytes(&self) -> u32 {
        self.free.iter().map(|r| r.end - r.start).sum()
    }

    /// Whether the physical range is free memory. The range may cross from
    /// one free descriptor into an adjacent one.
    pub fn is_free(&self, start: u32, len: u32) -> bool {
        let Some(end) = start.checked_add(len) else {
            return false;
        };
        let mut cursor = start;
        while cursor < end {
            match self
                .free
                .iter()
                .find(|r| r.start <= cursor && cursor < r.end)
            {
                Some(range) => cursor = range.end,
                None => return false,
            }
        }
        true
    }
}

/// Destination for an image: RAM through kseg0 or kseg1, limited to free
/// memory outside the DMA buffers.
pub struct ImageMemory<'a> {
    pub map: &'a Map,
}

impl Memory for ImageMemory<'_> {
    fn allowed(&self, vaddr: u32, len: u32) -> bool {
        // kseg0 and kseg1 map physical 0..512 MiB directly.
        if !(0x8000_0000..0xC000_0000).contains(&vaddr) {
            return false;
        }
        let phys = vaddr & 0x1FFF_FFFF;
        let Some(end) = phys.checked_add(len) else {
            return false;
        };
        let overlaps_dma = phys < DMA_PHYS + DMA_SIZE && DMA_PHYS < end;
        !overlaps_dma && self.map.is_free(phys, len)
    }

    fn write(&mut self, vaddr: u32, data: &[u8]) {
        // SAFETY: `allowed` accepted the section this chunk belongs to.
        unsafe {
            core::ptr::copy_nonoverlapping(data.as_ptr(), vaddr as usize as *mut u8, data.len());
        }
    }

    fn zero(&mut self, vaddr: u32, len: u32) {
        // SAFETY: as above.
        unsafe { core::ptr::write_bytes(vaddr as usize as *mut u8, 0, len as usize) };
    }
}

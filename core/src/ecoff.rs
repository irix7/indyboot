// SPDX-License-Identifier: GPL-3.0-only
//! Streaming loader for big-endian MIPS ECOFF executables.
//!
//! The image arrives as HTTP body chunks. [`Loader::feed`] collects the
//! headers, checks every section against the memory map, then copies each
//! later chunk straight to its section's address. Nothing is buffered beyond
//! the headers, so the image size is limited by free memory only.
//!
//! Layout read here: a 20-byte file header, an optional header of `f_opthdr`
//! bytes (the 56-byte a.out header), then `f_nscns` 40-byte section headers.

use heapless::Vec;

/// `f_magic` of a big-endian MIPS ECOFF file.
pub const MAGIC_MIPSEB: u16 = 0x0160;
/// Sections a file may have.
pub const MAX_SECTIONS: usize = 8;
/// Largest header block: file header, a.out header and section headers.
pub const HEADER_MAX: usize = FILHSZ + AOUTHSZ + SCNHSZ * MAX_SECTIONS;

const FILHSZ: usize = 20;
const AOUTHSZ: usize = 56;
const SCNHSZ: usize = 40;

/// Section flags for sections with no bytes in the file.
const STYP_BSS: u32 = 0x80;
const STYP_SBSS: u32 = 0x400;

/// Where the image is written. The loader crate implements this over RAM and
/// the ARCS memory descriptors; tests implement it over a byte vector.
pub trait Memory {
    /// Whether `len` bytes at virtual address `vaddr` may be overwritten.
    fn allowed(&self, vaddr: u32, len: u32) -> bool;
    fn write(&mut self, vaddr: u32, data: &[u8]);
    fn zero(&mut self, vaddr: u32, len: u32);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcoffError {
    /// `f_magic` is not 0x0160.
    Magic,
    /// Header sizes are inconsistent, or there are more than
    /// [`MAX_SECTIONS`] sections.
    Header,
    /// A section's file bytes start inside the headers, or two sections
    /// share file bytes.
    Layout,
    /// A section does not fit in free memory. Holds its address and size.
    Range(u32, u32),
    /// The entry point is outside every loaded section.
    Entry(u32),
    /// The file ended before the last section was complete.
    Truncated,
}

impl EcoffError {
    pub fn message(self) -> &'static str {
        match self {
            EcoffError::Magic => "not a big-endian MIPS ECOFF file (magic is not 0x0160)",
            EcoffError::Header => "bad ECOFF header",
            EcoffError::Layout => "ECOFF sections overlap in the file",
            EcoffError::Range(..) => "section does not fit in free memory",
            EcoffError::Entry(_) => "entry point is outside the loaded sections",
            EcoffError::Truncated => "image ended before its last section",
        }
    }
}

/// File bytes `[file_off, file_off + size)` go to `vaddr`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub file_off: u32,
    pub vaddr: u32,
    pub size: u32,
}

/// What to do with the file, read from its headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub entry: u32,
    /// Bytes of the file the headers occupy.
    pub header_len: usize,
    pub load: Vec<Segment, MAX_SECTIONS>,
    /// `(vaddr, size)` ranges to fill with zeros.
    pub zero: Vec<(u32, u32), MAX_SECTIONS>,
}

impl Plan {
    /// File offset one past the last loaded byte.
    pub fn file_end(&self) -> u32 {
        self.load
            .iter()
            .map(|s| s.file_off + s.size)
            .max()
            .unwrap_or(0)
    }

    /// Lowest and one-past-highest address the image occupies.
    pub fn extent(&self) -> (u32, u32) {
        let ranges = self
            .load
            .iter()
            .map(|s| (s.vaddr, s.size))
            .chain(self.zero.iter().copied());
        let mut low = u32::MAX;
        let mut high = 0;
        for (vaddr, size) in ranges {
            low = low.min(vaddr);
            high = high.max(vaddr.saturating_add(size));
        }
        (low, high)
    }
}

fn be16(buf: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([buf[at], buf[at + 1]])
}

fn be32(buf: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}

/// Bytes of header the file has, from its first [`FILHSZ`] bytes.
fn header_len(buf: &[u8]) -> Result<usize, EcoffError> {
    if be16(buf, 0) != MAGIC_MIPSEB {
        return Err(EcoffError::Magic);
    }
    let sections = be16(buf, 2) as usize;
    let opthdr = be16(buf, 16) as usize;
    if sections == 0 || sections > MAX_SECTIONS || opthdr != AOUTHSZ {
        return Err(EcoffError::Header);
    }
    Ok(FILHSZ + opthdr + SCNHSZ * sections)
}

/// Read the plan from a complete header block.
fn parse_plan(buf: &[u8]) -> Result<Plan, EcoffError> {
    let header_len = header_len(buf)?;
    let sections = be16(buf, 2) as usize;
    let entry = be32(buf, FILHSZ + 16);
    let mut plan = Plan {
        entry,
        header_len,
        load: Vec::new(),
        zero: Vec::new(),
    };
    for index in 0..sections {
        let at = FILHSZ + AOUTHSZ + SCNHSZ * index;
        let vaddr = be32(buf, at + 12);
        let size = be32(buf, at + 16);
        let file_off = be32(buf, at + 20);
        let flags = be32(buf, at + 36);
        if size == 0 {
            continue;
        }
        if vaddr.checked_add(size).is_none() {
            return Err(EcoffError::Range(vaddr, size));
        }
        if flags & (STYP_BSS | STYP_SBSS) != 0 || file_off == 0 {
            let _ = plan.zero.push((vaddr, size));
            continue;
        }
        if (file_off as usize) < header_len || file_off.checked_add(size).is_none() {
            return Err(EcoffError::Layout);
        }
        let _ = plan.load.push(Segment {
            file_off,
            vaddr,
            size,
        });
    }
    for (i, a) in plan.load.iter().enumerate() {
        for b in plan.load.iter().skip(i + 1) {
            if a.file_off < b.file_off + b.size && b.file_off < a.file_off + a.size {
                return Err(EcoffError::Layout);
            }
        }
    }
    let entry_loaded = plan
        .load
        .iter()
        .any(|s| entry >= s.vaddr && entry - s.vaddr < s.size);
    if !entry_loaded {
        return Err(EcoffError::Entry(entry));
    }
    Ok(plan)
}

/// Loads an ECOFF file that arrives in order, in chunks of any size.
pub struct Loader {
    head: [u8; HEADER_MAX],
    head_have: usize,
    plan: Option<Plan>,
    /// File offset of the next byte `feed` will see.
    pos: u32,
}

impl Default for Loader {
    fn default() -> Self {
        Self::new()
    }
}

impl Loader {
    pub fn new() -> Self {
        Loader {
            head: [0; HEADER_MAX],
            head_have: 0,
            plan: None,
            pos: 0,
        }
    }

    /// The plan, once the headers have arrived.
    pub fn plan(&self) -> Option<&Plan> {
        self.plan.as_ref()
    }

    /// Take the next bytes of the file.
    ///
    /// Memory is first written when the headers are complete and every
    /// section has passed [`Memory::allowed`].
    pub fn feed(&mut self, mut data: &[u8], memory: &mut impl Memory) -> Result<(), EcoffError> {
        if self.plan.is_none() {
            // Take header bytes until the block is complete.
            while self.plan.is_none() && !data.is_empty() {
                let want = if self.head_have < FILHSZ {
                    FILHSZ
                } else {
                    header_len(&self.head[..FILHSZ])?
                };
                let take = (want - self.head_have).min(data.len());
                self.head[self.head_have..self.head_have + take].copy_from_slice(&data[..take]);
                self.head_have += take;
                self.pos += take as u32;
                data = &data[take..];
                if self.head_have >= FILHSZ && self.head_have == header_len(&self.head[..FILHSZ])? {
                    let plan = parse_plan(&self.head[..self.head_have])?;
                    for seg in &plan.load {
                        if !memory.allowed(seg.vaddr, seg.size) {
                            return Err(EcoffError::Range(seg.vaddr, seg.size));
                        }
                    }
                    for &(vaddr, size) in &plan.zero {
                        if !memory.allowed(vaddr, size) {
                            return Err(EcoffError::Range(vaddr, size));
                        }
                    }
                    for &(vaddr, size) in &plan.zero {
                        memory.zero(vaddr, size);
                    }
                    self.plan = Some(plan);
                }
            }
            if data.is_empty() {
                return Ok(());
            }
        }
        let Some(plan) = &self.plan else {
            return Ok(());
        };
        let start = self.pos;
        let end = start.saturating_add(data.len() as u32);
        for seg in &plan.load {
            let seg_end = seg.file_off + seg.size;
            let from = start.max(seg.file_off);
            let to = end.min(seg_end);
            if from < to {
                let chunk = &data[(from - start) as usize..(to - start) as usize];
                memory.write(seg.vaddr + (from - seg.file_off), chunk);
            }
        }
        self.pos = end;
        Ok(())
    }

    /// Check the whole file arrived and return the entry point.
    pub fn finish(&self) -> Result<u32, EcoffError> {
        let plan = self.plan.as_ref().ok_or(EcoffError::Truncated)?;
        if self.pos < plan.file_end() {
            return Err(EcoffError::Truncated);
        }
        Ok(plan.entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u32 = 0x8804_0000;

    /// 64 KiB of memory at `BASE`, pre-filled with 0xEE.
    struct Ram {
        bytes: std::vec::Vec<u8>,
        writes: usize,
    }

    impl Ram {
        fn new() -> Self {
            Ram {
                bytes: vec![0xEE; 0x10000],
                writes: 0,
            }
        }
    }

    impl Memory for Ram {
        fn allowed(&self, vaddr: u32, len: u32) -> bool {
            vaddr >= BASE && (vaddr - BASE) as usize + len as usize <= self.bytes.len()
        }
        fn write(&mut self, vaddr: u32, data: &[u8]) {
            let at = (vaddr - BASE) as usize;
            self.bytes[at..at + data.len()].copy_from_slice(data);
            self.writes += 1;
        }
        fn zero(&mut self, vaddr: u32, len: u32) {
            let at = (vaddr - BASE) as usize;
            self.bytes[at..at + len as usize].fill(0);
            self.writes += 1;
        }
    }

    struct Section {
        vaddr: u32,
        data: std::vec::Vec<u8>,
        flags: u32,
        bss_size: u32,
    }

    /// Build an ECOFF file the way the projects' converters do: headers,
    /// padding to offset 208, then the section bytes in order.
    fn build(entry: u32, sections: &[Section]) -> std::vec::Vec<u8> {
        let mut out = vec![0u8; 208];
        out[0..2].copy_from_slice(&MAGIC_MIPSEB.to_be_bytes());
        out[2..4].copy_from_slice(&(sections.len() as u16).to_be_bytes());
        out[16..18].copy_from_slice(&(AOUTHSZ as u16).to_be_bytes());
        out[18..20].copy_from_slice(&0x100fu16.to_be_bytes());
        out[20..22].copy_from_slice(&0x0107u16.to_be_bytes());
        out[36..40].copy_from_slice(&entry.to_be_bytes());
        for (i, section) in sections.iter().enumerate() {
            let at = FILHSZ + AOUTHSZ + SCNHSZ * i;
            let is_bss = section.flags & STYP_BSS != 0;
            let size = if is_bss {
                section.bss_size
            } else {
                section.data.len() as u32
            };
            let file_off = if is_bss { 0 } else { out.len() as u32 };
            out[at + 8..at + 12].copy_from_slice(&section.vaddr.to_be_bytes());
            out[at + 12..at + 16].copy_from_slice(&section.vaddr.to_be_bytes());
            out[at + 16..at + 20].copy_from_slice(&size.to_be_bytes());
            out[at + 20..at + 24].copy_from_slice(&file_off.to_be_bytes());
            out[at + 36..at + 40].copy_from_slice(&section.flags.to_be_bytes());
            out.extend_from_slice(&section.data);
        }
        out
    }

    fn sample() -> std::vec::Vec<u8> {
        build(
            BASE,
            &[
                Section {
                    vaddr: BASE,
                    data: (0..=255).cycle().take(3000).collect(),
                    flags: 0x20,
                    bss_size: 0,
                },
                Section {
                    vaddr: BASE + 0x1000,
                    data: vec![0xAB; 777],
                    flags: 0x40,
                    bss_size: 0,
                },
                Section {
                    vaddr: BASE + 0x2000,
                    data: vec![],
                    flags: STYP_BSS,
                    bss_size: 0x400,
                },
            ],
        )
    }

    fn check_loaded(ram: &Ram) {
        let text: std::vec::Vec<u8> = (0..=255).cycle().take(3000).collect();
        assert_eq!(&ram.bytes[..3000], &text[..]);
        assert_eq!(ram.bytes[3000], 0xEE);
        assert!(ram.bytes[0x1000..0x1000 + 777].iter().all(|&b| b == 0xAB));
        assert_eq!(ram.bytes[0x1000 + 777], 0xEE);
        assert!(ram.bytes[0x2000..0x2400].iter().all(|&b| b == 0));
        assert_eq!(ram.bytes[0x2400], 0xEE);
    }

    #[test]
    fn loads_in_one_chunk() {
        let file = sample();
        let mut ram = Ram::new();
        let mut loader = Loader::new();
        loader.feed(&file, &mut ram).unwrap();
        assert_eq!(loader.finish(), Ok(BASE));
        check_loaded(&ram);
        let plan = loader.plan().unwrap();
        assert_eq!(plan.extent(), (BASE, BASE + 0x2400));
        assert_eq!(plan.file_end() as usize, file.len());
    }

    #[test]
    fn loads_in_every_chunk_size() {
        let file = sample();
        for chunk in [
            1, 2, 3, 7, 19, 20, 21, 75, 76, 77, 195, 196, 197, 208, 512, 1460,
        ] {
            let mut ram = Ram::new();
            let mut loader = Loader::new();
            for piece in file.chunks(chunk) {
                loader.feed(piece, &mut ram).unwrap();
            }
            assert_eq!(loader.finish(), Ok(BASE), "chunk size {chunk}");
            check_loaded(&ram);
        }
    }

    #[test]
    fn trailing_bytes_are_ignored() {
        let mut file = sample();
        file.extend_from_slice(&[0x55; 100]);
        let mut ram = Ram::new();
        let mut loader = Loader::new();
        loader.feed(&file, &mut ram).unwrap();
        assert_eq!(loader.finish(), Ok(BASE));
        check_loaded(&ram);
    }

    #[test]
    fn truncated_file_fails_at_finish() {
        let file = sample();
        let mut ram = Ram::new();
        let mut loader = Loader::new();
        loader.feed(&file[..file.len() - 1], &mut ram).unwrap();
        assert_eq!(loader.finish(), Err(EcoffError::Truncated));
        let mut loader = Loader::new();
        loader.feed(&file[..10], &mut ram).unwrap();
        assert_eq!(loader.finish(), Err(EcoffError::Truncated));
    }

    #[test]
    fn rejects_other_formats_before_writing() {
        let mut ram = Ram::new();
        let mut elf = sample();
        elf[..4].copy_from_slice(b"\x7fELF");
        assert_eq!(Loader::new().feed(&elf, &mut ram), Err(EcoffError::Magic));
        let mut swapped = sample();
        swapped[..2].copy_from_slice(&[0x60, 0x01]);
        assert_eq!(
            Loader::new().feed(&swapped, &mut ram),
            Err(EcoffError::Magic)
        );
        let mut sections = sample();
        sections[2..4].copy_from_slice(&9u16.to_be_bytes());
        assert_eq!(
            Loader::new().feed(&sections, &mut ram),
            Err(EcoffError::Header)
        );
        assert_eq!(ram.writes, 0);
    }

    #[test]
    fn rejects_sections_outside_free_memory_before_writing() {
        let file = build(
            BASE,
            &[
                Section {
                    vaddr: BASE,
                    data: vec![1; 64],
                    flags: 0x20,
                    bss_size: 0,
                },
                Section {
                    vaddr: BASE + 0xFFF0,
                    data: vec![2; 64],
                    flags: 0x40,
                    bss_size: 0,
                },
            ],
        );
        let mut ram = Ram::new();
        assert_eq!(
            Loader::new().feed(&file, &mut ram),
            Err(EcoffError::Range(BASE + 0xFFF0, 64))
        );
        assert_eq!(ram.writes, 0);
    }

    #[test]
    fn rejects_entry_outside_the_image() {
        let file = build(
            BASE + 0x100,
            &[Section {
                vaddr: BASE,
                data: vec![1; 64],
                flags: 0x20,
                bss_size: 0,
            }],
        );
        let mut ram = Ram::new();
        assert_eq!(
            Loader::new().feed(&file, &mut ram),
            Err(EcoffError::Entry(BASE + 0x100))
        );
    }

    #[test]
    fn rejects_section_bytes_inside_the_headers() {
        let mut file = sample();
        let at = FILHSZ + AOUTHSZ + 20;
        file[at..at + 4].copy_from_slice(&100u32.to_be_bytes());
        let mut ram = Ram::new();
        assert_eq!(Loader::new().feed(&file, &mut ram), Err(EcoffError::Layout));
    }
}

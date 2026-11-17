// SPDX-License-Identifier: GPL-3.0-only
//! Convert a big-endian MIPS ELF32 executable to the ECOFF file the Indy PROM
//! loads with `boot -f bootp()NAME`.
//!
//! Usage: `mkecoff INPUT.elf OUTPUT`
//!
//! The PT_LOAD segments are flattened into one image from the lowest address.
//! Bytes before the first writable segment become `.text`, the rest become
//! `.data`, and memory beyond the file bytes becomes `.bss`. The output has
//! the 20-byte file header, the 56-byte a.out header, three section headers,
//! and the image starting at file offset 208.

use std::process::ExitCode;

const TEXT_OFFSET: usize = 208;
const PT_LOAD: u32 = 1;
const PF_W: u32 = 2;

fn be16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

struct Load {
    vaddr: u32,
    data: Vec<u8>,
    memsz: u32,
    writable: bool,
}

fn convert(elf: &[u8]) -> Result<Vec<u8>, String> {
    if elf.len() < 52 || &elf[..4] != b"\x7fELF" {
        return Err("not an ELF file".into());
    }
    if elf[4] != 1 || elf[5] != 2 {
        return Err("need ELF32, big-endian".into());
    }
    if be16(elf, 18) != 8 {
        return Err("need a MIPS ELF (e_machine 8)".into());
    }
    let entry = be32(elf, 24);
    let phoff = be32(elf, 28) as usize;
    let phentsize = be16(elf, 42) as usize;
    let phnum = be16(elf, 44) as usize;

    let mut loads = Vec::new();
    for i in 0..phnum {
        let at = phoff + i * phentsize;
        let header = elf
            .get(at..at + 32)
            .ok_or("program header outside the file")?;
        if be32(header, 0) != PT_LOAD {
            continue;
        }
        let offset = be32(header, 4) as usize;
        let filesz = be32(header, 16) as usize;
        let memsz = be32(header, 20);
        if memsz == 0 {
            continue;
        }
        let data = elf
            .get(offset..offset + filesz)
            .ok_or("segment outside the file")?;
        loads.push(Load {
            vaddr: be32(header, 8),
            data: data.to_vec(),
            memsz,
            writable: be32(header, 24) & PF_W != 0,
        });
    }
    if loads.is_empty() {
        return Err("no PT_LOAD segments".into());
    }
    loads.sort_by_key(|load| load.vaddr);

    let base = loads[0].vaddr;
    let file_end = loads
        .iter()
        .filter(|load| !load.data.is_empty())
        .map(|load| load.vaddr + load.data.len() as u32)
        .max()
        .ok_or("no segment has file bytes")?;
    let mem_end = loads
        .iter()
        .map(|load| load.vaddr + load.memsz)
        .max()
        .unwrap();
    let data_start = loads
        .iter()
        .find(|load| load.writable && !load.data.is_empty())
        .map(|load| load.vaddr)
        .unwrap_or(file_end);

    let mut image = vec![0u8; (file_end - base) as usize];
    for load in &loads {
        let at = (load.vaddr - base) as usize;
        image[at..at + load.data.len()].copy_from_slice(&load.data);
    }
    if entry < base || entry >= data_start {
        return Err(format!(
            "entry {entry:#x} is outside .text {base:#x}..{data_start:#x}"
        ));
    }

    let tsize = data_start - base;
    let dsize = file_end - data_start;
    let bsize = mem_end - file_end;

    let mut out = vec![0u8; TEXT_OFFSET];
    let put16 =
        |out: &mut Vec<u8>, at: usize, v: u16| out[at..at + 2].copy_from_slice(&v.to_be_bytes());
    let put32 =
        |out: &mut Vec<u8>, at: usize, v: u32| out[at..at + 4].copy_from_slice(&v.to_be_bytes());
    // File header.
    put16(&mut out, 0, 0x0160); // MIPSEB
    put16(&mut out, 2, 3); // sections
    put16(&mut out, 16, 56); // a.out header size
    put16(&mut out, 18, 0x100f); // relocs and symbols stripped, executable, big-endian
                                 // a.out header.
    put16(&mut out, 20, 0x0107); // OMAGIC
    put32(&mut out, 24, tsize);
    put32(&mut out, 28, dsize);
    put32(&mut out, 32, bsize);
    put32(&mut out, 36, entry);
    put32(&mut out, 40, base);
    put32(&mut out, 44, data_start);
    put32(&mut out, 48, file_end);
    // Section headers.
    let sections: [(&[u8], u32, u32, u32, u32); 3] = [
        (b".text", base, tsize, TEXT_OFFSET as u32, 0x20),
        (
            b".data",
            data_start,
            dsize,
            TEXT_OFFSET as u32 + tsize,
            0x40,
        ),
        (b".bss", file_end, bsize, 0, 0x80),
    ];
    for (i, (name, addr, size, offset, flags)) in sections.into_iter().enumerate() {
        let at = 76 + 40 * i;
        out[at..at + name.len()].copy_from_slice(name);
        put32(&mut out, at + 8, addr);
        put32(&mut out, at + 12, addr);
        put32(&mut out, at + 16, size);
        put32(&mut out, at + 20, offset);
        put32(&mut out, at + 36, flags);
    }
    out.extend_from_slice(&image);
    eprintln!(
        "mkecoff: text {base:#x}+{tsize:#x} data {data_start:#x}+{dsize:#x} bss {file_end:#x}+{bsize:#x} entry {entry:#x}, {} bytes",
        out.len()
    );
    Ok(out)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: mkecoff INPUT.elf OUTPUT");
        return ExitCode::from(2);
    }
    let result = std::fs::read(&args[1])
        .map_err(|e| format!("{}: {e}", args[1]))
        .and_then(|elf| convert(&elf))
        .and_then(|out| std::fs::write(&args[2], out).map_err(|e| format!("{}: {e}", args[2])));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("mkecoff: {message}");
            ExitCode::FAILURE
        }
    }
}

// SPDX-License-Identifier: GPL-3.0-only
//! indyboot: a network bootloader for the SGI Indy.
//!
//! The ARCS PROM loads this program with `boot -f bootp()indyboot`. It
//! configures the network, fetches a manifest over HTTP, lists the operating
//! systems the manifest names on the PROM console, then loads the chosen
//! ECOFF image and starts it the way the PROM would have.

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]

mod arcs;
mod boot;
mod clock;
mod console;
mod mem;
mod net;
mod options;

use core::ffi::c_char;
use core::net::Ipv4Addr;
use indyboot_core::manifest::{Level, Manifest, MANIFEST_MAX};
use net::{FetchError, Lease, Net};
use options::{Address, Options};

core::arch::global_asm!(
    r#"
    .section .text._start, "ax"
    .globl _start
    .set noreorder
_start:
    # Clear .bss. The PROM enters with a0 = argc, a1 = argv, a2 = envp.
    la      $t0, __bss_start
    la      $t1, __bss_end
1:  sltu    $t2, $t0, $t1
    beqz    $t2, 2f
    nop
    sw      $zero, 0($t0)
    b       1b
    addiu   $t0, $t0, 4
2:  # Keep the PROM's stack and return address for the handoff and for exit.
    la      $t0, PROM_FRAME
    sw      $sp, 0($t0)
    sw      $ra, 4($t0)
    # Fill the stack with a pattern so its high-water mark can be measured.
    la      $t0, __stack_bottom
    la      $t1, __stack_top
    li      $t2, 0x5AA55AA5
3:  sw      $t2, 0($t0)
    addiu   $t0, $t0, 4
    bne     $t0, $t1, 3b
    nop
    la      $sp, __stack_top
    addiu   $sp, $sp, -16
    jal     indyboot_main
    nop
    la      $t0, PROM_FRAME
    lw      $sp, 0($t0)
    lw      $ra, 4($t0)
    jr      $ra
    nop

    # indyboot_handoff(entry, argc, argv, envp): enter an image on the
    # PROM's stack with a0 = argc, a1 = argv, a2 = envp.
    .globl indyboot_handoff
indyboot_handoff:
    move    $t9, $a0
    move    $a0, $a1
    move    $a1, $a2
    move    $a2, $a3
    la      $t0, PROM_FRAME
    lw      $sp, 0($t0)
    addiu   $sp, $sp, -32
    jalr    $t9
    nop
    j       indyboot_image_returned
    nop
    .set reorder
    .text
"#
);

/// The PROM's `$sp` and `$ra` at entry.
#[no_mangle]
static mut PROM_FRAME: [u32; 2] = [0; 2];

/// Manifest URL used when the boot command and the PROM give none. The
/// default is the lab netboot server; `INDYBOOT_URL=... ./build.sh` replaces it.
const BUILTIN_URL: Option<&str> = Some(match option_env!("INDYBOOT_URL") {
    Some(url) => url,
    None => "http://10.1.1.3/boot.cfg",
});

static mut MANIFEST_TEXT: [u8; MANIFEST_MAX] = [0; MANIFEST_MAX];

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    if let Some(location) = info.location() {
        println!(
            "\nindyboot: internal error at {}:{}",
            location.file(),
            location.line()
        );
    } else {
        println!("\nindyboot: internal error");
    }
    println!("Press a key to return to the PROM.");
    console::wait_key();
    arcs::enter_interactive_mode()
}

/// An image returned to indyboot instead of running forever.
#[no_mangle]
extern "C" fn indyboot_image_returned() -> ! {
    println!("\nindyboot: the image returned; back to the PROM.");
    arcs::enter_interactive_mode()
}

#[no_mangle]
extern "C" fn indyboot_main(
    argc: i32,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> i32 {
    if !arcs::present() {
        // No firmware vector: nothing can be printed. Return to the caller.
        return 1;
    }
    println!("\nindyboot {} for the SGI Indy", env!("CARGO_PKG_VERSION"));

    let options = Options::read(argc, argv, BUILTIN_URL);
    let map = mem::Map::read();
    let hz = clock::calibrate();
    match hz {
        Some(hz) => println!(
            "CPU count clock {}.{} MHz, {} MiB free",
            hz / 1_000_000,
            hz / 100_000 % 10,
            map.free_bytes() >> 20
        ),
        None => println!("Real-time clock did not advance; timeouts assume a 100 MHz CPU"),
    }

    let mac = station_address();
    println!(
        "Ethernet {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );
    let mut net = match Net::new(mac, &map) {
        Ok(net) => net,
        Err(reason) => return fail(reason),
    };
    if !configure(&mut net, &options) {
        return fail(
            "no network address. Add ip=ADDRESS/PREFIX gw=ROUTER dns=SERVER to the boot command.",
        );
    }

    let Some(url) = options.url else {
        return fail(
            "no manifest URL. Boot with: boot -f bootp()indyboot url=http://HOST/boot.cfg",
        );
    };

    let mut manifest = Manifest::new();
    loop {
        load_manifest(&mut net, url, &mut manifest);
        match menu(&manifest, &map, &net) {
            Choice::Boot(index) => {
                let entry = &manifest.entries[index];
                if let Err(()) = boot::boot(&mut net, &map, entry, envp) {
                    println!("Press a key for the menu.");
                    console::wait_key();
                }
            }
            Choice::Reload => {}
            Choice::Quit => {
                net.stop();
                return 0;
            }
        }
    }
}

fn fail(reason: &str) -> i32 {
    println!("indyboot: {}", reason);
    println!("Press a key to return to the PROM.");
    console::wait_key();
    1
}

/// The station address: the PROM's `eaddr` variable, then the copy in the
/// DS1386 NVRAM, then a locally administered fallback.
fn station_address() -> [u8; 6] {
    if let Some(mac) = arcs::environment(b"eaddr\0").and_then(parse_mac) {
        return mac;
    }
    let mut mac = [0u8; 6];
    for (i, byte) in mac.iter_mut().enumerate() {
        // SAFETY: the DS1386 window is device memory; this only reads it.
        let word = unsafe { core::ptr::read_volatile((0xBFBE_04E8usize + 4 * i) as *const u32) };
        *byte = word as u8;
    }
    if mac == [0xFF; 6] || mac == [0; 6] {
        println!("No station address in the PROM; using 02:00:69:12:34:56");
        return [0x02, 0x00, 0x69, 0x12, 0x34, 0x56];
    }
    mac
}

fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let mut mac = [0u8; 6];
    let mut parts = text.split(':');
    for byte in &mut mac {
        *byte = u8::from_str_radix(parts.next()?, 16).ok()?;
    }
    (parts.next().is_none() && mac != [0; 6] && mac != [0xFF; 6]).then_some(mac)
}

/// Get an address. BOOTP runs first because the PROM has just used a BOOTP
/// server. DHCP follows when BOOTP gave no router or no DNS server.
fn configure(net: &mut Net, options: &Options) -> bool {
    let lease = match options.address {
        Address::Static { address, prefix } => Some(Lease {
            address,
            prefix,
            router: None,
            dns: None,
            source: "static",
        }),
        Address::Bootp => {
            print!("Asking for an address: ");
            net.bootp()
        }
        Address::Dhcp => {
            print!("Asking for an address: ");
            net.dhcp()
        }
        Address::Auto => {
            print!("Asking for an address: ");
            let first = net.bootp();
            if first.is_some_and(|lease| lease.router.is_some() && lease.dns.is_some()) {
                first
            } else {
                let why = if first.is_some() {
                    "BOOTP gave no router or DNS server"
                } else {
                    "no BOOTP reply"
                };
                print!("{}, trying DHCP: ", why);
                net.dhcp().or(first)
            }
        }
    };
    let Some(mut lease) = lease else {
        println!("no reply");
        return false;
    };
    lease.router = options.router.or(lease.router);
    lease.dns = options.dns.or(lease.dns);
    net.apply(lease);
    show_lease(&lease);
    true
}

fn show_lease(lease: &Lease) {
    print!(
        "{} address {}/{}",
        lease.source, lease.address, lease.prefix
    );
    show("router", lease.router);
    show("DNS", lease.dns);
    println!();
}

fn show(label: &str, address: Option<Ipv4Addr>) {
    match address {
        Some(address) => print!(", {} {}", label, address),
        None => print!(", no {}", label),
    }
}

pub fn fetch_error(error: FetchError) {
    match error {
        FetchError::Url(error) => println!("{}", error.message()),
        FetchError::NoDnsServer => {
            println!("no DNS server; use an IP address in the URL or add dns=SERVER")
        }
        FetchError::Dns => println!("the host name did not resolve"),
        FetchError::Connect => println!("could not connect"),
        FetchError::Idle => println!("the server sent nothing for 30 seconds"),
        FetchError::Head => println!("the reply is not an HTTP response"),
        FetchError::Status(status) => println!("the server answered HTTP {}", status),
        FetchError::Redirect => println!("too many redirects"),
        FetchError::Short => println!("the connection closed before the end of the file"),
        FetchError::Rejected => println!("stopped"),
    }
}

/// Fetch one manifest file into the text buffer.
fn fetch_text(net: &mut Net, url: &str) -> Result<&'static str, ()> {
    // SAFETY: single-threaded; the previous borrow ended with the last parse.
    let buffer = unsafe { &mut *core::ptr::addr_of_mut!(MANIFEST_TEXT) };
    let mut have = 0usize;
    let mut too_long = false;
    let result = net.get(url, &mut |data| {
        if have + data.len() > buffer.len() {
            too_long = true;
            return Err(FetchError::Rejected);
        }
        buffer[have..have + data.len()].copy_from_slice(data);
        have += data.len();
        Ok(())
    });
    if too_long {
        println!("  {}: longer than {} bytes", url, MANIFEST_MAX);
        return Err(());
    }
    if let Err(error) = result {
        print!("  {}: ", url);
        fetch_error(error);
        return Err(());
    }
    core::str::from_utf8(&buffer[..have]).map_err(|_| println!("  {}: not UTF-8 text", url))
}

/// Fetch the main manifest and its includes. A file that fails is reported
/// and skipped, so one project's broken fragment does not hide the others.
fn load_manifest(net: &mut Net, url: &str, manifest: &mut Manifest) {
    *manifest = Manifest::new();
    println!("Manifest {}", url);
    let Ok(text) = fetch_text(net, url) else {
        return;
    };
    if let Err(error) = manifest.parse(text, url, Level::Main) {
        println!("  {} line {}: {}", url, error.line, error.kind.message());
    }
    let includes = manifest.includes.clone();
    for include in &includes {
        let Ok(text) = fetch_text(net, include) else {
            continue;
        };
        if let Err(error) = manifest.parse(text, include, Level::Included) {
            println!(
                "  {} line {}: {}",
                include,
                error.line,
                error.kind.message()
            );
        }
    }
}

enum Choice {
    Boot(usize),
    Reload,
    Quit,
}

fn menu(manifest: &Manifest, map: &mem::Map, net: &Net) -> Choice {
    let default = manifest.default_index();
    let mut countdown = match (default, manifest.timeout) {
        (Some(_), Some(seconds)) => Some(seconds),
        _ => None,
    };
    loop {
        println!();
        if manifest.entries.is_empty() {
            println!("The manifest lists no operating systems.");
        }
        for (i, entry) in manifest.entries.iter().enumerate() {
            let mark = if Some(i) == default { '*' } else { ' ' };
            print!("{}{:2}  {}", mark, i + 1, entry.title);
            if let Some(size) = entry.size {
                print!("  ({} KiB)", size.div_ceil(1024));
            }
            println!();
        }
        println!("  r  reload the list    i  network and memory    q  return to the PROM");

        if let (Some(index), Some(seconds)) = (default, countdown.take()) {
            println!(
                "Starting {} in {} s. Press a key for the menu.",
                manifest.entries[index].title, seconds
            );
            let deadline = clock::now_ms() + seconds as u64 * 1000;
            let mut interrupted = false;
            while clock::now_ms() < deadline {
                if arcs::key_waiting() {
                    let _ = arcs::read_byte();
                    interrupted = true;
                    break;
                }
            }
            if !interrupted {
                return Choice::Boot(index);
            }
        }

        match default {
            Some(index) => print!("Boot which? [{}] ", index + 1),
            None => print!("Boot which? "),
        }
        let mut buffer = [0u8; 16];
        let line = console::read_line(&mut buffer).trim();
        match line {
            "" => {
                if let Some(index) = default {
                    return Choice::Boot(index);
                }
            }
            "r" | "R" => return Choice::Reload,
            "q" | "Q" => return Choice::Quit,
            "i" | "I" => info(map, net),
            _ => match line.parse::<usize>() {
                Ok(number) if (1..=manifest.entries.len()).contains(&number) => {
                    return Choice::Boot(number - 1);
                }
                _ => println!("Type a number from the list, r, i or q."),
            },
        }
    }
}

extern "C" {
    static __stack_bottom: u32;
    static __stack_top: u32;
}

/// Bytes of stack used so far: the distance from the top to the lowest word
/// that no longer holds the fill pattern.
fn stack_used() -> (usize, usize) {
    // SAFETY: both symbols bound the stack section, which is always mapped.
    unsafe {
        let bottom = core::ptr::addr_of!(__stack_bottom);
        let top = core::ptr::addr_of!(__stack_top);
        let mut cursor = bottom;
        while cursor < top && core::ptr::read_volatile(cursor) == 0x5AA5_5AA5 {
            cursor = cursor.add(1);
        }
        (
            top as usize - cursor as usize,
            top as usize - bottom as usize,
        )
    }
}

fn info(map: &mem::Map, net: &Net) {
    let (used, size) = stack_used();
    println!("Stack high-water mark {} of {} bytes", used, size);
    if let Some(lease) = &net.lease {
        show_lease(lease);
    }
    println!("Ethernet receive channel restarts: {}", net.rx_restarts);
    println!("PROM memory descriptors (type, physical range):");
    for (kind, range) in map.descriptors() {
        let name = match kind {
            0 => "exception block",
            1 => "parameter block",
            2 => "free contiguous",
            3 => "free",
            4 => "bad",
            5 => "loaded program",
            6 => "firmware temporary",
            7 => "firmware permanent",
            _ => "other",
        };
        println!(
            "  {}  {:#010x}..{:#010x}  {}",
            kind, range.start, range.end, name
        );
    }
}

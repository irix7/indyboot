# indyboot

indyboot is a network bootloader for the SGI Indy (IP24, MIPS R4x00 and R5000,
big-endian). The factory ARCS PROM loads it over BOOTP and TFTP. indyboot then
fetches a list of operating systems over HTTP, shows it as a menu on the PROM
console, downloads the image you pick, checks its SHA-256 and starts it.

An image started by indyboot sees the same machine state as one started by
`boot -f bootp()NAME`, so an operating system that netboots from the PROM
needs no code change. [docs/adding-an-os.md](docs/adding-an-os.md) lists what
a project publishes to appear in the menu.

An example session:

```text
>> boot -f bootp()indyboot
indyboot 0.1.0 for the SGI Indy
CPU count clock 50.0 MHz, 30 MiB free
Ethernet 08:00:69:12:12:12
Asking for an address: BOOTP address 10.1.1.112/24, router 10.1.1.1, DNS 10.1.1.1
Manifest http://10.1.1.3/boot.cfg

  1  IRIX 7  (1561 KiB)
  2  Redox OS  (5389 KiB)
  r  reload the list    i  network and memory    q  return to the PROM
Boot which? 2
Loading Redox OS
  http://10.1.1.3/os/redox/redox
  ..........................................
  5517520 bytes, SHA-256 verified, loaded at 0x88040000..0x88583000, entry 0x88040000
```

## Build

The build needs the Rust nightly named in `rust-toolchain.toml` with
`rust-src`. No MIPS C compiler or binutils are used.

```sh
./build.sh
```

The output is `build/indyboot`, a 174 KB ECOFF file. `INDYBOOT_URL` replaces
the compiled-in manifest URL:

```sh
INDYBOOT_URL=http://boot.example.org/boot.cfg ./build.sh
```

## Boot

Copy `build/indyboot` to the TFTP directory of the BOOTP server the Indy
uses. At the PROM's Command Monitor:

```text
>> boot -f bootp()indyboot
```

Words after the file name change the defaults for that boot:

| Word | Effect | Default |
| --- | --- | --- |
| `url=URL` | Fetch this manifest | PROM variable `indyboot_url`, then `http://10.1.1.3/boot.cfg` |
| `ip=bootp` | Use BOOTP only | BOOTP first, then DHCP if BOOTP gave no router or no DNS server |
| `ip=dhcp` | Use DHCP only | |
| `ip=A.B.C.D/N` | Use this address and prefix length | |
| `gw=A.B.C.D` | Use this default router | From BOOTP or DHCP |
| `dns=A.B.C.D` | Use this DNS server | From BOOTP or DHCP |

For an unattended boot, set `SystemPartition=bootp()`, `OSLoader=indyboot`
and `AutoLoad=Yes` in the PROM, and give the manifest a `default` and a
`timeout`.

## Menu

| Key | Action |
| --- | --- |
| a number, Enter | Load and start that entry |
| Enter alone | Start the `default` entry, marked `*` |
| `r` | Fetch the manifest again |
| `i` | Show the address, the PROM's memory descriptors and the stack high-water mark |
| `q` | Return to the PROM |

A failed download, a wrong checksum or an image that does not fit in free
memory prints the reason and returns to the menu.

## Manifest

The manifest is a text file of at most 8192 bytes.

```text
timeout = 10
default = irix7
include = os/redox/indyboot.cfg

[irix7]
title  = IRIX 7
image  = os/irix7/irix7
sha256 = 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
size   = 1598392
args   = console=d
```

Top-level keys come before the first section:

| Key | Meaning |
| --- | --- |
| `include = URL` | Append the sections of another manifest. At most 8. An included file cannot include further files. |
| `default = NAME` | Entry started by Enter alone and by the countdown |
| `timeout = SECONDS` | Start `default` after this long without a key. Without it the menu waits. |

Each `[NAME]` section is one menu entry, at most 16 in total. `NAME` is 1 to
32 characters from `A-Z a-z 0-9 . _ -`.

| Key | Required | Meaning |
| --- | --- | --- |
| `image` | yes | URL of the ECOFF image |
| `sha256` | yes | 64 hexadecimal digits; the image is not started when it differs |
| `title` | no | Menu text, at most 60 bytes. Defaults to `NAME`. |
| `size` | no | Image size in bytes, shown in the menu and checked after the download |
| `args` | no | Up to 15 words passed to the image as `argv[1..]`, at most 120 bytes |

URLs are `http://` only, at most 200 bytes. A relative `image` or `include`
is resolved against the URL of the file it appears in. An included file that
fails to download or parse is reported and skipped. Unknown keys inside a
section are ignored.

`manifest/boot.cfg` is the main manifest of the lab server.
`tools/indyboot-entry.sh NAME TITLE IMAGE [ARGS...]` prints a section with the
checksum and size filled in.

## Lab server

The lab's BOOTP and TFTP server is `10.1.1.3` and serves
`/var/lib/sgi-netboot`. indyboot expects the same directory over HTTP on port
80:

| Path | Content |
| --- | --- |
| `indyboot` | `build/indyboot`, fetched by the PROM over TFTP |
| `boot.cfg` | `manifest/boot.cfg` |
| `os/NAME/indyboot.cfg`, `os/NAME/IMAGE` | One directory per operating system, written by that project |

## Test

```sh
cargo test                                      # parsers and the ECOFF loader, on the host
tools/gate.sh IMAGE MARKER [TIMEOUT_SECONDS]    # end to end in the iris emulator
```

`tools/gate.sh` starts an emulated Indy, serves `build/indyboot` over the
emulator's TFTP and `IMAGE` over a local HTTP server, types the boot command
at the PROM, picks the entry and waits for `MARKER` on the serial console.
Its header lists the variants: `CORRUPT=1`, `AUTOBOOT=1`, `QUIT=1`,
`DIRECT=1`, `BOOT_ARGS`, `RAM_MB`, `IRIS_ARGS` and `URL`.

Results on iris 0.1.13+irix7, 17 November 2026:

| Image | Result |
| --- | --- |
| Redox (`redox/build/indy/redox`, 5,517,520 bytes) | Reaches `init: switchroot`; also with `--cpu r5000 --cache full` and with 64 MiB |
| Haiku M0 (`hello.ecoff`, 344 bytes) | Prints `haiku: m0 alive` |
| IRIX 7 (`os/target/netboot/irix7`, 1,598,392 bytes) | Starts and prints `kernel: up`, then traps on a REX3 register because the emulated machine has no graphics. `DIRECT=1` shows the same trap when the PROM loads it without indyboot. |

indyboot has not run on a real Indy yet.

## How it works

1. The PROM loads the ECOFF file at `0x89C00000` and enters it with the ARCS
   firmware vector in place. The operating systems are linked at
   `0x88040000`, so indyboot stays out of that range.
2. Console output and keys go through ARCS `Write` and `Read`, so the menu
   appears on the graphics textport or on serial port 1, wherever the PROM's
   own console is.
3. The Seeq 80C03 Ethernet driver is `irix7-seeq` from
   [irix7/os](https://github.com/irix7/os). smoltcp supplies ARP, IPv4, TCP,
   DHCP and DNS. The driver is polled.
4. The image is streamed from the HTTP body straight to the addresses in its
   ECOFF section headers. Before the first byte is written, every section is
   checked against the PROM's memory descriptors and must lie in free memory.
5. After the SHA-256 matches, indyboot resets the Ethernet channels, calls
   ARCS `FlushAllCaches`, restores the PROM's stack pointer and jumps to the
   entry point with `$a0` = argc, `$a1` = argv and `$a2` = envp.

| Crate | Role |
| --- | --- |
| `core` | Manifest, URL, HTTP head, BOOTP and ECOFF parsing. `no_std`, tested on the host. |
| `loader` | The program the PROM loads: ARCS calls, console, clock, network, menu, handoff. |
| `tools/mkecoff` | Converts the loader's ELF to ECOFF. |

## Limits

- 32 MiB of RAM or more. The PROM refuses to load indyboot at `0x89C00000`
  on a smaller machine.
- Plain HTTP only. `https://` URLs are rejected with a message, because the
  loader has no TLS. The SHA-256 detects a damaged download. It does not
  authenticate the image, since the manifest travels over the same
  unauthenticated connection.
- ECOFF images only.
- An image, including its `.bss`, has to fit in one of the PROM's free
  ranges. On a 32 MiB or 64 MiB Indy the range holding `0x88040000` ends at
  `0x88740000`, which allows 7 MiB.
- The TCP receive window is 10 KB, set by the eight-descriptor receive ring.
  The Redox gate (5.5 MB) takes 40 s in iris from emulator start to
  `init: switchroot`.

## Licence

GPL-3.0-only, the licence of the `irix7-seeq` driver it links. See `LICENCE`.
The images indyboot starts are separate programs under their own licences.

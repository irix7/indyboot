# Add an operating system to indyboot

An operating system appears in the indyboot menu when three things exist:

1. An ECOFF image that the Indy PROM can start with `boot -f bootp()NAME`.
2. That image and an `indyboot.cfg` fragment, served over plain HTTP.
3. One `include` line in the main manifest.

A project that already netboots from the factory PROM has the first item and
needs no code change.

## 1. The image

indyboot starts an image the way the PROM does. The image has to meet the
PROM's contract:

| Item | Requirement |
| --- | --- |
| Format | Big-endian MIPS ECOFF: `f_magic` 0x0160, a 56-byte a.out header, 1 to 8 sections |
| Sections | Each section with file bytes is copied to its `s_vaddr`. `.bss` (flag 0x80) and `.sbss` (flag 0x400) sections are zeroed. |
| Addresses | kseg0 or kseg1. Every section, `.bss` included, lies in memory the PROM reports as free. |
| Entry | The a.out `entry` field, inside a section that has file bytes |
| One file | Everything the system needs at boot is in the image. indyboot loads one file per entry. |

Free memory on a 32 MiB Indy, as the PROM reports it under indyboot:

| Physical range | State |
| --- | --- |
| `0x08002000..0x08740000` | Free. Images linked at `0x88040000` load here: 7 MiB up to `0x88740000`. |
| `0x08740000..0x08800000` | PROM (firmware temporary). An image cannot cross it. |
| `0x08800000..0x09BE0000` | Free |
| `0x09BE0000..0x09D32000` | indyboot and its Ethernet buffers |
| `0x09D32000..0x0A000000` | Free |

A 64 MiB Indy has the same map with the last range ending at `0x0C000000`.
indyboot's range is free again once your image runs. The PROM's descriptor
list still shows `0x09C00000..0x09D32000` as type 5, loaded program.

State at your entry point:

| Item | Value |
| --- | --- |
| `$a0`, `$a1` | argc and argv. `argv[0]` is the image URL. `argv[1..]` are the words of `args` in the manifest. |
| `$a2` | The envp the PROM gave indyboot |
| `$sp` | The PROM's stack |
| CP0 Status | As the PROM left it: kernel mode, interrupts off. indyboot does not write it. |
| Caches | Flushed with ARCS `FlushAllCaches` after the image was written |
| ARCS | The parameter block at `0xA0001000` and the firmware vector are usable |
| Ethernet | The Seeq and both HPC3 Ethernet DMA channels are reset |
| `$ra` | Returning from the entry point calls ARCS `EnterInteractiveMode` |

Differences from a direct PROM boot:

- The PROM variable `netaddr` and the BOOTP reply are those of the indyboot
  load, which is the same server and address in practice.
- `argv` holds the manifest's words, in place of the PROM's `OSLoadOptions`
  style arguments.
- The descriptor for your own image is type 3 (free) where a direct boot
  shows type 5. Reserve your image range yourself.

Check the image without indyboot first:

```text
>> boot -f bootp()NAME
```

Then check it through indyboot in the iris emulator, from an indyboot
checkout:

```sh
./build.sh
tools/gate.sh /path/to/your/image 'text your system prints on the serial console'
```

`DIRECT=1 tools/gate.sh ...` boots the same image straight from the PROM, to
tell an image problem from a bootloader problem.

## 2. Publish the image and a fragment

Put the image and a file named `indyboot.cfg` in one directory on an HTTP
server. On the lab server that directory is `/var/lib/sgi-netboot/NAME/`,
served as `http://10.1.1.3/NAME/`.

`tools/indyboot-entry.sh` writes the fragment:

```sh
tools/indyboot-entry.sh redox "Redox OS" build/indy/redox > indyboot.cfg
install -D -m 0644 build/indy/redox /var/lib/sgi-netboot/redox/redox
install -D -m 0644 indyboot.cfg /var/lib/sgi-netboot/redox/indyboot.cfg
```

```text
[redox]
title = Redox OS
image = redox
sha256 = a919ea5c52f41725237ea607b8041715dc4b8ccc8e0652adf5d2245ca9dc9fa3
size = 5517520
```

Rules for the fragment:

- `image` and `sha256` are required. Regenerate the fragment every time the
  image changes; indyboot refuses an image whose SHA-256 differs.
- A relative `image` is resolved against the fragment's own URL, so the pair
  can move to another server together.
- Write the image first and the fragment second. A boot between the two
  writes then fails the checksum and returns to the menu.
- A fragment may hold several sections, for example a release and a debug
  build. The menu has room for 16 entries across all projects.
- Section names are shared by all projects. Prefix yours with the project
  name: `redox`, `redox-debug`.
- The server has to answer an HTTP/1.0 `GET` with a `200` and the file. A
  redirect to another `http://` URL is followed, up to four times.
  `https://` is not supported.

Run the entry script from your build, next to the step that installs the
image for PROM netboot, so the two cannot drift apart.

## 3. Add the include line

Open a pull request against `manifest/boot.cfg` in
[irix7/indyboot](https://github.com/irix7/indyboot) that adds:

```text
include = NAME/indyboot.cfg
```

`irix7`, `redox` and `haiku` are already listed. A fragment that is missing
or invalid is reported at boot and skipped, so a listed project can publish
later.

To try a fragment before it is listed, boot with it as the manifest:

```text
>> boot -f bootp()indyboot url=http://10.1.1.3/NAME/indyboot.cfg
```

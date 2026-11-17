/* indyboot image layout.
 *
 * The PROM loads every netboot image at the address in its ECOFF header. The
 * operating systems indyboot starts are linked at 0x88040000, so indyboot
 * sits at 0x89C00000 (physical 28 MiB) and leaves that range free. A 32 MiB
 * Indy ends at 0x8A000000.
 *
 * The 128 KiB below the image, 0x89BE0000..0x89C00000, hold the Ethernet DMA
 * buffers. They are reached through kseg1 only; see loader/src/net.rs.
 */
OUTPUT_FORMAT("elf32-tradbigmips")
ENTRY(_start)

PHDRS
{
    text PT_LOAD FLAGS(5);
    data PT_LOAD FLAGS(6);
}

SECTIONS
{
    . = 0x89C00000;
    __image_start = .;

    .text : {
        KEEP(*(.text._start))
        *(.text .text.*)
    } :text
    .rodata : {
        *(.rodata .rodata.*)
        . = ALIGN(16);
    } :text

    .data : {
        *(.data .data.*)
        *(.sdata .sdata.*)
        /* Keeps the data segment non-empty so the ECOFF has a .data. */
        LONG(0)
        . = ALIGN(16);
    } :data

    .bss (NOLOAD) : {
        __bss_start = .;
        *(.sbss .sbss.*)
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    } :data

    /* 1 MiB. The deepest path measured in iris uses the amount printed by
       the menu's `i` command; smoltcp's interface and the manifest are the
       large frames. */
    .stack (NOLOAD) : {
        __stack_bottom = .;
        . += 0x100000;
        __stack_top = .;
    } :data
    __image_end = .;

    ASSERT(__image_end <= 0x8A000000, "indyboot does not fit below 32 MiB")

    /DISCARD/ : {
        *(.MIPS.abiflags)
        *(.reginfo)
        *(.eh_frame .eh_frame_hdr)
        *(.comment)
        *(.note .note.*)
        *(.pdr)
        *(.gnu.attributes)
        *(.mdebug.*)
    }
}

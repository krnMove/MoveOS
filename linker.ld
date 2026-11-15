ENTRY(_start)

SECTIONS
{
    . = 0x40080000;

    .text : {
        *(.text._start)
        *(.text .text.*)
    }

    .rodata : {
        *(.rodata .rodata.*)
    }

    .user_text ALIGN(0x200000) : {
        __user_text_start = .;
        KEEP(*(.user.text))
        . = ALIGN(0x1000);
        __user_text_end = .;
    }

    .data : {
        *(.data .data.*)
    }

    .bss (NOLOAD) : {
        . = ALIGN(16);
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(0x200000);
        __user_data_start = .;
        KEEP(*(.user.data))
        . = ALIGN(0x1000);
        __user_data_end = .;
        . = ALIGN(16);
        __bss_end = .;
    }
}
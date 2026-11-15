use core::arch::asm;
use core::fmt;
use core::ptr::{addr_of, addr_of_mut};

const TABLE_ENTRIES: usize = 512;
const BLOCK_SIZE: usize = 2 * 1024 * 1024;
const PAGE_SIZE: usize = 4096;
const RAM_BASE: usize = 0x4000_0000;
const RAM_SIZE: usize = 0x4000_0000;
const GIC_MMIO_BASE: usize = 0x0800_0000;
const VIRTIO_MMIO_BASE: usize = 0x0a00_0000;
const TABLE_DESCRIPTOR: u64 = 0b11;
const BLOCK_DESCRIPTOR: u64 = 0b01;
const ACCESS_FLAG: u64 = 1 << 10;
const INNER_SHAREABLE: u64 = 0b11 << 8;
const DEVICE_ATTRIBUTE_INDEX: u64 = 1 << 2;
const EXECUTE_NEVER: u64 = (1 << 53) | (1 << 54);
const PRIVILEGED_EXECUTE_NEVER: u64 = 1 << 53;
const ACCESS_USER_READ_WRITE: u64 = 0b01 << 6;
const ACCESS_USER_READ_ONLY: u64 = 0b11 << 6;
const PAGE_DESCRIPTOR: u64 = 0b11;

#[repr(C, align(4096))]
struct PageTable([u64; TABLE_ENTRIES]);

static mut LEVEL1: PageTable = PageTable([0; TABLE_ENTRIES]);
static mut LOW_MEMORY: PageTable = PageTable([0; TABLE_ENTRIES]);
static mut HIGH_MEMORY: PageTable = PageTable([0; TABLE_ENTRIES]);
static mut USER_TEXT_PAGES: PageTable = PageTable([0; TABLE_ENTRIES]);
static mut USER_DATA_PAGES: PageTable = PageTable([0; TABLE_ENTRIES]);

unsafe extern "C" {
    static __user_text_start: u8;
    static __user_text_end: u8;
    static __user_data_start: u8;
    static __user_data_end: u8;
}

#[derive(Clone, Copy, Debug)]
pub enum Error {
    UnsupportedAddressLayout,
    UnsupportedUserRegion,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedAddressLayout => {
                formatter.write_str("RAM or device address exceeds the initial page tables")
            }
            Self::UnsupportedUserRegion => {
                formatter.write_str("user text or data does not fit in its isolated page table")
            }
        }
    }
}

fn block_descriptor(address: usize, attributes: u64) -> u64 {
    (address as u64 & 0x0000_ffff_ffe0_0000) | attributes | BLOCK_DESCRIPTOR
}

fn map_identity_region(
    table: &mut PageTable,
    table_offset: usize,
    physical_base: usize,
    length: usize,
    attributes: u64,
) {
    let first_entry = table_offset / BLOCK_SIZE;
    let entry_count = length / BLOCK_SIZE;
    for index in 0..entry_count {
        table.0[first_entry + index] =
            block_descriptor(physical_base + index * BLOCK_SIZE, attributes);
    }
}

fn map_user_region(
    high_memory: &mut PageTable,
    pages: &mut PageTable,
    start: usize,
    end: usize,
    user_attributes: u64,
) -> Result<(), Error> {
    if start < RAM_BASE
        || end <= start
        || end > RAM_BASE + RAM_SIZE
        || start % PAGE_SIZE != 0
        || end % PAGE_SIZE != 0
    {
        return Err(Error::UnsupportedUserRegion);
    }

    let first_block = (start - RAM_BASE) / BLOCK_SIZE;
    let last_block = (end - 1 - RAM_BASE) / BLOCK_SIZE;
    if first_block != last_block {
        return Err(Error::UnsupportedUserRegion);
    }

    let block_base = RAM_BASE + first_block * BLOCK_SIZE;
    let normal_attributes = ACCESS_FLAG | INNER_SHAREABLE;
    for index in 0..TABLE_ENTRIES {
        let address = block_base + index * PAGE_SIZE;
        pages.0[index] = (address as u64 & 0x0000_ffff_ffff_f000)
            | normal_attributes
            | PAGE_DESCRIPTOR
            | EXECUTE_NEVER;
    }

    for address in (start..end).step_by(PAGE_SIZE) {
        let index = (address - block_base) / PAGE_SIZE;
        pages.0[index] = (address as u64 & 0x0000_ffff_ffff_f000)
            | normal_attributes
            | PAGE_DESCRIPTOR
            | user_attributes;
    }

    high_memory.0[first_block] = (pages as *mut PageTable as u64) | TABLE_DESCRIPTOR;
    Ok(())
}

pub fn init() -> Result<(), Error> {
    let ram_end = RAM_BASE
        .checked_add(RAM_SIZE)
        .ok_or(Error::UnsupportedAddressLayout)?;
    if RAM_BASE % BLOCK_SIZE != 0
        || RAM_SIZE % BLOCK_SIZE != 0
        || ram_end > 0x8000_0000
        || VIRTIO_MMIO_BASE / (1 << 30) != 0
        || GIC_MMIO_BASE / (1 << 30) != 0
    {
        return Err(Error::UnsupportedAddressLayout);
    }

    unsafe {
        let level1 = addr_of_mut!(LEVEL1.0) as *mut u64;
        let low_memory = addr_of_mut!(LOW_MEMORY.0) as *mut u64;
        let high_memory = addr_of_mut!(HIGH_MEMORY.0) as *mut u64;

        for index in 0..TABLE_ENTRIES {
            level1.add(index).write_volatile(0);
            low_memory.add(index).write_volatile(0);
            high_memory.add(index).write_volatile(0);
        }

        level1
            .add(0)
            .write_volatile(low_memory as u64 | TABLE_DESCRIPTOR);
        level1
            .add(1)
            .write_volatile(high_memory as u64 | TABLE_DESCRIPTOR);

        let device_attributes = ACCESS_FLAG | DEVICE_ATTRIBUTE_INDEX | EXECUTE_NEVER;
        map_identity_region(
            &mut *addr_of_mut!(LOW_MEMORY),
            GIC_MMIO_BASE,
            GIC_MMIO_BASE,
            BLOCK_SIZE,
            device_attributes,
        );
        map_identity_region(
            &mut *addr_of_mut!(LOW_MEMORY),
            VIRTIO_MMIO_BASE,
            VIRTIO_MMIO_BASE,
            BLOCK_SIZE,
            device_attributes,
        );

        let normal_attributes = ACCESS_FLAG | INNER_SHAREABLE;
        map_identity_region(
            &mut *addr_of_mut!(HIGH_MEMORY),
            0,
            RAM_BASE,
            RAM_SIZE,
            normal_attributes,
        );

        map_user_region(
            &mut *addr_of_mut!(HIGH_MEMORY),
            &mut *addr_of_mut!(USER_TEXT_PAGES),
            addr_of!(__user_text_start) as usize,
            addr_of!(__user_text_end) as usize,
            ACCESS_USER_READ_ONLY | PRIVILEGED_EXECUTE_NEVER,
        )?;
        map_user_region(
            &mut *addr_of_mut!(HIGH_MEMORY),
            &mut *addr_of_mut!(USER_DATA_PAGES),
            addr_of!(__user_data_start) as usize,
            addr_of!(__user_data_end) as usize,
            ACCESS_USER_READ_WRITE | EXECUTE_NEVER,
        )?;

        let mair_el1 = 0x00ffu64;
        let tcr_el1 = 25u64
            | (1 << 8)
            | (1 << 10)
            | (0b11 << 12)
            | (25 << 16)
            | (1 << 23)
            | (1 << 24)
            | (1 << 26)
            | (0b11 << 28)
            | (0b10 << 30)
            | (0b10 << 32);
        asm!(
            "dsb ishst",
            "msr mair_el1, {mair}",
            "msr tcr_el1, {tcr}",
            "msr ttbr0_el1, {ttbr}",
            "dsb ish",
            "tlbi vmalle1",
            "dsb ish",
            "isb",
            mair = in(reg) mair_el1,
            tcr = in(reg) tcr_el1,
            ttbr = in(reg) level1 as u64,
            options(nostack, preserves_flags)
        );

        let mut sctlr_el1: u64;
        asm!("mrs {value}, sctlr_el1", value = out(reg) sctlr_el1, options(nomem, nostack, preserves_flags));
        sctlr_el1 |= 1 | (1 << 2) | (1 << 12);
        asm!(
            "msr sctlr_el1, {value}",
            "isb",
            value = in(reg) sctlr_el1,
            options(nostack, preserves_flags)
        );
    }

    Ok(())
}

pub fn is_enabled() -> bool {
    let sctlr_el1: u64;
    unsafe {
        asm!(
            "mrs {value}, sctlr_el1",
            value = out(reg) sctlr_el1,
            options(nomem, nostack, preserves_flags)
        );
    }
    sctlr_el1 & 1 != 0
}

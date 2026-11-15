use core::ptr::{addr_of, addr_of_mut};

use crate::{fat32::Fat32, virtio_keyboard::Key};

const SYS_WRITE_TEXT: u64 = 1;
const SYS_READ_KEY: u64 = 2;
const SYS_CLEAR: u64 = 3;
const SYS_BACKSPACE: u64 = 4;
const SYS_LIST_FILES: u64 = 5;
const SYS_READ_FILE: u64 = 6;
const SYS_WRITE_FILE: u64 = 7;
const SYS_EXIT: u64 = 8;
const MAX_TEXT_BYTES: usize = 512;
const MAX_FILE_BYTES: usize = 1024;
const KEY_ENTER: u64 = 256;
const KEY_BACKSPACE: u64 = 257;
const KEY_ESCAPE: u64 = 258;

#[repr(C)]
struct UserExceptionFrame {
    x: [u64; 19],
    fp: u64,
    lr: u64,
    reserved: u64,
    q: [u128; 32],
    fpcr: u64,
    fpsr: u64,
}

const _: [(); 704] = [(); core::mem::size_of::<UserExceptionFrame>()];

static mut VOLUME: Option<Fat32> = None;

unsafe extern "C" {
    static __user_text_start: u8;
    static __user_text_end: u8;
    static __user_data_start: u8;
    static __user_data_end: u8;
}

pub fn set_volume(volume: Option<Fat32>) {
    unsafe { addr_of_mut!(VOLUME).write(volume) };
}

#[unsafe(no_mangle)]
extern "C" fn syscall_dispatch(frame: *mut UserExceptionFrame) {
    let frame = unsafe { &mut *frame };
    let syndrome: u64;
    unsafe {
        core::arch::asm!(
            "mrs {value}, esr_el1",
            value = out(reg) syndrome,
            options(nomem, nostack, preserves_flags)
        );
    }

    if syndrome >> 26 & 0x3f != 0x15 || syndrome & 0xffff != 0 {
        crate::println!("Terminal exited after an unsupported user-mode exception")
            .unwrap_or_else(|_| panic!("virtio-gpu output failed"));
        return_to_kernel(frame);
        return;
    }

    frame.x[0] = match frame.x[8] {
        SYS_WRITE_TEXT => syscall_write_text(frame.x[0], frame.x[1]),
        SYS_READ_KEY => syscall_read_key(),
        SYS_CLEAR => syscall_clear(),
        SYS_BACKSPACE => syscall_backspace(),
        SYS_LIST_FILES => syscall_list_files(),
        SYS_READ_FILE => syscall_read_file(frame.x[0], frame.x[1]),
        SYS_WRITE_FILE => syscall_write_file(frame.x[0], frame.x[1], frame.x[2], frame.x[3]),
        SYS_EXIT => {
            return_to_kernel(frame);
            0
        }
        _ => u64::MAX,
    };
}

fn return_to_kernel(frame: &mut UserExceptionFrame) {
    unsafe {
        core::arch::asm!(
            "msr elr_el1, {return_address}",
            "msr spsr_el1, {kernel_mode}",
            return_address = in(reg) crate::user_shell::return_address(),
            kernel_mode = in(reg) 0b0101u64,
            options(nomem, nostack, preserves_flags)
        );
    }
    frame.x[0] = 0;
}

fn user_input(pointer: u64, length: u64) -> Option<&'static [u8]> {
    let pointer = usize::try_from(pointer).ok()?;
    let length = usize::try_from(length).ok()?;
    let end = pointer.checked_add(length)?;
    let data_start = addr_of!(__user_data_start) as usize;
    let data_end = addr_of!(__user_data_end) as usize;
    let text_start = addr_of!(__user_text_start) as usize;
    let text_end = addr_of!(__user_text_end) as usize;
    let in_user_data = pointer >= data_start && end <= data_end;
    let in_user_text = pointer >= text_start && end <= text_end;
    if !in_user_data && !in_user_text {
        return None;
    }
    Some(unsafe { core::slice::from_raw_parts(pointer as *const u8, length) })
}

fn mounted_volume() -> Option<Fat32> {
    unsafe { *addr_of!(VOLUME) }
}

fn syscall_write_text(pointer: u64, length: u64) -> u64 {
    let Some(bytes) = user_input(pointer, length) else {
        return u64::MAX;
    };
    if bytes.len() > MAX_TEXT_BYTES {
        return u64::MAX;
    }
    let Ok(text) = core::str::from_utf8(bytes) else {
        return u64::MAX;
    };
    crate::console::print(core::format_args!("{text}"))
        .unwrap_or_else(|_| panic!("virtio-gpu output failed"));
    0
}

fn syscall_read_key() -> u64 {
    loop {
        crate::scheduler::run_one();
        match crate::virtio_keyboard::poll_key() {
            Ok(Some(Key::Character(character))) if character.is_ascii() => {
                return character as u8 as u64;
            }
            Ok(Some(Key::Enter)) => return KEY_ENTER,
            Ok(Some(Key::Backspace)) => return KEY_BACKSPACE,
            Ok(Some(Key::Escape)) => return KEY_ESCAPE,
            Ok(_) => core::hint::spin_loop(),
            Err(error) => {
                crate::println!("Keyboard error: {}", error)
                    .unwrap_or_else(|_| panic!("virtio-gpu output failed"));
                return u64::MAX;
            }
        }
    }
}

fn syscall_clear() -> u64 {
    crate::console::clear().unwrap_or_else(|_| panic!("virtio-gpu clear failed"));
    0
}

fn syscall_backspace() -> u64 {
    crate::console::backspace().unwrap_or_else(|_| panic!("virtio-gpu output failed"));
    0
}

fn syscall_list_files() -> u64 {
    let Some(volume) = mounted_volume() else {
        crate::println!("LS: no FAT32 volume mounted")
            .unwrap_or_else(|_| panic!("virtio-gpu output failed"));
        return 1;
    };
    crate::println!("FILES:").unwrap_or_else(|_| panic!("virtio-gpu output failed"));
    if let Err(error) = volume.list_root(|entry| {
        crate::println!(
            "- {}{}",
            entry.name(),
            if entry.is_directory() { "/" } else { "" }
        )
        .unwrap_or_else(|_| panic!("virtio-gpu output failed"));
        true
    }) {
        crate::println!("LS: {}", error).unwrap_or_else(|_| panic!("virtio-gpu output failed"));
        return 1;
    }
    0
}

fn syscall_read_file(name_pointer: u64, name_length: u64) -> u64 {
    let Some(name) = user_input(name_pointer, name_length) else {
        return u64::MAX;
    };
    let Ok(name) = core::str::from_utf8(name) else {
        return u64::MAX;
    };
    let Some(volume) = mounted_volume() else {
        crate::println!("CAT: no FAT32 volume mounted")
            .unwrap_or_else(|_| panic!("virtio-gpu output failed"));
        return 1;
    };

    let mut contents = [0; MAX_FILE_BYTES];
    match volume.read_file_83(name, &mut contents) {
        Ok(length) => match core::str::from_utf8(&contents[..length]) {
            Ok(text) => crate::console::print(core::format_args!("{text}"))
                .unwrap_or_else(|_| panic!("virtio-gpu output failed")),
            Err(_) => {
                crate::println!("CAT: file is not UTF-8 text")
                    .unwrap_or_else(|_| panic!("virtio-gpu output failed"));
                return 1;
            }
        },
        Err(error) => {
            crate::println!("CAT: {}", error)
                .unwrap_or_else(|_| panic!("virtio-gpu output failed"));
            return 1;
        }
    }
    0
}

fn syscall_write_file(
    name_pointer: u64,
    name_length: u64,
    contents_pointer: u64,
    contents_length: u64,
) -> u64 {
    let (Some(name), Some(contents)) = (
        user_input(name_pointer, name_length),
        user_input(contents_pointer, contents_length),
    ) else {
        return u64::MAX;
    };
    if contents.len() > MAX_FILE_BYTES {
        crate::println!("WRITE: text is limited to {} bytes", MAX_FILE_BYTES)
            .unwrap_or_else(|_| panic!("virtio-gpu output failed"));
        return 1;
    }
    let Ok(name) = core::str::from_utf8(name) else {
        return u64::MAX;
    };
    let Some(volume) = mounted_volume() else {
        crate::println!("WRITE: no FAT32 volume mounted")
            .unwrap_or_else(|_| panic!("virtio-gpu output failed"));
        return 1;
    };
    match volume.write_file_83(name, contents) {
        Ok(()) => {
            crate::println!("wrote {} bytes to {}", contents.len(), name)
                .unwrap_or_else(|_| panic!("virtio-gpu output failed"));
            0
        }
        Err(error) => {
            crate::println!("WRITE: {}", error)
                .unwrap_or_else(|_| panic!("virtio-gpu output failed"));
            1
        }
    }
}

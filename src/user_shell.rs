use core::arch::asm;
use core::ptr::addr_of_mut;

const COMMAND_CAPACITY: usize = 256;
const USER_STACK_SIZE: usize = 16 * 1024;
const KEY_ENTER: u64 = 256;
const KEY_BACKSPACE: u64 = 257;
const KEY_ESCAPE: u64 = 258;
const SYS_WRITE_TEXT: u64 = 1;
const SYS_READ_KEY: u64 = 2;
const SYS_CLEAR: u64 = 3;
const SYS_BACKSPACE: u64 = 4;
const SYS_LIST_FILES: u64 = 5;
const SYS_READ_FILE: u64 = 6;
const SYS_WRITE_FILE: u64 = 7;
const SYS_EXIT: u64 = 8;

macro_rules! user_bytes {
    ($name:ident, $value:literal) => {
        #[used]
        #[unsafe(link_section = ".user.text")]
        static $name: [u8; $value.len()] = *$value;
    };
}

user_bytes!(HELP, b"HELP");
user_bytes!(
    HELP_TEXT,
    b"COMMANDS: HELP, LS, CAT <NAME.EXT>, WRITE <NAME.EXT> <TEXT>, CLEAR, EXIT\n"
);
user_bytes!(PROMPT, b"MoveOS> ");
user_bytes!(NEWLINE, b"\n");
user_bytes!(USAGE_CAT, b"Usage: CAT <NAME.EXT>\n");
user_bytes!(USAGE_WRITE, b"Usage: WRITE <NAME.EXT> <TEXT>\n");
user_bytes!(UNKNOWN, b"Unknown command. Type HELP.\n");
user_bytes!(TOO_LONG, b"Command is too long.\n");
user_bytes!(LS, b"LS");
user_bytes!(CAT, b"CAT");
user_bytes!(WRITE, b"WRITE");
user_bytes!(CLEAR, b"CLEAR");
user_bytes!(EXIT, b"EXIT");
user_bytes!(QUIT, b"QUIT");

#[unsafe(link_section = ".user.data")]
static mut COMMAND: [u8; COMMAND_CAPACITY] = [0; COMMAND_CAPACITY];

#[unsafe(link_section = ".user.data")]
static mut USER_STACK: [u8; USER_STACK_SIZE] = [0; USER_STACK_SIZE];

unsafe extern "C" {
    fn enter_user(entry: usize, stack_top: usize);
    fn user_shell_return();
}

pub fn launch() {
    let stack_start = addr_of_mut!(USER_STACK) as usize;
    let stack_top = (stack_start + USER_STACK_SIZE) & !0xf;
    unsafe {
        enter_user(user_shell_entry as *const () as usize, stack_top);
    }
}

pub fn return_address() -> u64 {
    user_shell_return as *const () as usize as u64
}

#[inline(never)]
#[unsafe(link_section = ".user.text")]
#[unsafe(no_mangle)]
pub extern "C" fn user_shell_entry() -> ! {
    let mut length = 0;
    let mut overflowed = false;
    write_bytes(&PROMPT);

    loop {
        match syscall(SYS_READ_KEY, 0, 0, 0, 0) {
            KEY_ENTER => {
                write_bytes(&NEWLINE);
                if execute_command(length) {
                    length = 0;
                    overflowed = false;
                    write_bytes(&PROMPT);
                }
            }
            KEY_BACKSPACE if length > 0 => {
                length -= 1;
                overflowed = false;
                unsafe {
                    addr_of_mut!(COMMAND)
                        .cast::<u8>()
                        .add(length)
                        .write_volatile(0);
                }
                syscall(SYS_BACKSPACE, 0, 0, 0, 0);
            }
            KEY_ESCAPE => exit_user(),
            character if character <= u8::MAX as u64 && length < COMMAND_CAPACITY - 1 => {
                let byte = character as u8;
                unsafe {
                    addr_of_mut!(COMMAND)
                        .cast::<u8>()
                        .add(length)
                        .write_volatile(byte);
                }
                length += 1;
                write_bytes(core::slice::from_ref(&byte));
            }
            character if character <= u8::MAX as u64 && !overflowed => {
                write_bytes(&TOO_LONG);
                overflowed = true;
            }
            _ => {}
        }
    }
}

#[inline(never)]
#[unsafe(link_section = ".user.text")]
fn execute_command(length: usize) -> bool {
    let command =
        unsafe { core::slice::from_raw_parts(addr_of_mut!(COMMAND).cast::<u8>(), length) };
    let mut start = 0;
    while start < command.len() && command[start].is_ascii_whitespace() {
        start += 1;
    }
    if start < command.len() && command[start] == b'/' {
        start += 1;
    }
    let command = &command[start..];
    let verb_end = command
        .iter()
        .position(u8::is_ascii_whitespace)
        .unwrap_or(command.len());
    let verb = &command[..verb_end];
    let arguments = command[verb_end..].trim_ascii_start();

    if equals_ignore_ascii_case(verb, &HELP) {
        write_bytes(&HELP_TEXT);
    } else if equals_ignore_ascii_case(verb, &LS) {
        syscall(SYS_LIST_FILES, 0, 0, 0, 0);
    } else if equals_ignore_ascii_case(verb, &CAT) {
        if let Some((name, _)) = first_argument(arguments) {
            syscall(SYS_READ_FILE, name.as_ptr() as u64, name.len() as u64, 0, 0);
        } else {
            write_bytes(&USAGE_CAT);
        }
    } else if equals_ignore_ascii_case(verb, &WRITE) {
        if let Some((name, contents)) = first_argument(arguments) {
            syscall(
                SYS_WRITE_FILE,
                name.as_ptr() as u64,
                name.len() as u64,
                contents.as_ptr() as u64,
                contents.len() as u64,
            );
        } else {
            write_bytes(&USAGE_WRITE);
        }
    } else if equals_ignore_ascii_case(verb, &CLEAR) {
        syscall(SYS_CLEAR, 0, 0, 0, 0);
    } else if equals_ignore_ascii_case(verb, &EXIT) || equals_ignore_ascii_case(verb, &QUIT) {
        exit_user();
    } else if !verb.is_empty() {
        write_bytes(&UNKNOWN);
    }
    true
}

#[inline(never)]
#[unsafe(link_section = ".user.text")]
fn first_argument(input: &[u8]) -> Option<(&[u8], &[u8])> {
    let input = input.trim_ascii_start();
    if input.is_empty() {
        return None;
    }
    let split = input
        .iter()
        .position(u8::is_ascii_whitespace)
        .unwrap_or(input.len());
    let name = &input[..split];
    let contents = input[split..].trim_ascii_start();
    Some((name, contents))
}

#[inline(never)]
#[unsafe(link_section = ".user.text")]
fn equals_ignore_ascii_case(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
}

#[inline(never)]
#[unsafe(link_section = ".user.text")]
fn write_bytes(bytes: &[u8]) {
    syscall(
        SYS_WRITE_TEXT,
        bytes.as_ptr() as u64,
        bytes.len() as u64,
        0,
        0,
    );
}

#[inline(never)]
#[unsafe(link_section = ".user.text")]
fn syscall(number: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64) -> u64 {
    let mut result = arg0;
    unsafe {
        asm!(
            "svc #0",
            in("x8") number,
            inlateout("x0") result,
            in("x1") arg1,
            in("x2") arg2,
            in("x3") arg3,
            options(nostack)
        );
    }
    result
}

#[inline(never)]
#[unsafe(link_section = ".user.text")]
fn exit_user() -> ! {
    syscall(SYS_EXIT, 0, 0, 0, 0);
    loop {
        unsafe {
            asm!("wfe", options(nomem, nostack, preserves_flags));
        }
    }
}

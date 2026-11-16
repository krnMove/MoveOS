use core::fmt;
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use core::sync::atomic::{Ordering, fence};

const MMIO_BASE: usize = 0x0a00_0000;
const MMIO_STRIDE: usize = 0x200;
const MMIO_SLOTS: usize = 32;
const DEVICE_MAGIC: u32 = 0x7472_6976;
const DEVICE_INPUT: u32 = 18;
const FEATURE_VERSION_1: u32 = 1;
const QUEUE_SIZE: usize = 8;
const MAX_DEVICES: usize = 4;
const EVENT_SYN: u16 = 0;
const EVENT_KEY: u16 = 1;
const EVENT_ABS: u16 = 3;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const BTN_LEFT: u16 = 272;
const BTN_TOUCH: u16 = 330;

const REG_MAGIC: usize = 0x000;
const REG_VERSION: usize = 0x004;
const REG_DEVICE_ID: usize = 0x008;
const REG_DEVICE_FEATURES: usize = 0x010;
const REG_DEVICE_FEATURES_SEL: usize = 0x014;
const REG_DRIVER_FEATURES: usize = 0x020;
const REG_DRIVER_FEATURES_SEL: usize = 0x024;
const REG_QUEUE_SEL: usize = 0x030;
const REG_QUEUE_NUM_MAX: usize = 0x034;
const REG_QUEUE_NUM: usize = 0x038;
const REG_QUEUE_READY: usize = 0x044;
const REG_QUEUE_NOTIFY: usize = 0x050;
const REG_STATUS: usize = 0x070;
const REG_QUEUE_DESC_LOW: usize = 0x080;
const REG_QUEUE_DESC_HIGH: usize = 0x084;
const REG_QUEUE_DRIVER_LOW: usize = 0x090;
const REG_QUEUE_DRIVER_HIGH: usize = 0x094;
const REG_QUEUE_DEVICE_LOW: usize = 0x0a0;
const REG_QUEUE_DEVICE_HIGH: usize = 0x0a4;
const REG_CONFIG: usize = 0x100;

const DESCRIPTOR_F_WRITE: u16 = 2;

#[derive(Clone, Copy, Debug)]
pub enum Error {
    DeviceNotFound,
    UnsupportedVersion,
    MissingVersionFeature,
    FeatureNegotiationFailed,
    QueueUnavailable,
    InvalidUsedDescriptor(u32),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeviceNotFound => formatter.write_str("virtio input device not found"),
            Self::UnsupportedVersion => {
                formatter.write_str("unsupported virtio input transport version")
            }
            Self::MissingVersionFeature => formatter.write_str("virtio input lacks VERSION_1"),
            Self::FeatureNegotiationFailed => {
                formatter.write_str("virtio input feature negotiation failed")
            }
            Self::QueueUnavailable => formatter.write_str("virtio input event queue unavailable"),
            Self::InvalidUsedDescriptor(id) => {
                write!(formatter, "invalid virtio input event descriptor {id}")
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Key {
    Character(char),
    Enter,
    Backspace,
    Escape,
    Up,
    Down,
    Save,
}

#[derive(Clone, Copy, Debug)]
pub enum Input {
    Key(Key),
    Pointer { x: u16, y: u16 },
    Touch { x: u16, y: u16, pressed: bool },
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Descriptor {
    address: u64,
    length: u32,
    flags: u16,
    next: u16,
}

impl Descriptor {
    const EMPTY: Self = Self {
        address: 0,
        length: 0,
        flags: 0,
        next: 0,
    };
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UsedElement {
    id: u32,
    length: u32,
}

#[repr(C, align(16))]
struct DescriptorTable([Descriptor; QUEUE_SIZE]);

#[repr(C, align(2))]
struct AvailableRing {
    flags: u16,
    index: u16,
    ring: [u16; QUEUE_SIZE],
    used_event: u16,
}

#[repr(C, align(4))]
struct UsedRing {
    flags: u16,
    index: u16,
    ring: [UsedElement; QUEUE_SIZE],
    available_event: u16,
}

#[repr(C, align(8))]
#[derive(Clone, Copy)]
struct InputEvent {
    event_type: u16,
    code: u16,
    value: u32,
}

impl InputEvent {
    const EMPTY: Self = Self {
        event_type: 0,
        code: 0,
        value: 0,
    };
}

#[repr(C, align(4096))]
struct InputQueue {
    descriptors: DescriptorTable,
    available: AvailableRing,
    used: UsedRing,
    events: [InputEvent; QUEUE_SIZE],
    base: usize,
    available_index: u16,
    last_used_index: u16,
    abs_x_min: i32,
    abs_x_max: i32,
    abs_y_min: i32,
    abs_y_max: i32,
    touch_x: i32,
    touch_y: i32,
    touch_pressed: bool,
    pointer_dirty: bool,
}

impl InputQueue {
    const fn new() -> Self {
        Self {
            descriptors: DescriptorTable([Descriptor::EMPTY; QUEUE_SIZE]),
            available: AvailableRing {
                flags: 0,
                index: 0,
                ring: [0; QUEUE_SIZE],
                used_event: 0,
            },
            used: UsedRing {
                flags: 0,
                index: 0,
                ring: [UsedElement { id: 0, length: 0 }; QUEUE_SIZE],
                available_event: 0,
            },
            events: [InputEvent::EMPTY; QUEUE_SIZE],
            base: 0,
            available_index: 0,
            last_used_index: 0,
            abs_x_min: 0,
            abs_x_max: 32767,
            abs_y_min: 0,
            abs_y_max: 32767,
            touch_x: 0,
            touch_y: 0,
            touch_pressed: false,
            pointer_dirty: false,
        }
    }
}

static mut INPUT_QUEUES: [InputQueue; MAX_DEVICES] = [const { InputQueue::new() }; MAX_DEVICES];
static mut DEVICE_COUNT: usize = 0;
static mut SHIFT: bool = false;
static mut CONTROL: bool = false;
static mut CAPS_LOCK: bool = false;

fn read32(base: usize, register: usize) -> u32 {
    unsafe { read_volatile((base + register) as *const u32) }
}

fn write32(base: usize, register: usize, value: u32) {
    unsafe { write_volatile((base + register) as *mut u32, value) }
}

fn read8(base: usize, register: usize) -> u8 {
    unsafe { read_volatile((base + register) as *const u8) }
}

fn write8(base: usize, register: usize, value: u8) {
    unsafe { write_volatile((base + register) as *mut u8, value) }
}

fn abs_range(base: usize, axis: u8) -> Option<(i32, i32)> {
    write8(base, REG_CONFIG, 0x12);
    write8(base, REG_CONFIG + 1, axis);
    if read8(base, REG_CONFIG + 2) < 8 {
        return None;
    }
    let minimum = read32(base, REG_CONFIG + 8) as i32;
    let maximum = read32(base, REG_CONFIG + 12) as i32;
    (maximum > minimum).then_some((minimum, maximum))
}

fn configure_queue(base: usize, queue: *mut InputQueue) -> Result<(), Error> {
    write32(base, REG_QUEUE_SEL, 0);
    if read32(base, REG_QUEUE_NUM_MAX) < QUEUE_SIZE as u32 {
        return Err(Error::QueueUnavailable);
    }
    write32(base, REG_QUEUE_NUM, QUEUE_SIZE as u32);
    let descriptors = unsafe { addr_of_mut!((*queue).descriptors.0) as u64 };
    let available = unsafe { addr_of_mut!((*queue).available) as u64 };
    let used = unsafe { addr_of_mut!((*queue).used) as u64 };
    write32(base, REG_QUEUE_DESC_LOW, descriptors as u32);
    write32(base, REG_QUEUE_DESC_HIGH, (descriptors >> 32) as u32);
    write32(base, REG_QUEUE_DRIVER_LOW, available as u32);
    write32(base, REG_QUEUE_DRIVER_HIGH, (available >> 32) as u32);
    write32(base, REG_QUEUE_DEVICE_LOW, used as u32);
    write32(base, REG_QUEUE_DEVICE_HIGH, (used >> 32) as u32);

    let events = unsafe { addr_of_mut!((*queue).events) as *mut InputEvent };
    let descriptor_table = unsafe { addr_of_mut!((*queue).descriptors.0) as *mut Descriptor };
    let available_ring = unsafe { addr_of_mut!((*queue).available) };
    for index in 0..QUEUE_SIZE {
        unsafe {
            write_volatile(
                descriptor_table.add(index),
                Descriptor {
                    address: events.add(index) as u64,
                    length: core::mem::size_of::<InputEvent>() as u32,
                    flags: DESCRIPTOR_F_WRITE,
                    next: 0,
                },
            );
            write_volatile(addr_of_mut!((*available_ring).ring[index]), index as u16);
        }
    }
    fence(Ordering::Release);
    unsafe {
        (*queue).available_index = QUEUE_SIZE as u16;
        write_volatile(
            addr_of_mut!((*available_ring).index),
            (*queue).available_index,
        );
    }
    write32(base, REG_QUEUE_READY, 1);
    if read32(base, REG_QUEUE_READY) != 1 {
        return Err(Error::QueueUnavailable);
    }
    write32(base, REG_QUEUE_NOTIFY, 0);
    Ok(())
}

fn initialize_device(base: usize, queue: *mut InputQueue) -> Result<(), Error> {
    if read32(base, REG_VERSION) != 2 {
        return Err(Error::UnsupportedVersion);
    }
    write32(base, REG_STATUS, 0);
    write32(base, REG_STATUS, 1);
    write32(base, REG_STATUS, 3);

    write32(base, REG_DEVICE_FEATURES_SEL, 1);
    if read32(base, REG_DEVICE_FEATURES) & FEATURE_VERSION_1 == 0 {
        write32(base, REG_STATUS, 0x80);
        return Err(Error::MissingVersionFeature);
    }
    write32(base, REG_DRIVER_FEATURES_SEL, 0);
    write32(base, REG_DRIVER_FEATURES, 0);
    write32(base, REG_DRIVER_FEATURES_SEL, 1);
    write32(base, REG_DRIVER_FEATURES, FEATURE_VERSION_1);
    write32(base, REG_STATUS, 11);
    if read32(base, REG_STATUS) & 8 == 0 {
        write32(base, REG_STATUS, read32(base, REG_STATUS) | 0x80);
        return Err(Error::FeatureNegotiationFailed);
    }

    unsafe {
        if let Some((minimum, maximum)) = abs_range(base, ABS_X as u8) {
            (*queue).abs_x_min = minimum;
            (*queue).abs_x_max = maximum;
        }
        if let Some((minimum, maximum)) = abs_range(base, ABS_Y as u8) {
            (*queue).abs_y_min = minimum;
            (*queue).abs_y_max = maximum;
        }
        (*queue).base = base;
    }
    configure_queue(base, queue)?;
    write32(base, REG_STATUS, 15);
    Ok(())
}

pub fn init() -> Result<(), Error> {
    let mut device_count = 0;
    for slot in 0..MMIO_SLOTS {
        let base = MMIO_BASE + slot * MMIO_STRIDE;
        if read32(base, REG_MAGIC) != DEVICE_MAGIC || read32(base, REG_DEVICE_ID) != DEVICE_INPUT {
            continue;
        }
        if device_count == MAX_DEVICES {
            break;
        }
        let queue = unsafe {
            addr_of_mut!(INPUT_QUEUES)
                .cast::<InputQueue>()
                .add(device_count)
        };
        initialize_device(base, queue)?;
        device_count += 1;
    }
    if device_count == 0 {
        return Err(Error::DeviceNotFound);
    }
    unsafe { DEVICE_COUNT = device_count };
    Ok(())
}

fn next_event(queue: *mut InputQueue) -> Result<Option<InputEvent>, Error> {
    let used = unsafe { addr_of!((*queue).used) };
    let available = unsafe { addr_of_mut!((*queue).available) };
    let last_used_index = unsafe { (*queue).last_used_index };
    let available_index = unsafe { (*queue).available_index };
    let used_index = unsafe { read_volatile(addr_of!((*used).index)) };
    if last_used_index == used_index {
        return Ok(None);
    }
    fence(Ordering::Acquire);
    let used_slot = last_used_index as usize % QUEUE_SIZE;
    let descriptor_id = unsafe { read_volatile(addr_of!((*used).ring[used_slot].id)) };
    if descriptor_id as usize >= QUEUE_SIZE {
        return Err(Error::InvalidUsedDescriptor(descriptor_id));
    }
    let events = unsafe { addr_of_mut!((*queue).events) as *mut InputEvent };
    let event = unsafe { read_volatile(events.add(descriptor_id as usize)) };
    let available_slot = available_index as usize % QUEUE_SIZE;
    unsafe {
        write_volatile(
            addr_of_mut!((*available).ring[available_slot]),
            descriptor_id as u16,
        );
        fence(Ordering::Release);
        (*queue).available_index = available_index.wrapping_add(1);
        write_volatile(addr_of_mut!((*available).index), (*queue).available_index);
        (*queue).last_used_index = last_used_index.wrapping_add(1);
    }
    write32(unsafe { (*queue).base }, REG_QUEUE_NOTIFY, 0);
    Ok(Some(event))
}

fn normalize_axis(value: i32, minimum: i32, maximum: i32) -> u16 {
    let value = i64::from(value.clamp(minimum, maximum)) - i64::from(minimum);
    let range = i64::from(maximum) - i64::from(minimum);
    ((value * i64::from(u16::MAX)) / range) as u16
}

pub fn poll_input() -> Result<Option<Input>, Error> {
    let count = unsafe { DEVICE_COUNT };
    for index in 0..count {
        let queue = unsafe { addr_of_mut!(INPUT_QUEUES).cast::<InputQueue>().add(index) };
        while let Some(event) = next_event(queue)? {
            if event.event_type == EVENT_ABS {
                unsafe {
                    match event.code {
                        ABS_X => {
                            (*queue).touch_x = event.value as i32;
                            (*queue).pointer_dirty = true;
                        }
                        ABS_Y => {
                            (*queue).touch_y = event.value as i32;
                            (*queue).pointer_dirty = true;
                        }
                        _ => {}
                    }
                }
                continue;
            }
            if event.event_type == EVENT_SYN && event.code == 0 {
                let position = unsafe {
                    if !(*queue).pointer_dirty {
                        None
                    } else {
                        (*queue).pointer_dirty = false;
                        Some((
                            normalize_axis(
                                (*queue).touch_x,
                                (*queue).abs_x_min,
                                (*queue).abs_x_max,
                            ),
                            normalize_axis(
                                (*queue).touch_y,
                                (*queue).abs_y_min,
                                (*queue).abs_y_max,
                            ),
                        ))
                    }
                };
                if let Some((x, y)) = position {
                    return Ok(Some(Input::Pointer { x, y }));
                }
                continue;
            }
            if event.event_type != EVENT_KEY {
                continue;
            }
            if event.code == BTN_TOUCH || event.code == BTN_LEFT {
                let pressed = event.value != 0;
                let (x, y) = unsafe {
                    if (*queue).touch_pressed == pressed {
                        continue;
                    }
                    (*queue).touch_pressed = pressed;
                    (
                        normalize_axis((*queue).touch_x, (*queue).abs_x_min, (*queue).abs_x_max),
                        normalize_axis((*queue).touch_y, (*queue).abs_y_min, (*queue).abs_y_max),
                    )
                };
                return Ok(Some(Input::Touch { x, y, pressed }));
            }
            if event.code == 42 || event.code == 54 {
                unsafe { SHIFT = event.value != 0 };
                continue;
            }
            if event.code == 29 || event.code == 97 {
                unsafe { CONTROL = event.value != 0 };
                continue;
            }
            if event.code == 58 && event.value == 1 {
                unsafe { CAPS_LOCK = !CAPS_LOCK };
                continue;
            }
            if event.value == 0 {
                continue;
            }
            let key = match event.code {
                28 => Some(Key::Enter),
                14 => Some(Key::Backspace),
                1 => Some(Key::Escape),
                103 => Some(Key::Up),
                108 => Some(Key::Down),
                31 if unsafe { CONTROL } => Some(Key::Save),
                _ => key_character(event.code).map(|character| {
                    let uppercase = unsafe { SHIFT ^ CAPS_LOCK };
                    Key::Character(if uppercase {
                        character.to_ascii_uppercase()
                    } else {
                        character
                    })
                }),
            };
            if let Some(key) = key {
                return Ok(Some(Input::Key(key)));
            }
        }
    }
    Ok(None)
}

fn key_character(code: u16) -> Option<char> {
    let shifted = unsafe { SHIFT };
    Some(match code {
        2..=10 => {
            if shifted {
                match code {
                    2 => '!',
                    3 => '@',
                    4 => '#',
                    5 => '$',
                    6 => '%',
                    7 => '^',
                    8 => '&',
                    9 => '*',
                    _ => '(',
                }
            } else {
                (b'1' + (code as u8 - 2)) as char
            }
        }
        11 => {
            if shifted {
                ')'
            } else {
                '0'
            }
        }
        12 => {
            if shifted {
                '_'
            } else {
                '-'
            }
        }
        13 => {
            if shifted {
                '+'
            } else {
                '='
            }
        }
        16 => 'q',
        17 => 'w',
        18 => 'e',
        19 => 'r',
        20 => 't',
        21 => 'y',
        22 => 'u',
        23 => 'i',
        24 => 'o',
        25 => 'p',
        26 => {
            if shifted {
                '{'
            } else {
                '['
            }
        }
        27 => {
            if shifted {
                '}'
            } else {
                ']'
            }
        }
        30 => 'a',
        31 => 's',
        32 => 'd',
        33 => 'f',
        34 => 'g',
        35 => 'h',
        36 => 'j',
        37 => 'k',
        38 => 'l',
        39 => {
            if shifted {
                ':'
            } else {
                ';'
            }
        }
        40 => {
            if shifted {
                '"'
            } else {
                '\''
            }
        }
        41 => {
            if shifted {
                '~'
            } else {
                '`'
            }
        }
        43 => {
            if shifted {
                '|'
            } else {
                '\\'
            }
        }
        44 => 'z',
        45 => 'x',
        46 => 'c',
        47 => 'v',
        48 => 'b',
        49 => 'n',
        50 => 'm',
        51 => {
            if shifted {
                '<'
            } else {
                ','
            }
        }
        52 => {
            if shifted {
                '>'
            } else {
                '.'
            }
        }
        53 => {
            if shifted {
                '?'
            } else {
                '/'
            }
        }
        57 => ' ',
        _ => return None,
    })
}

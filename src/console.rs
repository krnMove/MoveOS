use core::fmt::{self, Write};
use core::ptr::{addr_of_mut, copy, read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::virtio_gpu::Error as GpuError;
use crate::{fat32, virtio_gpu, virtio_keyboard::Key};

const MAX_WIDTH: usize = 1280;
const MAX_HEIGHT: usize = 800;
const UNLOCK_FRAMES: usize = 12;
const ICON_SCALE_STEPS: usize = 2;
const CELL_WIDTH: usize = 6;
const CELL_HEIGHT: usize = 8;
const BACKGROUND: u32 = 0xff00_0000;
const FOREGROUND: u32 = 0xffff_ffff;
const LOCK_BACKGROUND: u32 = 0xff08_090b;
const GRADIENT_END: u32 = 0xff3c_3f44;
const HEADER_BACKGROUND: u32 = 0xff17_191d;
const ACCENT: u32 = 0xffe8_f0f8;
const APP_BACKGROUND: u32 = LOCK_BACKGROUND;
const FILES_BACKGROUND: u32 = 0xff27_292e;
const FILES_SELECTED: u32 = 0xff41_454c;
const FILES_SEPARATOR: u32 = 0xff4b_4f56;
const KEYBOARD_BACKGROUND: u32 = 0xff19_1b20;
const KEY_BACKGROUND: u32 = 0xff3a_3e46;
const KEY_BORDER: u32 = 0xff57_5d67;
const KEYBOARD_HEIGHT: usize = 220;
const KEY_ROW_HEIGHT: usize = 34;
const KEY_ROW_GAP: usize = 5;
const KEY_ROW_STRIDE: usize = KEY_ROW_HEIGHT + KEY_ROW_GAP;
const POINTER_RADIUS: isize = 5;
const POINTER_DIAMETER: usize = (POINTER_RADIUS * 2 + 1) as usize;

#[repr(align(4096))]
struct Framebuffer([u32; MAX_WIDTH * MAX_HEIGHT]);

static mut FRAMEBUFFER: Framebuffer = Framebuffer([0; MAX_WIDTH * MAX_HEIGHT]);
static mut WIDTH: usize = 0;
static mut HEIGHT: usize = 0;
static mut CURSOR_X: usize = 8;
static mut CURSOR_Y: usize = 8;
static mut TERMINAL_HOURS: u8 = 0;
static mut TERMINAL_MINUTES: u8 = 0;
static mut TERMINAL_ACTIVE: bool = false;
static mut KEYBOARD_VISIBLE: bool = false;
static mut KEYBOARD_SAVE_BUTTON: bool = false;
static mut DESKTOP_ICON_VISIBLE: [bool; 2] = [true, true];
static mut POINTER_POSITIONED: bool = false;
static mut POINTER_DRAWN: bool = false;
static mut POINTER_X: usize = 0;
static mut POINTER_Y: usize = 0;
static mut POINTER_BACKGROUND: [u32; POINTER_DIAMETER * POINTER_DIAMETER] =
    [0; POINTER_DIAMETER * POINTER_DIAMETER];
static INITIALIZED: AtomicBool = AtomicBool::new(false);

pub struct MoveFilesView<'a> {
    pub creating_name: Option<&'a str>,
    pub selected_index: usize,
    pub selected_name: Option<&'a str>,
    pub is_directory: bool,
    pub contents: &'a [u8],
    pub editing: bool,
    pub dirty: bool,
    pub confirm_discard: bool,
    pub keyboard: bool,
    pub message: Option<&'a str>,
    pub create_error: Option<fat32::Error>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilesTouchAction {
    Select(usize),
    Edit,
    Back,
}

const FONT: [[u8; 7]; 36] = [
    [14, 17, 17, 31, 17, 17, 17], // A
    [30, 17, 17, 30, 17, 17, 30], // B
    [14, 17, 16, 16, 16, 17, 14], // C
    [30, 17, 17, 17, 17, 17, 30], // D
    [31, 16, 16, 30, 16, 16, 31], // E
    [31, 16, 16, 30, 16, 16, 16], // F
    [14, 17, 16, 23, 17, 17, 15], // G
    [17, 17, 17, 31, 17, 17, 17], // H
    [14, 4, 4, 4, 4, 4, 14],      // I
    [7, 2, 2, 2, 18, 18, 12],     // J
    [17, 18, 20, 24, 20, 18, 17], // K
    [16, 16, 16, 16, 16, 16, 31], // L
    [17, 27, 21, 21, 17, 17, 17], // M
    [17, 25, 21, 19, 17, 17, 17], // N
    [14, 17, 17, 17, 17, 17, 14], // O
    [30, 17, 17, 30, 16, 16, 16], // P
    [14, 17, 17, 17, 21, 18, 13], // Q
    [30, 17, 17, 30, 20, 18, 17], // R
    [15, 16, 16, 14, 1, 1, 30],   // S
    [31, 4, 4, 4, 4, 4, 4],       // T
    [17, 17, 17, 17, 17, 17, 14], // U
    [17, 17, 17, 17, 17, 10, 4],  // V
    [17, 17, 17, 21, 21, 21, 10], // W
    [17, 17, 10, 4, 10, 17, 17],  // X
    [17, 17, 10, 4, 4, 4, 4],     // Y
    [31, 1, 2, 4, 8, 16, 31],     // Z
    [14, 17, 19, 21, 25, 17, 14], // 0
    [4, 12, 4, 4, 4, 4, 14],      // 1
    [14, 17, 1, 2, 4, 8, 31],     // 2
    [30, 1, 1, 14, 1, 1, 30],     // 3
    [2, 6, 10, 18, 31, 2, 2],     // 4
    [31, 16, 16, 30, 1, 1, 30],   // 5
    [14, 16, 16, 30, 17, 17, 14], // 6
    [31, 1, 2, 4, 8, 8, 8],       // 7
    [14, 17, 17, 14, 17, 17, 14], // 8
    [14, 17, 17, 15, 1, 1, 14],   // 9
];

pub fn init() -> Result<(), GpuError> {
    let framebuffer = unsafe { addr_of_mut!(FRAMEBUFFER.0) as *mut u32 };
    let mode = virtio_gpu::initialize(framebuffer, MAX_WIDTH, MAX_HEIGHT)?;
    unsafe {
        WIDTH = mode.width;
        HEIGHT = mode.height;
        fill_background();
    }
    INITIALIZED.store(true, Ordering::Release);
    virtio_gpu::flush(mode.width, mode.height)
}

pub fn is_initialized() -> bool {
    INITIALIZED.load(Ordering::Acquire)
}

pub fn screen_size() -> (usize, usize) {
    unsafe { (WIDTH, HEIGHT) }
}

pub fn touch_to_pixel(x: u16, y: u16) -> (usize, usize) {
    let (width, height) = screen_size();
    (
        (u64::from(x) * width.saturating_sub(1) as u64 / u16::MAX as u64) as usize,
        (u64::from(y) * height.saturating_sub(1) as u64 / u16::MAX as u64) as usize,
    )
}

pub fn keyboard_key_at(x: u16, y: u16) -> Option<Key> {
    let (x, y) = touch_to_pixel(x, y);
    unsafe { keyboard_key_at_pixel(x, y) }
}

pub fn files_touch_action_at(
    x: usize,
    y: usize,
    creating: bool,
    selected_index: usize,
) -> Option<FilesTouchAction> {
    const ROW_HEIGHT: usize = 24;
    let (width, height) = screen_size();
    let portrait = height > width;
    let split = if portrait {
        files_divider_y(height)
    } else {
        files_divider_x(width)
    };
    let list_top = files_list_top(creating);
    let list_bottom = if portrait {
        split.saturating_sub(8)
    } else if unsafe { KEYBOARD_VISIBLE } {
        keyboard_top().saturating_sub(8)
    } else {
        height.saturating_sub(40)
    };
    let in_list = if portrait {
        y >= list_top && y < list_bottom
    } else {
        x < split && y >= list_top && y < list_bottom
    };
    if in_list {
        let visible_rows = (list_bottom.saturating_sub(list_top) / ROW_HEIGHT).max(1);
        let row = (y - list_top) / ROW_HEIGHT;
        if row >= visible_rows {
            return None;
        }
        let first_visible = selected_index.saturating_sub(visible_rows - 1);
        return Some(FilesTouchAction::Select(first_visible + row));
    }

    if portrait {
        let controls_top = split.saturating_add(16 + 46);
        if (controls_top..controls_top + 20).contains(&y) {
            return Some(FilesTouchAction::Edit);
        }
    } else if x >= split {
        if (96..132).contains(&y) {
            return Some(FilesTouchAction::Edit);
        }
        if (48..80).contains(&y) {
            return Some(FilesTouchAction::Back);
        }
    }
    None
}

pub fn move_pointer(x: u16, y: u16) -> fmt::Result {
    if !is_initialized() {
        return Err(fmt::Error);
    }
    let (x, y) = touch_to_pixel(x, y);
    unsafe {
        let previous = if POINTER_DRAWN {
            Some((POINTER_X, POINTER_Y))
        } else {
            None
        };
        restore_pointer();
        POINTER_X = x;
        POINTER_Y = y;
        POINTER_POSITIONED = true;
        draw_pointer();
        if let Some((previous_x, previous_y)) = previous {
            flush_pointer_region(previous_x, previous_y)?;
        }
        flush_pointer_region(x, y)
    }
}

pub fn desktop_app_at(x: usize, y: usize) -> Option<usize> {
    if !(60..176).contains(&y) {
        return None;
    }
    let index = x.checked_sub(20)? / 128;
    if index >= 2 || x >= 20 + (index + 1) * 128 {
        return None;
    }
    unsafe {
        if DESKTOP_ICON_VISIBLE[index] {
            Some(index)
        } else {
            None
        }
    }
}

pub fn hide_desktop_icon(index: usize) {
    if index < 2 {
        unsafe { DESKTOP_ICON_VISIBLE[index] = false };
    }
}

#[derive(Clone, Copy, Debug)]
pub enum ContextAction {
    Info,
    Remove,
    Close,
}

pub fn draw_context_menu(index: usize, showing_info: bool) -> fmt::Result {
    if !is_initialized() || index >= 2 {
        return Err(fmt::Error);
    }
    unsafe {
        restore_pointer();
        let left = WIDTH.saturating_sub(320) / 2;
        let top = HEIGHT.saturating_sub(190) / 2;
        fill_rounded_rect(left, top, 320, 190, 16, 0xff20_2329);
        fill_rounded_rect(left + 3, top + 3, 314, 184, 13, 0xff20_2329);
        draw_text(
            if index == 0 { "TERMINAL" } else { "MOVE FILES" },
            left + 24,
            top + 20,
            2,
            FOREGROUND,
        );
        if showing_info {
            draw_text(
                if index == 0 {
                    "ISOLATED USER APP"
                } else {
                    "FAT32 FILE MANAGER"
                },
                left + 24,
                top + 68,
                1,
                ACCENT,
            );
            draw_context_button(left + 20, top + 126, 280, "CLOSE");
        } else {
            draw_context_button(left + 20, top + 68, 280, "INFO");
            draw_context_button(left + 20, top + 118, 280, "REMOVE ICON");
        }
        draw_pointer();
        virtio_gpu::flush(WIDTH, HEIGHT).map_err(|_| fmt::Error)
    }
}

pub fn context_menu_choice(x: usize, y: usize, showing_info: bool) -> Option<ContextAction> {
    let (width, height) = screen_size();
    let left = width.saturating_sub(320) / 2;
    let top = height.saturating_sub(190) / 2;
    if x < left + 20 || x >= left + 300 {
        return None;
    }
    if showing_info {
        (top + 126..top + 166)
            .contains(&y)
            .then_some(ContextAction::Close)
    } else if (top + 68..top + 108).contains(&y) {
        Some(ContextAction::Info)
    } else if (top + 118..top + 158).contains(&y) {
        Some(ContextAction::Remove)
    } else {
        None
    }
}

pub fn print(arguments: fmt::Arguments<'_>) -> fmt::Result {
    ConsoleWriter.write_fmt(arguments)
}

pub fn clear() -> fmt::Result {
    if !is_initialized() {
        return Err(fmt::Error);
    }
    unsafe {
        restore_pointer();
        fill_rect(0, 0, WIDTH, HEIGHT, APP_BACKGROUND);
        KEYBOARD_VISIBLE = true;
        KEYBOARD_SAVE_BUTTON = false;
        draw_status_bar("TERMINAL", TERMINAL_HOURS, TERMINAL_MINUTES, 4)?;
        draw_keyboard(false);
        CURSOR_X = 16;
        CURSOR_Y = 52;
        draw_pointer();
        virtio_gpu::flush(WIDTH, HEIGHT).map_err(|_| fmt::Error)
    }
}

pub fn draw_lock_screen(hours: u8, minutes: u8) -> fmt::Result {
    if !is_initialized() {
        return Err(fmt::Error);
    }

    unsafe {
        restore_pointer();
        fill_rect(0, 0, WIDTH, HEIGHT, 0xff00_0000);
        TERMINAL_ACTIVE = false;
        KEYBOARD_VISIBLE = false;
        draw_lock_clock(hours, minutes);
        draw_pointer();
        virtio_gpu::flush(WIDTH, HEIGHT).map_err(|_| fmt::Error)
    }
}

pub fn draw_desktop(hours: u8, minutes: u8, phase: u16) -> fmt::Result {
    if !is_initialized() {
        return Err(fmt::Error);
    }

    unsafe { draw_desktop_scene(hours, minutes, phase, 4, ICON_SCALE_STEPS, 2) }
}

pub fn unlock_to_desktop(hours: u8, minutes: u8, phase: u16) -> fmt::Result {
    if !is_initialized() {
        return Err(fmt::Error);
    }

    unsafe {
        KEYBOARD_VISIBLE = false;
        for frame in 0..=UNLOCK_FRAMES {
            restore_pointer();
            fill_gradient(0, phase);
            let bar_y = frame as isize * 40 / UNLOCK_FRAMES as isize - 40;
            fill_rect_clipped_y(0, bar_y, WIDTH, 40, HEADER_BACKGROUND);
            draw_pointer();
            virtio_gpu::flush(WIDTH, HEIGHT).map_err(|_| fmt::Error)?;
            if frame < UNLOCK_FRAMES {
                wait_animation_frame();
            }
        }

        for elements in 1..=4 {
            draw_desktop_scene(hours, minutes, phase, elements, 0, 0)?;
            wait_animation_frame();
        }

        for visible_icons in 1..=2 {
            for scale in 1..=ICON_SCALE_STEPS {
                draw_desktop_scene(hours, minutes, phase, 4, scale, visible_icons)?;
                if scale < ICON_SCALE_STEPS || visible_icons < 2 {
                    wait_animation_frame();
                }
            }
        }
        Ok(())
    }
}

unsafe fn draw_desktop_scene(
    hours: u8,
    minutes: u8,
    phase: u16,
    status_elements: u8,
    icon_scale: usize,
    visible_icons: usize,
) -> fmt::Result {
    unsafe {
        restore_pointer();
        fill_gradient(0, phase);
        TERMINAL_ACTIVE = false;
        KEYBOARD_VISIBLE = false;
        draw_status_bar("", hours, minutes, status_elements)?;
        draw_desktop_icons(icon_scale, visible_icons);
        draw_pointer();
        virtio_gpu::flush(WIDTH, HEIGHT).map_err(|_| fmt::Error)
    }
}

pub fn draw_terminal(hours: u8, minutes: u8) -> fmt::Result {
    if !is_initialized() {
        return Err(fmt::Error);
    }

    unsafe {
        restore_pointer();
        TERMINAL_HOURS = hours;
        TERMINAL_MINUTES = minutes;
        TERMINAL_ACTIVE = true;
        fill_rect(0, 0, WIDTH, HEIGHT, APP_BACKGROUND);
        KEYBOARD_VISIBLE = true;
        KEYBOARD_SAVE_BUTTON = false;
        draw_status_bar("TERMINAL", hours, minutes, 4)?;
        draw_keyboard(false);
        CURSOR_X = 16;
        CURSOR_Y = 52;
        draw_pointer();
        virtio_gpu::flush(WIDTH, HEIGHT).map_err(|_| fmt::Error)
    }
}

pub fn draw_move_files(
    hours: u8,
    minutes: u8,
    volume: Option<fat32::Fat32>,
    filesystem_error: Option<fat32::Error>,
    view: MoveFilesView<'_>,
) -> fmt::Result {
    if !is_initialized() {
        return Err(fmt::Error);
    }

    unsafe {
        restore_pointer();
        TERMINAL_ACTIVE = false;
        KEYBOARD_VISIBLE = view.keyboard;
        KEYBOARD_SAVE_BUTTON = view.editing && !view.confirm_discard;
        fill_rect(0, 0, WIDTH, HEIGHT, FILES_BACKGROUND);
        draw_status_bar("MOVE FILES", hours, minutes, 4)?;
        let portrait = HEIGHT > WIDTH;
        let split = if portrait {
            files_divider_y(HEIGHT)
        } else {
            files_divider_x(WIDTH)
        };
        let content_x = if portrait {
            24
        } else {
            split.saturating_add(24)
        };
        draw_text("FILES", 24, 52, 2, ACCENT);
        if portrait {
            fill_rect(16, split, WIDTH.saturating_sub(32), 2, FILES_SEPARATOR);
            draw_text(
                if view.editing { "EDITOR" } else { "PREVIEW" },
                24,
                split.saturating_add(16),
                2,
                ACCENT,
            );
        } else {
            draw_text(
                if view.editing { "EDITOR" } else { "PREVIEW" },
                content_x,
                52,
                2,
                ACCENT,
            );
            fill_rect(split, 48, 2, HEIGHT.saturating_sub(64), FILES_SEPARATOR);
        }
        if let Some(name) = view.creating_name {
            draw_text("NEW FILE:", 24, 76, 1, ACCENT);
            draw_text(name, 24, 90, 1, FOREGROUND);
        }

        if let Some(volume) = volume {
            draw_file_panes(volume, &view, split, content_x, portrait)?;
        } else {
            CURSOR_X = content_x;
            CURSOR_Y = if portrait {
                split.saturating_add(48)
            } else {
                84
            };
            if let Some(error) = filesystem_error {
                write!(&mut FramebufferWriter, "{}", error)?;
            } else {
                write!(&mut FramebufferWriter, "NO VOLUME MOUNTED")?;
            }
        }
        if let Some(error) = view.create_error {
            CURSOR_X = 24;
            CURSOR_Y = HEIGHT.saturating_sub(44);
            write!(&mut FramebufferWriter, "{}", error)?;
        }
        if let Some(message) = view.message {
            draw_text(
                message,
                24,
                if view.keyboard {
                    keyboard_top().saturating_sub(24)
                } else {
                    HEIGHT.saturating_sub(24)
                },
                1,
                ACCENT,
            );
        } else if view.dirty {
            draw_text(
                "UNSAVED CHANGES",
                24,
                if view.keyboard {
                    keyboard_top().saturating_sub(24)
                } else {
                    HEIGHT.saturating_sub(24)
                },
                1,
                ACCENT,
            );
        }
        if KEYBOARD_VISIBLE {
            draw_keyboard(KEYBOARD_SAVE_BUTTON);
        }
        draw_pointer();
        virtio_gpu::flush(WIDTH, HEIGHT).map_err(|_| fmt::Error)
    }
}

unsafe fn draw_file_panes(
    volume: fat32::Fat32,
    view: &MoveFilesView<'_>,
    split: usize,
    content_x: usize,
    portrait: bool,
) -> fmt::Result {
    const ROW_HEIGHT: usize = 24;
    let list_top = files_list_top(view.creating_name.is_some());
    let list_bottom = if portrait {
        split.saturating_sub(8)
    } else if view.keyboard {
        keyboard_top().saturating_sub(8)
    } else {
        unsafe { HEIGHT }.saturating_sub(40)
    };
    let visible_rows = (list_bottom.saturating_sub(list_top) / ROW_HEIGHT).max(1);
    let first_visible = view.selected_index.saturating_sub(visible_rows - 1);
    let width = unsafe { WIDTH };
    let mut writer = FramebufferWriter;
    let mut count = 0;
    let mut truncated = false;
    let mut write_failed = false;
    let result = volume.list_root(|entry| {
        let current = count;
        count += 1;
        if current < first_visible {
            return true;
        }
        let visible_index = current - first_visible;
        if visible_index >= visible_rows {
            truncated = true;
            return false;
        }
        let row_y = list_top + visible_index * ROW_HEIGHT;

        if current == view.selected_index {
            unsafe {
                fill_rect(
                    16,
                    row_y,
                    if portrait {
                        width.saturating_sub(32)
                    } else {
                        split.saturating_sub(24)
                    },
                    ROW_HEIGHT - 2,
                    FILES_SELECTED,
                );
            }
        }
        unsafe {
            CURSOR_X = 24;
            CURSOR_Y = row_y + 3;
        }
        let suffix = if entry.is_directory() { "/" } else { "" };
        if writeln!(&mut writer, "{}{}", entry.name(), suffix).is_err() {
            write_failed = true;
            return false;
        }
        unsafe {
            fill_rect(
                24,
                row_y + ROW_HEIGHT - 2,
                if portrait {
                    width.saturating_sub(48)
                } else {
                    split.saturating_sub(40)
                },
                1,
                FILES_SEPARATOR,
            );
        }
        true
    });

    if write_failed {
        return Err(fmt::Error);
    }
    match result {
        Ok(()) if count == 0 => {
            unsafe {
                CURSOR_X = 24;
                CURSOR_Y = list_top;
            }
            writeln!(&mut writer, "EMPTY")?;
        }
        Ok(()) => {}
        Err(error) => writeln!(&mut writer, "READ ERROR: {}", error)?,
    }

    if truncated {
        unsafe {
            draw_text(
                "MORE",
                if portrait {
                    width.saturating_sub(64)
                } else {
                    split.saturating_sub(58)
                },
                52,
                1,
                ACCENT,
            )
        };
    }
    if let Some(name) = view.selected_name {
        unsafe {
            let content_top = if portrait {
                split.saturating_add(40)
            } else {
                80
            };
            draw_text(name, content_x, content_top, 2, FOREGROUND);
            if view.is_directory {
                draw_text("FOLDER", content_x, content_top + 30, 1, ACCENT);
            } else if view.editing {
                draw_text(
                    "TYPE TO APPEND  CTRL+S SAVE",
                    content_x,
                    content_top + 22,
                    1,
                    ACCENT,
                );
            } else {
                draw_text(
                    "E EDIT  UP/DOWN SELECT",
                    content_x,
                    content_top + 22,
                    1,
                    ACCENT,
                );
            }
            if view.confirm_discard {
                draw_text(
                    "S SAVE  D DISCARD  ESC CANCEL",
                    content_x,
                    content_top + 38,
                    1,
                    ACCENT,
                );
            } else if !view.is_directory {
                draw_file_contents(view.contents, content_x, content_top + 46)?;
            }
        }
    } else {
        unsafe {
            draw_text(
                "SELECT A FILE",
                content_x,
                if portrait {
                    split.saturating_add(40)
                } else {
                    84
                },
                1,
                ACCENT,
            )
        };
    }
    Ok(())
}

unsafe fn draw_file_contents(contents: &[u8], x: usize, top: usize) -> fmt::Result {
    let width = unsafe { WIDTH };
    let height = unsafe { HEIGHT };
    let bottom = if unsafe { KEYBOARD_VISIBLE } {
        keyboard_top().saturating_sub(8)
    } else {
        height.saturating_sub(36)
    };
    let characters_per_line = width
        .saturating_sub(x + 24)
        .checked_div(CELL_WIDTH * 2)
        .unwrap_or(1)
        .max(1);
    let mut row = 0;
    let mut column = 0;
    if contents.is_empty() {
        unsafe { draw_text("EMPTY FILE", x, top, 1, ACCENT) };
        return Ok(());
    }

    for &byte in contents {
        if byte == b'\r' {
            continue;
        }
        if byte == b'\n' {
            row += 1;
            column = 0;
        } else {
            if column == characters_per_line {
                row += 1;
                column = 0;
            }
            let y = top + row * CELL_HEIGHT * 2;
            if y + CELL_HEIGHT * 2 > bottom {
                unsafe { draw_text("MORE", x, bottom.saturating_sub(16), 1, ACCENT) };
                return Ok(());
            }
            let character = if byte.is_ascii_graphic() || byte == b' ' {
                byte as char
            } else if byte.is_ascii() {
                '?'
            } else {
                unsafe { draw_text("NON-ASCII TEXT", x, top, 1, ACCENT) };
                return Ok(());
            };
            unsafe { draw_scaled_glyph(character, x + column * CELL_WIDTH * 2, y, 2, FOREGROUND) };
            column += 1;
        }
        if top + row * CELL_HEIGHT * 2 + CELL_HEIGHT * 2 > bottom {
            unsafe { draw_text("MORE", x, bottom.saturating_sub(16), 1, ACCENT) };
            return Ok(());
        }
    }
    Ok(())
}

unsafe fn draw_scaled_glyph(character: char, x: usize, y: usize, scale: usize, color: u32) {
    let width = unsafe { WIDTH };
    let height = unsafe { HEIGHT };
    let buffer = unsafe { addr_of_mut!(FRAMEBUFFER.0) as *mut u32 };
    for (row, bits) in glyph(character).iter().enumerate() {
        for column in 0..5 {
            if bits & (1 << (4 - column)) == 0 {
                continue;
            }
            for dy in 0..scale {
                for dx in 0..scale {
                    let pixel_x = x + column * scale + dx;
                    let pixel_y = y + row * scale + dy;
                    if pixel_x < width && pixel_y < height {
                        unsafe {
                            write_volatile(buffer.add(pixel_y * width + pixel_x), color);
                        }
                    }
                }
            }
        }
    }
}

unsafe fn draw_desktop_icons(scale: usize, visible_icons: usize) {
    if scale == 0 {
        return;
    }

    const ICON_SIZE: usize = 64;
    const CELL_SIZE: usize = 128;
    const ORIGIN_X: usize = 20;
    const ICON_Y: usize = 68;
    const LABEL_Y: usize = 144;

    for (index, label) in ["Terminal", "Move Files"].into_iter().enumerate() {
        if index >= visible_icons || !unsafe { DESKTOP_ICON_VISIBLE[index] } {
            continue;
        }
        let icon_scale = if index + 1 == visible_icons {
            scale
        } else {
            ICON_SCALE_STEPS
        };
        let cell_x = ORIGIN_X + index * CELL_SIZE;
        let icon_x = cell_x + (CELL_SIZE - ICON_SIZE) / 2;
        let icon_size = ICON_SIZE * icon_scale / ICON_SCALE_STEPS;
        let icon_y = ICON_Y;

        unsafe {
            let (background, radius) = match index {
                0 => (0xff25_282d, 12),
                _ => (0xffd5_d8dd, 12),
            };
            fill_rounded_rect(
                icon_x,
                icon_y,
                icon_size,
                icon_size,
                radius * icon_scale / ICON_SCALE_STEPS,
                background,
            );

            match index {
                0 => draw_text_rounded(
                    ">_",
                    icon_x + icon_size / 2 - 18 * icon_scale / 4,
                    icon_y + icon_size / 2 - 7 * icon_scale / 4,
                    3 * icon_scale / 4,
                    ACCENT,
                ),
                _ => {
                    let folder_color = 0xff6d_737c;
                    fill_rounded_rect(
                        icon_x + 12 * icon_scale / 4,
                        icon_y + 26 * icon_scale / 4,
                        40 * icon_scale / 4,
                        25 * icon_scale / 4,
                        4 * icon_scale / 4,
                        folder_color,
                    );
                    fill_rounded_rect(
                        icon_x + 14 * icon_scale / 4,
                        icon_y + 19 * icon_scale / 4,
                        18 * icon_scale / 4,
                        11 * icon_scale / 4,
                        3 * icon_scale / 4,
                        folder_color,
                    );
                }
            }

            if icon_scale == ICON_SCALE_STEPS {
                let label_width = label.chars().count() * CELL_WIDTH * 2;
                let label_x = cell_x + (CELL_SIZE - label_width) / 2;
                draw_text_rounded(label, label_x, LABEL_Y, 2, ACCENT);
            }
        }
    }
}

unsafe fn draw_context_button(x: usize, y: usize, width: usize, label: &str) {
    unsafe {
        fill_rounded_rect(x, y, width, 40, 10, KEY_BACKGROUND);
        draw_centered(label, y + 14, 1, FOREGROUND);
    }
}

fn keyboard_top() -> usize {
    unsafe { HEIGHT.saturating_sub(KEYBOARD_HEIGHT) }
}

fn files_divider_x(width: usize) -> usize {
    (width / 3).max(240).min(width.saturating_sub(180))
}

fn files_divider_y(height: usize) -> usize {
    height * 3 / 8
}

fn files_list_top(creating: bool) -> usize {
    if creating { 112 } else { 80 }
}

unsafe fn draw_keyboard(show_save: bool) {
    let width = unsafe { WIDTH };
    let top = keyboard_top();
    unsafe { fill_rect(0, top, width, KEYBOARD_HEIGHT, KEYBOARD_BACKGROUND) };

    let digits = b"1234567890";
    let letters = b"QWERTYUIOP";
    let middle = b"ASDFGHJKL";
    let lower = b"ZXCVBNM.-/";
    for (row, keys) in [
        (0, digits.as_slice()),
        (1, letters.as_slice()),
        (2, middle.as_slice()),
        (3, lower.as_slice()),
    ] {
        let count = keys.len();
        let available_width = width.saturating_sub(16);
        let gap = 4;
        let key_width = if count == 9 {
            (available_width.saturating_sub(gap * 8) / count).min(88)
        } else {
            (available_width.saturating_sub(gap * (count - 1)) / count).min(100)
        };
        let total_width = count * key_width + (count - 1) * gap;
        let start = width.saturating_sub(total_width) / 2;
        let y = top + row * KEY_ROW_STRIDE + 5;
        for (index, key) in keys.iter().enumerate() {
            let x = start + index * (key_width + gap);
            unsafe {
                draw_keyboard_key(
                    x,
                    y,
                    key_width,
                    KEY_ROW_HEIGHT,
                    core::str::from_utf8(core::slice::from_ref(key)).unwrap(),
                )
            };
        }
    }

    let bottom_y = top + 4 * KEY_ROW_STRIDE + 5;
    let gap = 5;
    let left = 8;
    if show_save {
        let available = width.saturating_sub(16 + gap * 3);
        let backspace_width = available / 6;
        let space_width = available / 2;
        let save_width = available / 6;
        let enter_width = available.saturating_sub(backspace_width + space_width + save_width);
        let space_x = left + backspace_width + gap;
        let save_x = space_x + space_width + gap;
        unsafe {
            draw_keyboard_key(left, bottom_y, backspace_width, KEY_ROW_HEIGHT, "DEL");
            draw_keyboard_key(space_x, bottom_y, space_width, KEY_ROW_HEIGHT, "SPACE");
            draw_keyboard_key(save_x, bottom_y, save_width, KEY_ROW_HEIGHT, "SAVE");
            draw_keyboard_key(
                save_x + save_width + gap,
                bottom_y,
                enter_width,
                KEY_ROW_HEIGHT,
                "ENTER",
            );
        }
    } else {
        let available = width.saturating_sub(16 + gap * 2);
        let backspace_width = available / 5;
        let space_width = available * 3 / 5;
        let enter_width = available.saturating_sub(backspace_width + space_width);
        unsafe {
            draw_keyboard_key(left, bottom_y, backspace_width, KEY_ROW_HEIGHT, "DEL");
            draw_keyboard_key(
                left + backspace_width + gap,
                bottom_y,
                space_width,
                KEY_ROW_HEIGHT,
                "SPACE",
            );
            draw_keyboard_key(
                left + backspace_width + gap + space_width + gap,
                bottom_y,
                enter_width,
                KEY_ROW_HEIGHT,
                "ENTER",
            );
        }
    }
}

unsafe fn draw_keyboard_key(x: usize, y: usize, width: usize, height: usize, label: &str) {
    unsafe {
        fill_rounded_rect(x, y, width, height, 6, KEY_BORDER);
        fill_rounded_rect(
            x + 1,
            y + 1,
            width.saturating_sub(2),
            height.saturating_sub(2),
            5,
            KEY_BACKGROUND,
        );
        let text_width = label.chars().count() * CELL_WIDTH;
        draw_text(
            label,
            x + width.saturating_sub(text_width) / 2,
            y + height.saturating_sub(7) / 2,
            1,
            FOREGROUND,
        );
    }
}

unsafe fn keyboard_key_at_pixel(x: usize, y: usize) -> Option<Key> {
    let width = unsafe { WIDTH };
    let top = keyboard_top();
    if !unsafe { KEYBOARD_VISIBLE } || y < top {
        return None;
    }
    let row = (y - top) / KEY_ROW_STRIDE;
    let row_y = top + row * KEY_ROW_STRIDE + 5;
    if y < row_y || y >= row_y + KEY_ROW_HEIGHT {
        return None;
    }
    let (keys, count, max_key_width): (&[u8], usize, usize) = match row {
        0 => (b"1234567890", 10, 100),
        1 => (b"QWERTYUIOP", 10, 100),
        2 => (b"ASDFGHJKL", 9, 88),
        3 => (b"ZXCVBNM.-/", 10, 100),
        4 => {
            let gap = 5;
            let left = 8;
            if unsafe { KEYBOARD_SAVE_BUTTON } {
                let available = width.saturating_sub(16 + gap * 3);
                let backspace_width = available / 6;
                let space_width = available / 2;
                let save_width = available / 6;
                let space_start = left + backspace_width + gap;
                let save_start = space_start + space_width + gap;
                let enter_start = save_start + save_width + gap;
                return if x >= left && x < left + backspace_width {
                    Some(Key::Backspace)
                } else if x >= space_start && x < space_start + space_width {
                    Some(Key::Character(' '))
                } else if x >= save_start && x < save_start + save_width {
                    Some(Key::Save)
                } else if x >= enter_start && x < width.saturating_sub(8) {
                    Some(Key::Enter)
                } else {
                    None
                };
            } else {
                let available = width.saturating_sub(16 + gap * 2);
                let backspace_width = available / 5;
                let space_width = available * 3 / 5;
                return if x >= left && x < left + backspace_width {
                    Some(Key::Backspace)
                } else if x >= left + backspace_width + gap
                    && x < left + backspace_width + gap + space_width
                {
                    Some(Key::Character(' '))
                } else if x >= left + backspace_width + gap + space_width + gap
                    && x < width.saturating_sub(8)
                {
                    Some(Key::Enter)
                } else {
                    None
                };
            }
        }
        _ => return None,
    };
    let gap = 4;
    let available = width.saturating_sub(16);
    let key_width = if count == 9 {
        (available.saturating_sub(gap * 8) / count).min(max_key_width)
    } else {
        (available.saturating_sub(gap * (count - 1)) / count).min(max_key_width)
    };
    let total_width = count * key_width + (count - 1) * gap;
    let start = width.saturating_sub(total_width) / 2;
    let relative_x = x.checked_sub(start)?;
    let index = relative_x / (key_width + gap);
    if index >= count || relative_x % (key_width + gap) >= key_width {
        return None;
    }
    Some(Key::Character(keys[index] as char))
}

unsafe fn draw_status_bar(
    title: &str,
    hours: u8,
    minutes: u8,
    visible_elements: u8,
) -> fmt::Result {
    let time = [
        b'0' + hours / 10,
        b'0' + hours % 10,
        b':',
        b'0' + minutes / 10,
        b'0' + minutes % 10,
    ];
    let time = core::str::from_utf8(&time).map_err(|_| fmt::Error)?;
    unsafe {
        fill_rect(0, 0, WIDTH, 40, HEADER_BACKGROUND);
        if visible_elements >= 1 {
            draw_text(time, 12, 10, 2, FOREGROUND);
        }
        if visible_elements >= 2 {
            draw_centered(title, 10, 2, FOREGROUND);
        }
        let status_x = WIDTH.saturating_sub(160);
        if visible_elements >= 3 {
            draw_text("WIFI", status_x, 10, 2, ACCENT);
        }
        let battery_x = status_x + 56;
        if visible_elements >= 4 {
            fill_rect(battery_x, 15, 17, 10, ACCENT);
            fill_rect(battery_x + 2, 17, 13, 6, HEADER_BACKGROUND);
            fill_rect(battery_x + 17, 18, 2, 4, ACCENT);
            fill_rect(battery_x + 3, 18, 11, 4, ACCENT);
            draw_text("100%", battery_x + 24, 10, 2, ACCENT);
        }
    }
    Ok(())
}

pub fn backspace() -> fmt::Result {
    if !is_initialized() {
        return Err(fmt::Error);
    }
    unsafe {
        restore_pointer();
        if CURSOR_X > 8 {
            CURSOR_X -= CELL_WIDTH;
            let buffer = addr_of_mut!(FRAMEBUFFER.0) as *mut u32;
            for row in 0..CELL_HEIGHT {
                for column in 0..CELL_WIDTH {
                    write_volatile(
                        buffer.add((CURSOR_Y + row) * WIDTH + CURSOR_X + column),
                        if TERMINAL_ACTIVE {
                            APP_BACKGROUND
                        } else {
                            BACKGROUND
                        },
                    );
                }
            }
        }
        draw_pointer();
        virtio_gpu::flush(WIDTH, HEIGHT).map_err(|_| fmt::Error)
    }
}

struct ConsoleWriter;

impl Write for ConsoleWriter {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if !is_initialized() {
            return Err(fmt::Error);
        }

        unsafe {
            restore_pointer();
            for character in text.chars() {
                if character == '\n' {
                    newline();
                } else if character == '\r' {
                    CURSOR_X = 8;
                } else {
                    draw_character(character);
                }
            }

            draw_pointer();
            virtio_gpu::flush(WIDTH, HEIGHT).map_err(|_| fmt::Error)
        }
    }
}

struct FramebufferWriter;

impl Write for FramebufferWriter {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if !is_initialized() {
            return Err(fmt::Error);
        }

        unsafe {
            for character in text.chars() {
                if character == '\n' {
                    newline();
                } else if character == '\r' {
                    CURSOR_X = 24;
                } else {
                    draw_character(character);
                }
            }
        }
        Ok(())
    }
}

unsafe fn fill_gradient(offset_y: usize, phase: u16) {
    let width = unsafe { WIDTH };
    let height = unsafe { HEIGHT };
    let buffer = unsafe { addr_of_mut!(FRAMEBUFFER.0) as *mut u32 };
    let max_y = height.saturating_sub(1).max(1);
    let max_x = width.max(1);

    for y in offset_y.min(height)..height {
        let source_y = y.saturating_sub(offset_y);
        let vertical = (source_y * 150 / max_y) as u32;
        for x in 0..width {
            let wave_position =
                (u32::from(phase) + (x * 256 / max_x) as u32 + (source_y * 96 / max_y) as u32)
                    & 0x1ff;
            let wave = if wave_position < 256 {
                wave_position
            } else {
                511 - wave_position
            };
            let amount = (vertical + wave * 100 / 255).min(255);
            let inverse = 255 - amount;
            let color = 0xff00_0000
                | (mix_channel(
                    (LOCK_BACKGROUND >> 16) & 0xff,
                    (GRADIENT_END >> 16) & 0xff,
                    inverse,
                    amount,
                ) << 16)
                | (mix_channel(
                    (LOCK_BACKGROUND >> 8) & 0xff,
                    (GRADIENT_END >> 8) & 0xff,
                    inverse,
                    amount,
                ) << 8)
                | mix_channel(LOCK_BACKGROUND & 0xff, GRADIENT_END & 0xff, inverse, amount);
            unsafe {
                write_volatile(buffer.add(y * width + x), color);
            }
        }
    }
}

unsafe fn fill_rect_clipped_y(x: usize, y: isize, width: usize, height: usize, color: u32) {
    let screen_height = unsafe { HEIGHT };
    let top = y.max(0) as usize;
    let bottom = (y + height as isize).min(screen_height as isize).max(0) as usize;
    if bottom > top {
        unsafe { fill_rect(x, top, width, bottom - top, color) };
    }
}

fn mix_channel(start: u32, end: u32, inverse: u32, amount: u32) -> u32 {
    (start * inverse + end * amount) / 255
}

unsafe fn draw_lock_clock(hours: u8, minutes: u8) {
    const DIGIT_HEIGHT: usize = 88;
    const DIGIT_WIDTH: usize = 49;
    const COLON_WIDTH: usize = 10;
    const GAP: usize = 9;

    let height = if unsafe { HEIGHT } >= 600 {
        DIGIT_HEIGHT
    } else {
        DIGIT_HEIGHT * unsafe { HEIGHT } / 600
    };
    let width = DIGIT_WIDTH * height / DIGIT_HEIGHT;
    let colon_width = COLON_WIDTH * height / DIGIT_HEIGHT;
    let gap = GAP * height / DIGIT_HEIGHT;
    let total_width = width * 4 + colon_width + gap * 4;
    let mut x = unsafe { WIDTH }.saturating_sub(total_width) / 2;
    let y = unsafe { HEIGHT / 4 };
    let color = 0xfff2_f7ff;
    let radius = (height as f32 * 0.027).max(1.4);
    let colon_radius = radius * 1.15;

    unsafe {
        draw_modern_digit(x, y, width, height, hours / 10, radius, color);
        x += width + gap;
        draw_modern_digit(x, y, width, height, hours % 10, radius, color);
        x += width + gap;
        draw_rounded_line(
            x as f32 + colon_width as f32 / 2.0,
            y as f32 + height as f32 * 0.36,
            x as f32 + colon_width as f32 / 2.0,
            y as f32 + height as f32 * 0.36,
            colon_radius,
            color,
        );
        draw_rounded_line(
            x as f32 + colon_width as f32 / 2.0,
            y as f32 + height as f32 * 0.69,
            x as f32 + colon_width as f32 / 2.0,
            y as f32 + height as f32 * 0.69,
            colon_radius,
            color,
        );
        x += colon_width + gap;
        draw_modern_digit(x, y, width, height, minutes / 10, radius, color);
        x += width + gap;
        draw_modern_digit(x, y, width, height, minutes % 10, radius, color);
    }
}

unsafe fn draw_modern_digit(
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    digit: u8,
    radius: f32,
    color: u32,
) {
    let path: &[(u8, u8)] = match digit {
        0 => &[
            (50, 2),
            (76, 5),
            (93, 24),
            (98, 68),
            (93, 113),
            (76, 134),
            (50, 138),
            (24, 134),
            (7, 113),
            (2, 68),
            (7, 24),
            (24, 5),
            (50, 2),
        ],
        1 => &[(25, 22), (54, 2), (54, 138), (18, 138), (86, 138)],
        2 => &[
            (10, 28),
            (20, 10),
            (43, 2),
            (68, 4),
            (88, 17),
            (96, 36),
            (91, 54),
            (74, 73),
            (10, 132),
            (96, 132),
        ],
        3 => &[
            (12, 12),
            (40, 2),
            (70, 5),
            (90, 20),
            (94, 39),
            (84, 55),
            (64, 67),
            (83, 75),
            (96, 94),
            (91, 118),
            (73, 135),
            (43, 140),
            (13, 130),
        ],
        4 => &[(76, 140), (76, 2), (7, 84), (96, 84)],
        5 => &[
            (93, 3),
            (18, 3),
            (12, 61),
            (55, 61),
            (79, 69),
            (94, 88),
            (92, 111),
            (75, 132),
            (46, 140),
            (14, 130),
        ],
        6 => &[
            (85, 8),
            (58, 2),
            (35, 8),
            (15, 30),
            (8, 69),
            (10, 108),
            (28, 132),
            (55, 140),
            (82, 130),
            (94, 108),
            (92, 88),
            (78, 72),
            (55, 66),
            (28, 72),
            (12, 88),
        ],
        7 => &[(6, 2), (96, 2), (44, 140)],
        8 => &[
            (50, 70),
            (25, 66),
            (10, 50),
            (12, 25),
            (28, 5),
            (53, 2),
            (78, 8),
            (92, 28),
            (90, 50),
            (75, 66),
            (50, 70),
            (25, 74),
            (10, 91),
            (12, 116),
            (28, 136),
            (53, 140),
            (78, 134),
            (92, 114),
            (90, 91),
            (75, 74),
            (50, 70),
        ],
        9 => &[
            (88, 75),
            (68, 86),
            (40, 82),
            (18, 68),
            (8, 45),
            (12, 22),
            (32, 5),
            (58, 2),
            (82, 12),
            (95, 34),
            (92, 72),
            (86, 112),
            (68, 133),
            (40, 140),
            (18, 132),
        ],
        _ => return,
    };

    unsafe {
        for points in path.windows(2) {
            let (x0, y0) = points[0];
            let (x1, y1) = points[1];
            draw_rounded_line(
                x as f32 + x0 as f32 * width as f32 / 100.0,
                y as f32 + y0 as f32 * height as f32 / 140.0,
                x as f32 + x1 as f32 * width as f32 / 100.0,
                y as f32 + y1 as f32 * height as f32 / 140.0,
                radius,
                color,
            );
        }
    }
}

unsafe fn draw_rounded_line(x0: f32, y0: f32, x1: f32, y1: f32, radius: f32, color: u32) {
    let width = unsafe { WIDTH };
    let height = unsafe { HEIGHT };
    if width == 0 || height == 0 {
        return;
    }

    let left = (x0.min(x1) - radius - 1.0).max(0.0) as usize;
    let top = (y0.min(y1) - radius - 1.0).max(0.0) as usize;
    let right = (x0.max(x1) + radius + 1.0).min((width - 1) as f32) as usize;
    let bottom = (y0.max(y1) + radius + 1.0).min((height - 1) as f32) as usize;
    let dx = x1 - x0;
    let dy = y1 - y0;
    let length_squared = dx * dx + dy * dy;
    let inner_squared = radius * radius;
    let outer_squared = (radius + 1.0) * (radius + 1.0);
    let buffer = unsafe { addr_of_mut!(FRAMEBUFFER.0) as *mut u32 };

    for py in top..=bottom {
        for px in left..=right {
            let center_x = px as f32 + 0.5;
            let center_y = py as f32 + 0.5;
            let projection = if length_squared == 0.0 {
                0.0
            } else {
                (((center_x - x0) * dx + (center_y - y0) * dy) / length_squared).clamp(0.0, 1.0)
            };
            let nearest_x = x0 + projection * dx;
            let nearest_y = y0 + projection * dy;
            let distance_squared = (center_x - nearest_x) * (center_x - nearest_x)
                + (center_y - nearest_y) * (center_y - nearest_y);
            let alpha = if distance_squared <= inner_squared {
                1.0
            } else if distance_squared < outer_squared {
                (outer_squared - distance_squared) / (outer_squared - inner_squared)
            } else {
                continue;
            };

            unsafe {
                let pixel = buffer.add(py * width + px);
                let background = read_volatile(pixel);
                write_volatile(pixel, blend_color(background, color, alpha));
            }
        }
    }
}

fn blend_color(background: u32, foreground: u32, alpha: f32) -> u32 {
    let mut result = 0xff00_0000;
    for shift in [0, 8, 16] {
        let background_channel = ((background >> shift) & 0xff) as f32;
        let foreground_channel = ((foreground >> shift) & 0xff) as f32;
        let channel =
            (background_channel + (foreground_channel - background_channel) * alpha) as u32;
        result |= channel << shift;
    }
    result
}

unsafe fn restore_pointer() {
    if !unsafe { POINTER_DRAWN } {
        return;
    }
    let width = unsafe { WIDTH };
    let height = unsafe { HEIGHT };
    let left = unsafe { POINTER_X as isize - POINTER_RADIUS };
    let top = unsafe { POINTER_Y as isize - POINTER_RADIUS };
    let saved = addr_of_mut!(POINTER_BACKGROUND) as *const u32;
    let buffer = unsafe { addr_of_mut!(FRAMEBUFFER.0) as *mut u32 };

    for row in 0..POINTER_DIAMETER {
        for column in 0..POINTER_DIAMETER {
            let x = left + column as isize;
            let y = top + row as isize;
            if x >= 0 && y >= 0 && x < width as isize && y < height as isize {
                unsafe {
                    write_volatile(
                        buffer.add(y as usize * width + x as usize),
                        read_volatile(saved.add(row * POINTER_DIAMETER + column)),
                    );
                }
            }
        }
    }
    unsafe { POINTER_DRAWN = false };
}

unsafe fn draw_pointer() {
    if !unsafe { POINTER_POSITIONED } {
        return;
    }
    let width = unsafe { WIDTH };
    let height = unsafe { HEIGHT };
    let left = unsafe { POINTER_X as isize - POINTER_RADIUS };
    let top = unsafe { POINTER_Y as isize - POINTER_RADIUS };
    let saved = addr_of_mut!(POINTER_BACKGROUND) as *mut u32;
    let buffer = unsafe { addr_of_mut!(FRAMEBUFFER.0) as *mut u32 };
    let radius_squared = (POINTER_RADIUS * POINTER_RADIUS) as usize;

    for row in 0..POINTER_DIAMETER {
        for column in 0..POINTER_DIAMETER {
            let x = left + column as isize;
            let y = top + row as isize;
            if x < 0 || y < 0 || x >= width as isize || y >= height as isize {
                continue;
            }
            let pixel = unsafe { buffer.add(y as usize * width + x as usize) };
            let background = unsafe { read_volatile(pixel) };
            unsafe {
                write_volatile(saved.add(row * POINTER_DIAMETER + column), background);
            }

            let dx = column as isize - POINTER_RADIUS;
            let dy = row as isize - POINTER_RADIUS;
            let distance_squared = (dx * dx + dy * dy) as usize;
            if distance_squared <= radius_squared {
                let alpha = if distance_squared <= 4 { 0.58 } else { 0.34 };
                unsafe {
                    write_volatile(pixel, blend_color(background, 0xfff2_f5ff, alpha));
                }
            }
        }
    }
    unsafe { POINTER_DRAWN = true };
}

fn flush_pointer_region(x: usize, y: usize) -> fmt::Result {
    let (width, height) = screen_size();
    let left = x.saturating_sub(POINTER_RADIUS as usize);
    let top = y.saturating_sub(POINTER_RADIUS as usize);
    virtio_gpu::flush_region(
        left,
        top,
        POINTER_DIAMETER.min(width.saturating_sub(left)),
        POINTER_DIAMETER.min(height.saturating_sub(top)),
    )
    .map_err(|_| fmt::Error)
}

fn wait_animation_frame() {
    let frequency: u64;
    let start: u64;
    unsafe {
        core::arch::asm!(
            "mrs {frequency}, cntfrq_el0",
            "mrs {start}, cntpct_el0",
            frequency = out(reg) frequency,
            start = out(reg) start,
            options(nomem, nostack, preserves_flags)
        );
    }
    let delay = frequency / 30;
    if delay == 0 {
        return;
    }
    loop {
        let current: u64;
        unsafe {
            core::arch::asm!(
                "mrs {current}, cntpct_el0",
                current = out(reg) current,
                options(nomem, nostack, preserves_flags)
            );
        }
        if current.wrapping_sub(start) >= delay {
            break;
        }
        core::hint::spin_loop();
    }
}

unsafe fn fill_rect(x: usize, y: usize, width: usize, height: usize, color: u32) {
    let screen_width = unsafe { WIDTH };
    let screen_height = unsafe { HEIGHT };
    let right = x.saturating_add(width).min(screen_width);
    let bottom = y.saturating_add(height).min(screen_height);
    let buffer = unsafe { addr_of_mut!(FRAMEBUFFER.0) as *mut u32 };
    for row in y.min(screen_height)..bottom {
        for column in x.min(screen_width)..right {
            unsafe {
                write_volatile(buffer.add(row * screen_width + column), color);
            }
        }
    }
}

unsafe fn fill_rounded_rect(
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    radius: usize,
    color: u32,
) {
    let screen_width = unsafe { WIDTH };
    let screen_height = unsafe { HEIGHT };
    if width == 0 || height == 0 {
        return;
    }

    let radius = radius.min(width / 2).min(height / 2);
    let buffer = unsafe { addr_of_mut!(FRAMEBUFFER.0) as *mut u32 };
    let right = x.saturating_add(width).min(screen_width);
    let bottom = y.saturating_add(height).min(screen_height);
    for py in y.min(screen_height)..bottom {
        for px in x.min(screen_width)..right {
            let center_x = if px < x + radius {
                x + radius
            } else if px >= x + width - radius {
                x + width - radius - 1
            } else {
                px
            };
            let center_y = if py < y + radius {
                y + radius
            } else if py >= y + height - radius {
                y + height - radius - 1
            } else {
                py
            };
            let dx = px.abs_diff(center_x);
            let dy = py.abs_diff(center_y);
            if dx * dx + dy * dy <= radius * radius {
                unsafe {
                    write_volatile(buffer.add(py * screen_width + px), color);
                }
            }
        }
    }
}

unsafe fn draw_centered(text: &str, y: usize, scale: usize, color: u32) {
    let width = unsafe { WIDTH };
    let text_width = text.chars().count().saturating_mul(CELL_WIDTH * scale);
    let x = width.saturating_sub(text_width) / 2;
    unsafe { draw_text(text, x, y, scale, color) };
}

unsafe fn draw_text(text: &str, x: usize, y: usize, scale: usize, color: u32) {
    let screen_width = unsafe { WIDTH };
    let screen_height = unsafe { HEIGHT };
    let buffer = unsafe { addr_of_mut!(FRAMEBUFFER.0) as *mut u32 };
    let mut character_x = x;

    for character in text.chars() {
        for (row, bits) in glyph(character).iter().enumerate() {
            for column in 0..5 {
                if bits & (1 << (4 - column)) == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let pixel_x = character_x + column * scale + dx;
                        let pixel_y = y + row * scale + dy;
                        if pixel_x < screen_width && pixel_y < screen_height {
                            unsafe {
                                write_volatile(buffer.add(pixel_y * screen_width + pixel_x), color);
                            }
                        }
                    }
                }
            }
        }
        character_x = character_x.saturating_add(CELL_WIDTH * scale);
    }
}

unsafe fn draw_text_rounded(text: &str, x: usize, y: usize, scale: usize, color: u32) {
    if scale == 0 {
        return;
    }

    let mut character_x = x;
    let radius = (scale as f32 * 0.62).max(0.65);
    for character in text.chars() {
        for (row, bits) in glyph(character).iter().enumerate() {
            for column in 0..5 {
                if bits & (1 << (4 - column)) != 0 {
                    let center_x = character_x + column * scale + scale / 2;
                    let center_y = y + row * scale + scale / 2;
                    unsafe {
                        draw_rounded_line(
                            center_x as f32,
                            center_y as f32,
                            center_x as f32,
                            center_y as f32,
                            radius,
                            color,
                        );
                    }
                }
            }
        }
        character_x = character_x.saturating_add(CELL_WIDTH * scale);
    }
}

unsafe fn fill_background() {
    unsafe {
        let buffer = addr_of_mut!(FRAMEBUFFER.0) as *mut u32;
        for index in 0..WIDTH * HEIGHT {
            write_volatile(buffer.add(index), BACKGROUND);
        }
    }
}

unsafe fn newline() {
    unsafe {
        CURSOR_X = 8;
        CURSOR_Y += CELL_HEIGHT;
        let bottom = if TERMINAL_ACTIVE && KEYBOARD_VISIBLE {
            keyboard_top()
        } else {
            HEIGHT
        };
        if CURSOR_Y + CELL_HEIGHT > bottom {
            scroll();
        }
    }
}

unsafe fn scroll() {
    let width = unsafe { WIDTH };
    let height = unsafe { HEIGHT };
    let bottom = if unsafe { TERMINAL_ACTIVE && KEYBOARD_VISIBLE } {
        keyboard_top()
    } else {
        height
    };
    let top = if unsafe { TERMINAL_ACTIVE } {
        52.min(height.saturating_sub(CELL_HEIGHT))
    } else {
        0
    };
    let buffer = unsafe { addr_of_mut!(FRAMEBUFFER.0) as *mut u32 };
    let rows_to_keep = bottom.saturating_sub(top + CELL_HEIGHT);
    let body_start = width * top;
    let body_end = body_start + width * rows_to_keep;
    let background = if unsafe { TERMINAL_ACTIVE } {
        APP_BACKGROUND
    } else {
        BACKGROUND
    };
    unsafe {
        copy(
            buffer.add(body_start + width * CELL_HEIGHT),
            buffer.add(body_start),
            width * rows_to_keep,
        );
        for index in body_end..width * bottom {
            write_volatile(buffer.add(index), background);
        }
        CURSOR_Y = top + rows_to_keep;
    }
}

unsafe fn draw_character(character: char) {
    if unsafe { CURSOR_X + CELL_WIDTH > WIDTH } {
        unsafe { newline() };
    }
    let bottom = if unsafe { TERMINAL_ACTIVE && KEYBOARD_VISIBLE } {
        keyboard_top()
    } else {
        unsafe { HEIGHT }
    };
    if unsafe { CURSOR_Y + CELL_HEIGHT > bottom } {
        unsafe { scroll() };
    }

    let glyph = glyph(character);
    let x = unsafe { CURSOR_X };
    let y = unsafe { CURSOR_Y };
    let width = unsafe { WIDTH };
    let buffer = unsafe { addr_of_mut!(FRAMEBUFFER.0) as *mut u32 };
    for (row, bits) in glyph.iter().enumerate() {
        for column in 0..5 {
            if bits & (1 << (4 - column)) != 0 {
                unsafe { write_volatile(buffer.add((y + row) * width + x + column), FOREGROUND) };
            }
        }
    }
    unsafe { CURSOR_X += CELL_WIDTH };
}

fn glyph(character: char) -> [u8; 7] {
    let upper = character.to_ascii_uppercase();
    if upper.is_ascii_uppercase() {
        FONT[(upper as u8 - b'A') as usize]
    } else if upper.is_ascii_digit() {
        FONT[26 + (upper as u8 - b'0') as usize]
    } else {
        match character {
            ' ' => [0; 7],
            '.' => [0, 0, 0, 0, 0, 12, 12],
            ',' => [0, 0, 0, 0, 4, 4, 8],
            ':' => [0, 4, 4, 0, 4, 4, 0],
            '-' => [0, 0, 0, 31, 0, 0, 0],
            '_' => [0, 0, 0, 0, 0, 0, 31],
            '!' => [4, 4, 4, 4, 4, 0, 4],
            '%' => [17, 2, 4, 8, 17, 0, 0],
            '?' => [14, 17, 1, 2, 4, 0, 4],
            '=' => [0, 31, 0, 31, 0, 0, 0],
            '<' => [4, 8, 16, 8, 4, 0, 0],
            '>' => [16, 8, 4, 8, 16, 0, 0],
            '\'' => [4, 4, 8, 0, 0, 0, 0],
            '(' => [2, 4, 8, 8, 8, 4, 2],
            ')' => [8, 4, 2, 2, 2, 4, 8],
            '/' => [1, 2, 2, 4, 8, 8, 16],
            _ => [14, 17, 1, 2, 4, 0, 4],
        }
    }
}

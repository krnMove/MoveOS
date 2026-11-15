use core::fmt::{self, Write};
use core::ptr::{addr_of_mut, copy, read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::virtio_gpu::Error as GpuError;
use crate::{fat32, virtio_gpu};

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
const ACCENT: u32 = 0xffe8_f0_f8;
const APP_BACKGROUND: u32 = LOCK_BACKGROUND;

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
static INITIALIZED: AtomicBool = AtomicBool::new(false);

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

pub fn print(arguments: fmt::Arguments<'_>) -> fmt::Result {
    ConsoleWriter.write_fmt(arguments)
}

pub fn clear() -> fmt::Result {
    if !is_initialized() {
        return Err(fmt::Error);
    }
    unsafe {
        fill_rect(0, 0, WIDTH, HEIGHT, APP_BACKGROUND);
        draw_status_bar("TERMINAL", TERMINAL_HOURS, TERMINAL_MINUTES, 4)?;
        CURSOR_X = 16;
        CURSOR_Y = 52;
        virtio_gpu::flush(WIDTH, HEIGHT).map_err(|_| fmt::Error)
    }
}

pub fn draw_lock_screen(hours: u8, minutes: u8) -> fmt::Result {
    if !is_initialized() {
        return Err(fmt::Error);
    }

    unsafe {
        fill_rect(0, 0, WIDTH, HEIGHT, 0xff00_0000);
        TERMINAL_ACTIVE = false;
        draw_lock_clock(hours, minutes);
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
        for frame in 0..=UNLOCK_FRAMES {
            fill_gradient(0, phase);
            let bar_y = frame as isize * 40 / UNLOCK_FRAMES as isize - 40;
            fill_rect_clipped_y(0, bar_y, WIDTH, 40, HEADER_BACKGROUND);
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
        fill_gradient(0, phase);
        TERMINAL_ACTIVE = false;
        draw_status_bar("", hours, minutes, status_elements)?;
        draw_desktop_icons(icon_scale, visible_icons);
        virtio_gpu::flush(WIDTH, HEIGHT).map_err(|_| fmt::Error)
    }
}

pub fn draw_terminal(hours: u8, minutes: u8) -> fmt::Result {
    if !is_initialized() {
        return Err(fmt::Error);
    }

    unsafe {
        TERMINAL_HOURS = hours;
        TERMINAL_MINUTES = minutes;
        TERMINAL_ACTIVE = true;
        fill_rect(0, 0, WIDTH, HEIGHT, APP_BACKGROUND);
        draw_status_bar("TERMINAL", hours, minutes, 4)?;
        CURSOR_X = 16;
        CURSOR_Y = 52;
        virtio_gpu::flush(WIDTH, HEIGHT).map_err(|_| fmt::Error)
    }
}

pub fn draw_move_files(
    hours: u8,
    minutes: u8,
    volume: Option<fat32::Fat32>,
    filesystem_error: Option<fat32::Error>,
    creating_name: Option<&str>,
    created: bool,
    already_exists: bool,
    create_error: Option<fat32::Error>,
) -> fmt::Result {
    if !is_initialized() {
        return Err(fmt::Error);
    }

    unsafe {
        TERMINAL_ACTIVE = false;
        fill_rect(0, 0, WIDTH, HEIGHT, APP_BACKGROUND);
        draw_status_bar("MOVE FILES", hours, minutes, 4)?;
        if let Some(name) = creating_name {
            draw_text("NEW FILE:", 24, 52, 2, ACCENT);
            draw_text(name, 24, 72, 2, FOREGROUND);
        } else if created {
            draw_text("CREATED", 24, 52, 2, ACCENT);
        } else if already_exists {
            draw_text("ALREADY EXISTS", 24, 52, 2, ACCENT);
        } else if create_error.is_some() {
            draw_text("CREATE ERROR", 24, 52, 2, ACCENT);
        } else {
            draw_text("FILES", 24, 52, 2, ACCENT);
        }

        if let Some(error) = create_error {
            CURSOR_X = 24;
            CURSOR_Y = 72;
            write!(&mut FramebufferWriter, "{}", error)?;
        }

        if let Some(volume) = volume {
            let list_y = if creating_name.is_some() || create_error.is_some() {
                100
            } else {
                72
            };
            draw_file_entries(volume, list_y)?;
        } else {
            CURSOR_X = 24;
            CURSOR_Y = if creating_name.is_some() { 100 } else { 72 };
            if let Some(error) = filesystem_error {
                write!(&mut FramebufferWriter, "{}", error)?;
            } else {
                write!(&mut FramebufferWriter, "NO VOLUME MOUNTED")?;
            }
        }
        virtio_gpu::flush(WIDTH, HEIGHT).map_err(|_| fmt::Error)
    }
}

unsafe fn draw_file_entries(volume: fat32::Fat32, top: usize) -> fmt::Result {
    unsafe {
        CURSOR_X = 24;
        CURSOR_Y = top;
    }

    let mut writer = FramebufferWriter;
    let mut count = 0;
    let mut truncated = false;
    let mut write_failed = false;
    let result = volume.list_root(|entry| {
        if unsafe { CURSOR_Y + CELL_HEIGHT > HEIGHT.saturating_sub(12) } {
            truncated = true;
            return false;
        }

        let suffix = if entry.is_directory() { "/" } else { "" };
        if writeln!(&mut writer, "{}{}", entry.name(), suffix).is_err() {
            write_failed = true;
            return false;
        }
        count += 1;
        true
    });

    if write_failed {
        return Err(fmt::Error);
    }
    match result {
        Ok(()) if count == 0 => {
            unsafe {
                CURSOR_X = 24;
                CURSOR_Y = top;
            }
            write!(&mut writer, "EMPTY")
        }
        Ok(()) if truncated => {
            unsafe { draw_text("MORE", 100, 52, 2, ACCENT) };
            Ok(())
        }
        Ok(()) => Ok(()),
        Err(error) => {
            writeln!(&mut writer, "READ ERROR: {}", error)
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

    for (index, label) in ["Terminal", "Move Files"]
        .into_iter()
        .enumerate()
        .take(visible_icons)
    {
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
            for character in text.chars() {
                if character == '\n' {
                    newline();
                } else if character == '\r' {
                    CURSOR_X = 8;
                } else {
                    draw_character(character);
                }
            }

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
        if CURSOR_Y + CELL_HEIGHT > HEIGHT {
            scroll();
        }
    }
}

unsafe fn scroll() {
    let width = unsafe { WIDTH };
    let height = unsafe { HEIGHT };
    let top = if unsafe { TERMINAL_ACTIVE } {
        52.min(height.saturating_sub(CELL_HEIGHT))
    } else {
        0
    };
    let buffer = unsafe { addr_of_mut!(FRAMEBUFFER.0) as *mut u32 };
    let rows_to_keep = height.saturating_sub(top + CELL_HEIGHT);
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
        for index in body_end..width * height {
            write_volatile(buffer.add(index), background);
        }
        CURSOR_Y = top + rows_to_keep;
    }
}

unsafe fn draw_character(character: char) {
    if unsafe { CURSOR_X + CELL_WIDTH > WIDTH } {
        unsafe { newline() };
    }
    if unsafe { CURSOR_Y + CELL_HEIGHT > HEIGHT } {
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

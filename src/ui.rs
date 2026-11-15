use core::fmt;

use crate::{console, fat32, scheduler, shell, virtio_keyboard};

enum Screen {
    Lock,
    Desktop,
    Files,
}

struct FilesState {
    creating: bool,
    name: [u8; 12],
    name_length: usize,
    created: bool,
    already_exists: bool,
    error: Option<fat32::Error>,
}

impl FilesState {
    const fn new() -> Self {
        Self {
            creating: false,
            name: [0; 12],
            name_length: 0,
            created: false,
            already_exists: false,
            error: None,
        }
    }

    fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_length]).expect("file name input must be ASCII")
    }

    fn draw(
        &self,
        hours: u8,
        minutes: u8,
        volume: Option<fat32::Fat32>,
        filesystem_error: Option<fat32::Error>,
    ) -> fmt::Result {
        console::draw_move_files(
            hours,
            minutes,
            volume,
            filesystem_error,
            if self.creating {
                Some(self.name())
            } else {
                None
            },
            self.created,
            self.already_exists,
            self.error,
        )
    }
}

pub fn run(volume: Option<fat32::Fat32>, filesystem_error: Option<fat32::Error>) -> ! {
    let (frequency, start) = timer_state();
    let mut screen = Screen::Lock;
    let mut last_minute = u64::MAX;
    let mut last_wallpaper_frame = u64::MAX;
    let mut files = FilesState::new();

    loop {
        scheduler::run_one();

        let ticks = elapsed_ticks(start);
        let elapsed = if frequency == 0 { 0 } else { ticks / frequency };
        let minute = elapsed / 60;
        let wallpaper_frame = if frequency == 0 {
            0
        } else {
            ticks / (frequency / 8).max(1)
        };
        let wallpaper_changed =
            matches!(screen, Screen::Desktop) && wallpaper_frame != last_wallpaper_frame;

        if minute != last_minute || wallpaper_changed {
            last_minute = minute;
            let (hours, minutes) = clock_time(minute);
            match screen {
                Screen::Lock => console::draw_lock_screen(hours, minutes)
                    .unwrap_or_else(|_| panic!("virtio-gpu lock screen drawing failed")),
                Screen::Desktop => {
                    console::draw_desktop(hours, minutes, (wallpaper_frame as u16).wrapping_mul(8))
                        .unwrap_or_else(|_| panic!("virtio-gpu desktop drawing failed"));
                    last_wallpaper_frame = wallpaper_frame;
                }
                Screen::Files => files
                    .draw(hours, minutes, volume, filesystem_error)
                    .unwrap_or_else(|_| panic!("virtio-gpu file placeholder drawing failed")),
            }
        }

        match virtio_keyboard::poll_key()
            .unwrap_or_else(|error| panic!("keyboard polling failed: {error}"))
        {
            Some(virtio_keyboard::Key::Up) if matches!(screen, Screen::Lock) => {
                screen = Screen::Desktop;
                let (hours, minutes) = clock_time(minute);
                console::unlock_to_desktop(
                    hours,
                    minutes,
                    (wallpaper_frame as u16).wrapping_mul(8),
                )
                .unwrap_or_else(|_| panic!("virtio-gpu desktop drawing failed"));
                last_minute = minute;
                last_wallpaper_frame = wallpaper_frame;
            }
            Some(virtio_keyboard::Key::Character('1')) if matches!(screen, Screen::Desktop) => {
                let (hours, minutes) = clock_time(minute);
                shell::run(volume, filesystem_error, hours, minutes);
                screen = Screen::Desktop;
                let resumed_ticks = elapsed_ticks(start);
                let resumed_elapsed = if frequency == 0 {
                    0
                } else {
                    resumed_ticks / frequency
                };
                let resumed_minute = resumed_elapsed / 60;
                let resumed_wallpaper_frame = if frequency == 0 {
                    0
                } else {
                    resumed_ticks / (frequency / 8).max(1)
                };
                let (hours, minutes) = clock_time(resumed_minute);
                console::draw_desktop(
                    hours,
                    minutes,
                    (resumed_wallpaper_frame as u16).wrapping_mul(8),
                )
                .unwrap_or_else(|_| panic!("virtio-gpu desktop drawing failed"));
                last_minute = resumed_minute;
                last_wallpaper_frame = resumed_wallpaper_frame;
            }
            Some(virtio_keyboard::Key::Character('2')) if matches!(screen, Screen::Desktop) => {
                screen = Screen::Files;
                files = FilesState::new();
                let (hours, minutes) = clock_time(minute);
                files
                    .draw(hours, minutes, volume, filesystem_error)
                    .unwrap_or_else(|_| panic!("virtio-gpu file placeholder drawing failed"));
                last_minute = minute;
            }
            Some(virtio_keyboard::Key::Character('n' | 'N'))
                if matches!(screen, Screen::Files) && !files.creating =>
            {
                files.creating = true;
                files.name_length = 0;
                files.created = false;
                files.already_exists = false;
                files.error = None;
                let (hours, minutes) = clock_time(minute);
                files
                    .draw(hours, minutes, volume, filesystem_error)
                    .unwrap_or_else(|_| panic!("virtio-gpu file creation screen drawing failed"));
            }
            Some(virtio_keyboard::Key::Character(character))
                if matches!(screen, Screen::Files) && files.creating =>
            {
                if files.name_length < files.name.len()
                    && character.is_ascii()
                    && (character.is_ascii_alphanumeric()
                        || b".$%'-_@~`!(){}^#&".contains(&(character as u8)))
                {
                    files.name[files.name_length] = character as u8;
                    files.name_length += 1;
                    files.error = None;
                    files.already_exists = false;
                    let (hours, minutes) = clock_time(minute);
                    files
                        .draw(hours, minutes, volume, filesystem_error)
                        .unwrap_or_else(|_| {
                            panic!("virtio-gpu file creation screen drawing failed")
                        });
                }
            }
            Some(virtio_keyboard::Key::Backspace)
                if matches!(screen, Screen::Files) && files.creating =>
            {
                if files.name_length > 0 {
                    files.name_length -= 1;
                } else {
                    files.creating = false;
                }
                let (hours, minutes) = clock_time(minute);
                files
                    .draw(hours, minutes, volume, filesystem_error)
                    .unwrap_or_else(|_| panic!("virtio-gpu file creation screen drawing failed"));
            }
            Some(virtio_keyboard::Key::Enter)
                if matches!(screen, Screen::Files) && files.creating && files.name_length > 0 =>
            {
                files.creating = false;
                files.created = false;
                files.already_exists = false;
                let name = files.name();
                files.error = match volume {
                    Some(volume) => match volume.entry_exists_83(name) {
                        Ok(true) => {
                            files.already_exists = true;
                            None
                        }
                        Ok(false) => match volume.write_file_83(name, &[]) {
                            Ok(()) => {
                                files.created = true;
                                None
                            }
                            Err(error) => Some(error),
                        },
                        Err(error) => Some(error),
                    },
                    None => None,
                };
                files.name_length = 0;
                let (hours, minutes) = clock_time(minute);
                files
                    .draw(hours, minutes, volume, filesystem_error)
                    .unwrap_or_else(|_| panic!("virtio-gpu file list drawing failed"));
            }
            Some(virtio_keyboard::Key::Escape)
                if matches!(screen, Screen::Files) && files.creating =>
            {
                files.creating = false;
                files.name_length = 0;
                files.error = None;
                files.already_exists = false;
                let (hours, minutes) = clock_time(minute);
                files
                    .draw(hours, minutes, volume, filesystem_error)
                    .unwrap_or_else(|_| panic!("virtio-gpu file list drawing failed"));
            }
            Some(virtio_keyboard::Key::Backspace | virtio_keyboard::Key::Escape)
                if matches!(screen, Screen::Files) =>
            {
                screen = Screen::Desktop;
                let (hours, minutes) = clock_time(minute);
                console::draw_desktop(hours, minutes, (wallpaper_frame as u16).wrapping_mul(8))
                    .unwrap_or_else(|_| panic!("virtio-gpu desktop drawing failed"));
                last_minute = minute;
                last_wallpaper_frame = wallpaper_frame;
            }
            _ => core::hint::spin_loop(),
        }
    }
}

fn clock_time(elapsed_minutes: u64) -> (u8, u8) {
    (
        ((elapsed_minutes / 60) % 24) as u8,
        (elapsed_minutes % 60) as u8,
    )
}

fn timer_state() -> (u64, u64) {
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
    (frequency, start)
}

fn elapsed_ticks(start: u64) -> u64 {
    let current: u64;
    unsafe {
        core::arch::asm!(
            "mrs {current}, cntpct_el0",
            current = out(reg) current,
            options(nomem, nostack, preserves_flags)
        );
    }
    current.wrapping_sub(start)
}

use core::fmt;

use crate::{console, fat32, scheduler, shell, virtio_keyboard};

enum Screen {
    Lock,
    Desktop,
    Files,
}

#[derive(Clone, Copy)]
struct TouchStart {
    x: u16,
    y: u16,
    ticks: u64,
}

#[derive(Clone, Copy)]
struct ContextMenu {
    app_index: usize,
    showing_info: bool,
}

struct FilesState {
    creating: bool,
    name: [u8; 12],
    name_length: usize,
    selected_index: usize,
    selected_name: [u8; 12],
    selected_name_length: usize,
    contents: [u8; 1024],
    content_length: usize,
    is_directory: bool,
    content_available: bool,
    editing: bool,
    dirty: bool,
    confirm_discard: bool,
    message: Option<&'static str>,
    error: Option<fat32::Error>,
}

impl FilesState {
    const fn new() -> Self {
        Self {
            creating: false,
            name: [0; 12],
            name_length: 0,
            selected_index: 0,
            selected_name: [0; 12],
            selected_name_length: 0,
            contents: [0; 1024],
            content_length: 0,
            is_directory: false,
            content_available: false,
            editing: false,
            dirty: false,
            confirm_discard: false,
            message: None,
            error: None,
        }
    }

    fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_length]).expect("file name input must be ASCII")
    }

    fn selected_name(&self) -> Option<&str> {
        if self.selected_name_length == 0 {
            None
        } else {
            Some(
                core::str::from_utf8(&self.selected_name[..self.selected_name_length])
                    .expect("FAT32 short filename must be ASCII"),
            )
        }
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
            console::MoveFilesView {
                creating_name: if self.creating {
                    Some(self.name())
                } else {
                    None
                },
                selected_index: self.selected_index,
                selected_name: self.selected_name(),
                is_directory: self.is_directory,
                contents: &self.contents[..self.content_length],
                editing: self.editing,
                dirty: self.dirty,
                confirm_discard: self.confirm_discard,
                keyboard: self.editing || self.creating,
                message: self.message,
                create_error: self.error,
            },
        )
    }

    fn refresh_selected(&mut self, volume: Option<fat32::Fat32>) {
        self.selected_name_length = 0;
        self.content_length = 0;
        self.is_directory = false;
        self.content_available = false;
        self.error = None;
        let Some(volume) = volume else {
            return;
        };
        let mut current = 0;
        let mut selected = None;
        if let Err(error) = volume.list_root(|entry| {
            if current == self.selected_index {
                selected = Some(entry);
                false
            } else {
                current += 1;
                true
            }
        }) {
            self.error = Some(error);
            return;
        }
        let Some(entry) = selected else {
            return;
        };
        self.selected_name_length = entry.name().copy_to(&mut self.selected_name);
        self.is_directory = entry.is_directory();
        if self.is_directory {
            return;
        }
        let mut name = [0; 12];
        name[..self.selected_name_length]
            .copy_from_slice(&self.selected_name[..self.selected_name_length]);
        let name = core::str::from_utf8(&name[..self.selected_name_length])
            .expect("selected filename must be ASCII");
        match volume.read_file_83(name, &mut self.contents) {
            Ok(length) => {
                self.content_length = length;
                self.content_available = true;
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn move_selection(&mut self, volume: Option<fat32::Fat32>, down: bool) {
        let Some(volume) = volume else {
            return;
        };
        let mut count = 0;
        let result = volume.list_root(|_| {
            count += 1;
            true
        });
        if let Err(error) = result {
            self.error = Some(error);
            return;
        }

        if count == 0 {
            self.selected_index = 0;
            self.refresh_selected(Some(volume));
            return;
        }
        let previous = self.selected_index;
        if down {
            self.selected_index = (self.selected_index + 1).min(count - 1);
        } else {
            self.selected_index = self.selected_index.saturating_sub(1);
        }
        if self.selected_index != previous {
            self.editing = false;
            self.dirty = false;
            self.confirm_discard = false;
            self.message = None;
            self.refresh_selected(Some(volume));
        }
    }

    fn select_index(&mut self, volume: Option<fat32::Fat32>, index: usize) {
        if self.selected_index == index {
            return;
        }
        self.selected_index = index;
        self.editing = false;
        self.dirty = false;
        self.confirm_discard = false;
        self.message = None;
        self.refresh_selected(volume);
    }

    fn save(&mut self, volume: Option<fat32::Fat32>) {
        let (Some(volume), Some(name)) = (volume, self.selected_name()) else {
            self.message = Some("NO FILE SELECTED");
            return;
        };
        match volume.write_file_83(name, &self.contents[..self.content_length]) {
            Ok(()) => {
                self.dirty = false;
                self.editing = false;
                self.confirm_discard = false;
                self.message = Some("SAVED");
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn discard(&mut self, volume: Option<fat32::Fat32>) {
        self.editing = false;
        self.dirty = false;
        self.confirm_discard = false;
        self.message = Some("CHANGES DISCARDED");
        self.refresh_selected(volume);
    }

    fn create_selected_index(&mut self, volume: fat32::Fat32) {
        let mut target = [0; 12];
        target[..self.name_length].copy_from_slice(&self.name[..self.name_length]);
        let target = &target[..self.name_length];
        let mut index = 0;
        let mut found = None;
        if let Err(error) = volume.list_root(|entry| {
            let mut name = [0; 12];
            let length = entry.name().copy_to(&mut name);
            if name[..length].eq_ignore_ascii_case(target) {
                found = Some(index);
                false
            } else {
                index += 1;
                true
            }
        }) {
            self.error = Some(error);
        }
        if let Some(index) = found {
            self.selected_index = index;
            self.refresh_selected(Some(volume));
        }
    }
}

pub fn run(volume: Option<fat32::Fat32>, filesystem_error: Option<fat32::Error>) -> ! {
    let (frequency, start) = timer_state();
    let mut screen = Screen::Lock;
    let mut last_minute = u64::MAX;
    let mut last_wallpaper_frame = u64::MAX;
    let mut files = FilesState::new();
    let mut touch_start = None;
    let mut context_menu: Option<ContextMenu> = None;

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
        let wallpaper_changed = matches!(screen, Screen::Desktop)
            && context_menu.is_none()
            && wallpaper_frame != last_wallpaper_frame;

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
            if let Some(menu) = context_menu {
                console::draw_context_menu(menu.app_index, menu.showing_info)
                    .unwrap_or_else(|_| panic!("virtio-gpu context menu drawing failed"));
            }
        }

        let input = virtio_keyboard::poll_input()
            .unwrap_or_else(|error| panic!("virtio input polling failed: {error}"));
        let touch_key = match input {
            Some(virtio_keyboard::Input::Pointer { x, y }) => {
                console::move_pointer(x, y)
                    .unwrap_or_else(|_| panic!("virtio-gpu pointer drawing failed"));
                None
            }
            Some(virtio_keyboard::Input::Touch {
                x,
                y,
                pressed: true,
            }) => {
                console::move_pointer(x, y)
                    .unwrap_or_else(|_| panic!("virtio-gpu pointer drawing failed"));
                touch_start = Some(TouchStart {
                    x,
                    y,
                    ticks: elapsed_ticks(start),
                });
                None
            }
            Some(virtio_keyboard::Input::Touch {
                x,
                y,
                pressed: false,
            }) => {
                console::move_pointer(x, y)
                    .unwrap_or_else(|_| panic!("virtio-gpu pointer drawing failed"));
                let start_touch = touch_start.take();
                process_touch(
                    start_touch,
                    x,
                    y,
                    &mut screen,
                    &mut files,
                    &mut context_menu,
                    volume,
                    filesystem_error,
                    frequency,
                    elapsed_ticks(start),
                    minute,
                    wallpaper_frame,
                )
            }
            _ => None,
        };
        let key = match input {
            Some(virtio_keyboard::Input::Key(key)) => Some(key),
            _ => touch_key,
        };
        match key {
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
                files.refresh_selected(volume);
                let (hours, minutes) = clock_time(minute);
                files
                    .draw(hours, minutes, volume, filesystem_error)
                    .unwrap_or_else(|_| panic!("virtio-gpu file placeholder drawing failed"));
                last_minute = minute;
            }
            Some(virtio_keyboard::Key::Character('n' | 'N'))
                if matches!(screen, Screen::Files) && !files.creating && !files.editing =>
            {
                files.creating = true;
                files.name_length = 0;
                files.message = None;
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
                    files.message = None;
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
            Some(virtio_keyboard::Key::Backspace)
                if matches!(screen, Screen::Files) && files.editing && !files.confirm_discard =>
            {
                if files.content_length > 0 {
                    files.content_length -= 1;
                    files.dirty = true;
                    files.message = None;
                }
                let (hours, minutes) = clock_time(minute);
                files
                    .draw(hours, minutes, volume, filesystem_error)
                    .unwrap_or_else(|_| panic!("virtio-gpu file editor drawing failed"));
            }
            Some(virtio_keyboard::Key::Enter)
                if matches!(screen, Screen::Files) && files.creating && files.name_length > 0 =>
            {
                files.creating = false;
                files.message = None;
                files.error = match volume {
                    Some(volume) => {
                        let name_length = files.name_length;
                        let mut name = [0; 12];
                        name[..name_length].copy_from_slice(&files.name[..name_length]);
                        let name = core::str::from_utf8(&name[..name_length])
                            .expect("file name input must be ASCII");
                        match volume.entry_exists_83(name) {
                            Ok(true) => {
                                files.message = Some("ALREADY EXISTS");
                                None
                            }
                            Ok(false) => match volume.write_file_83(name, &[]) {
                                Ok(()) => {
                                    files.message = Some("FILE CREATED");
                                    files.create_selected_index(volume);
                                    None
                                }
                                Err(error) => Some(error),
                            },
                            Err(error) => Some(error),
                        }
                    }
                    None => {
                        files.message = Some("NO VOLUME MOUNTED");
                        None
                    }
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
                files.message = None;
                let (hours, minutes) = clock_time(minute);
                files
                    .draw(hours, minutes, volume, filesystem_error)
                    .unwrap_or_else(|_| panic!("virtio-gpu file list drawing failed"));
            }
            Some(virtio_keyboard::Key::Up)
                if matches!(screen, Screen::Files) && !files.creating && !files.editing =>
            {
                files.move_selection(volume, false);
                let (hours, minutes) = clock_time(minute);
                files
                    .draw(hours, minutes, volume, filesystem_error)
                    .unwrap_or_else(|_| panic!("virtio-gpu file list drawing failed"));
            }
            Some(virtio_keyboard::Key::Down)
                if matches!(screen, Screen::Files) && !files.creating && !files.editing =>
            {
                files.move_selection(volume, true);
                let (hours, minutes) = clock_time(minute);
                files
                    .draw(hours, minutes, volume, filesystem_error)
                    .unwrap_or_else(|_| panic!("virtio-gpu file list drawing failed"));
            }
            Some(virtio_keyboard::Key::Character('e' | 'E'))
                if matches!(screen, Screen::Files) && !files.creating && !files.editing =>
            {
                if files.selected_name_length == 0 {
                    files.message = Some("SELECT A FILE");
                } else if files.is_directory {
                    files.message = Some("FOLDER EDITING NOT SUPPORTED");
                } else if !files.content_available {
                    files.message = Some("FILE CANNOT BE EDITED");
                } else if files.contents[..files.content_length]
                    .iter()
                    .all(|byte| byte.is_ascii())
                {
                    files.editing = true;
                    files.dirty = false;
                    files.confirm_discard = false;
                    files.message = None;
                    files.error = None;
                } else {
                    files.message = Some("ONLY ASCII TEXT CAN BE EDITED");
                }
                let (hours, minutes) = clock_time(minute);
                files
                    .draw(hours, minutes, volume, filesystem_error)
                    .unwrap_or_else(|_| panic!("virtio-gpu file editor drawing failed"));
            }
            Some(virtio_keyboard::Key::Save)
                if matches!(screen, Screen::Files) && files.editing =>
            {
                files.save(volume);
                let (hours, minutes) = clock_time(minute);
                files
                    .draw(hours, minutes, volume, filesystem_error)
                    .unwrap_or_else(|_| panic!("virtio-gpu file editor drawing failed"));
            }
            Some(virtio_keyboard::Key::Character('s' | 'S'))
                if matches!(screen, Screen::Files) && files.editing && files.confirm_discard =>
            {
                files.save(volume);
                let (hours, minutes) = clock_time(minute);
                files
                    .draw(hours, minutes, volume, filesystem_error)
                    .unwrap_or_else(|_| panic!("virtio-gpu file editor drawing failed"));
            }
            Some(virtio_keyboard::Key::Character('d' | 'D'))
                if matches!(screen, Screen::Files) && files.editing && files.confirm_discard =>
            {
                files.discard(volume);
                let (hours, minutes) = clock_time(minute);
                files
                    .draw(hours, minutes, volume, filesystem_error)
                    .unwrap_or_else(|_| panic!("virtio-gpu file editor drawing failed"));
            }
            Some(virtio_keyboard::Key::Character(character))
                if matches!(screen, Screen::Files) && files.editing && !files.confirm_discard =>
            {
                if character.is_ascii_graphic() || character == ' ' {
                    if files.content_length < files.contents.len() {
                        files.contents[files.content_length] = character as u8;
                        files.content_length += 1;
                        files.dirty = true;
                        files.message = None;
                    } else {
                        files.message = Some("FILE LIMIT: 1024 BYTES");
                    }
                }
                let (hours, minutes) = clock_time(minute);
                files
                    .draw(hours, minutes, volume, filesystem_error)
                    .unwrap_or_else(|_| panic!("virtio-gpu file editor drawing failed"));
            }
            Some(virtio_keyboard::Key::Enter)
                if matches!(screen, Screen::Files) && files.editing && !files.confirm_discard =>
            {
                if files.content_length < files.contents.len() {
                    files.contents[files.content_length] = b'\n';
                    files.content_length += 1;
                    files.dirty = true;
                    files.message = None;
                } else {
                    files.message = Some("FILE LIMIT: 1024 BYTES");
                }
                let (hours, minutes) = clock_time(minute);
                files
                    .draw(hours, minutes, volume, filesystem_error)
                    .unwrap_or_else(|_| panic!("virtio-gpu file editor drawing failed"));
            }
            Some(virtio_keyboard::Key::Escape)
                if matches!(screen, Screen::Files) && files.editing =>
            {
                if files.confirm_discard {
                    files.confirm_discard = false;
                } else if files.dirty {
                    files.confirm_discard = true;
                } else {
                    files.editing = false;
                }
                let (hours, minutes) = clock_time(minute);
                files
                    .draw(hours, minutes, volume, filesystem_error)
                    .unwrap_or_else(|_| panic!("virtio-gpu file editor drawing failed"));
            }
            Some(virtio_keyboard::Key::Backspace | virtio_keyboard::Key::Escape)
                if matches!(screen, Screen::Files) && !files.creating && !files.editing =>
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

#[allow(clippy::too_many_arguments)]
fn process_touch(
    start: Option<TouchStart>,
    x: u16,
    y: u16,
    screen: &mut Screen,
    files: &mut FilesState,
    context_menu: &mut Option<ContextMenu>,
    volume: Option<fat32::Fat32>,
    filesystem_error: Option<fat32::Error>,
    frequency: u64,
    now: u64,
    minute: u64,
    wallpaper_frame: u64,
) -> Option<virtio_keyboard::Key> {
    let start = start?;
    let (start_x, start_y) = console::touch_to_pixel(start.x, start.y);
    let (end_x, end_y) = console::touch_to_pixel(x, y);
    let dx = end_x as isize - start_x as isize;
    let dy = end_y as isize - start_y as isize;

    if let Some(menu) = *context_menu {
        match console::context_menu_choice(end_x, end_y, menu.showing_info) {
            Some(console::ContextAction::Info) => {
                if let Some(menu) = context_menu.as_mut() {
                    menu.showing_info = true;
                    console::draw_context_menu(menu.app_index, true)
                        .unwrap_or_else(|_| panic!("virtio-gpu context menu drawing failed"));
                }
            }
            Some(console::ContextAction::Remove) => {
                console::hide_desktop_icon(menu.app_index);
                *context_menu = None;
                draw_desktop_at(minute, wallpaper_frame);
            }
            Some(console::ContextAction::Close) | None => {
                *context_menu = None;
                draw_desktop_at(minute, wallpaper_frame);
            }
        }
        return None;
    }

    if matches!(screen, Screen::Lock) && dy < -80 && dy.unsigned_abs() > dx.unsigned_abs() {
        *screen = Screen::Desktop;
        let (hours, minutes) = clock_time(minute);
        console::unlock_to_desktop(hours, minutes, (wallpaper_frame as u16).wrapping_mul(8))
            .unwrap_or_else(|_| panic!("virtio-gpu desktop drawing failed"));
        return None;
    }

    if matches!(screen, Screen::Files)
        && !files.creating
        && !files.editing
        && dx > 80
        && dx.unsigned_abs() > dy.unsigned_abs()
    {
        *screen = Screen::Desktop;
        draw_desktop_at(minute, wallpaper_frame);
        return None;
    }

    if matches!(screen, Screen::Files)
        && let Some(key) = console::keyboard_key_at(x, y)
    {
        return Some(key);
    }

    if matches!(screen, Screen::Desktop) {
        let app_index = console::desktop_app_at(start_x, start_y)?;
        let held_ticks = now.wrapping_sub(start.ticks);
        if frequency > 0
            && held_ticks >= frequency.saturating_mul(3) / 5
            && console::desktop_app_at(end_x, end_y) == Some(app_index)
        {
            let menu = ContextMenu {
                app_index,
                showing_info: false,
            };
            *context_menu = Some(menu);
            console::draw_context_menu(app_index, false)
                .unwrap_or_else(|_| panic!("virtio-gpu context menu drawing failed"));
            return None;
        }
        if console::desktop_app_at(end_x, end_y) == Some(app_index) {
            return Some(virtio_keyboard::Key::Character(if app_index == 0 {
                '1'
            } else {
                '2'
            }));
        }
        return None;
    }

    if matches!(screen, Screen::Files) {
        if dx.unsigned_abs() > 80 || dy.unsigned_abs() > 80 {
            return None;
        }
        match console::files_touch_action_at(start_x, start_y, files.creating, files.selected_index)
        {
            Some(console::FilesTouchAction::Select(index)) => {
                let mut count = 0;
                if let Some(volume) = volume {
                    if let Err(error) = volume.list_root(|_| {
                        count += 1;
                        true
                    }) {
                        files.error = Some(error);
                    }
                    if index < count {
                        files.select_index(Some(volume), index);
                    }
                }
                files
                    .draw(
                        clock_time(minute).0,
                        clock_time(minute).1,
                        volume,
                        filesystem_error,
                    )
                    .unwrap_or_else(|_| panic!("virtio-gpu file list drawing failed"));
                return None;
            }
            Some(console::FilesTouchAction::Edit) if !files.editing && !files.creating => {
                return Some(virtio_keyboard::Key::Character('e'));
            }
            Some(console::FilesTouchAction::Edit) => {}
            Some(console::FilesTouchAction::Back) => {
                return Some(virtio_keyboard::Key::Escape);
            }
            None => {}
        }
    }

    None
}

fn draw_desktop_at(minute: u64, wallpaper_frame: u64) {
    let (hours, minutes) = clock_time(minute);
    console::draw_desktop(hours, minutes, (wallpaper_frame as u16).wrapping_mul(8))
        .unwrap_or_else(|_| panic!("virtio-gpu desktop drawing failed"));
}

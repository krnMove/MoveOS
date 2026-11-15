use crate::fat32::Fat32;

pub fn run(
    volume: Option<Fat32>,
    filesystem_error: Option<crate::fat32::Error>,
    hours: u8,
    minutes: u8,
) {
    crate::console::draw_terminal(hours, minutes)
        .unwrap_or_else(|_| panic!("virtio-gpu terminal drawing failed"));
    if let Some(error) = filesystem_error {
        crate::println!("Filesystem unavailable: {}", error)
            .unwrap_or_else(|_| panic!("virtio-gpu output failed"));
    } else if let Some(volume) = volume {
        crate::println!("Filesystem mounted: {} sectors", volume.total_sectors())
            .unwrap_or_else(|_| panic!("virtio-gpu output failed"));
    }

    crate::syscall::set_volume(volume);
    crate::user_shell::launch();
}

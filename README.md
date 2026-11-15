MoveOS:

Мобильная операционная система, построенная на собственной архитектуре ядра **MoveKernel (ARM64)**.

- **Текущая версия**: 0.1.1 (GUI-прототип)
- **Платформа**: QEMU `virt` (Cortex-A57), ARM64
- **Язык**: Rust (`no_std` / `no_main`)
- **Ядро**: [MoveKernel ARM](https://github.com/krnMove/MoveKernel-ARM)

Возможности:

### Графическая оболочка
- Экран блокировки (Lock Screen) с часами.
- Рабочий стол (Desktop) с сеткой приложений.
- Единый системный статус-бар (часы, Wi-Fi, батарея).
- Единая тёмная палитра и собственный шрифт 5x7.
- Плавные переходы между экранами (Slide).

Приложения:
- **Terminal**: полноэкранный терминал с доступом к FAT32. Команды: `/HELP`, `/TASKS`, `/LS`, `/CAT`, `/WRITE`, `/CLEAR`, `exit`.
- **Move Files**: файловый менеджер, отображающий корень FAT32. Создаёт пустые файлы 8.3 по клавише `N`. Файлы, созданные в Terminal, сразу видны в списке.

Ядро (MoveKernel):
- Загрузка в QEMU `virt` (Cortex-A57), свой загрузчик (`boot.s`) и линкер-скрипт.
- ARM MMU: identity-map для RAM, GIC и virtio MMIO.
- GICv3 (Generic Timer отключён).
- Кооперативный планировщик задач.
- Драйверы: virtio-gpu, virtio-keyboard, virtio-blk.
- FAT32: чтение, запись, создание и перезапись файлов в корне.
- 1 MiB heap с выравниванием и повторным использованием блоков.

Управление в QEMU:

| Действие | Клавиша |
| :--- | :--- |
| Разблокировать экран | `↑` (Up) |
| Открыть Terminal | `1` |
| Открыть Move Files | `2` |
| Вернуться на Desktop | `Esc` или `Backspace` |
| Создать файл в Move Files | `N`, затем имя 8.3 + `Enter` |
| Выход из Terminal в shell | `/exit` (или `Esc` на пустой строке) |

Запуск в QEMU:

Соберите ядро и тестовый FAT32-образ с файлом `HELLO.TXT` одной командой:

```powershell
powershell -ExecutionPolicy Bypass -File tools\build-arm.ps1

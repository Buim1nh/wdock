# wdock

[English](README.md) · **Tiếng Việt**

wdock là thanh menu phía trên (top bar) và dock kiểu macOS cho Windows 11, viết bằng Rust.

- Mục tiêu tài nguyên: RAM dưới 25 MB, CPU gần 0% khi rảnh, 0% khi đang chơi game.
- Không inject vào tiến trình khác và không dùng keyboard hook, nên an toàn với anti-cheat.

[![Video demo wdock](docs/media/demo.gif)](docs/media/demo.mp4)

*Video demo gồm dock phóng to, launcher, bảng phím tắt, lịch và Quick Settings. Bấm vào để xem bản MP4 1080p.*

## Ảnh chụp màn hình

Tất cả ảnh có độ phân giải 1920×1080.

| | |
|---|---|
| ![Desktop với top bar và dock](docs/media/desktop.png) | ![Dock phóng to](docs/media/dock.png) |
| Top bar và dock | Dock phóng to kèm nhãn tên |
| ![Launcher](docs/media/launcher.png) | ![Bảng phím tắt](docs/media/shortcuts.png) |
| Launcher (Win+Alt+Space) | Bảng phím tắt (Win+Alt+/) |
| ![Lịch](docs/media/calendar.png) | ![Quick Settings](docs/media/quick-settings.png) |
| Lịch | Quick Settings: mạng, âm lượng, độ sáng, pin, nguồn |

## Tính năng

- **Top bar:**
  - Nút launcher và nút mở bảng phím tắt.
  - Ô chọn workspace (mặc định 4 workspace).
  - Tiêu đề cửa sổ đang mở.
  - Khay hệ thống do wdock tự giữ.
  - Biểu tượng âm lượng, mạng, pin; bấm vào để mở Quick Settings.
  - Đồng hồ; bấm vào để mở lịch.
- **Dock:**
  - App ghim và app đang chạy, có chấm báo app đang chạy.
  - Phóng to kiểu macOS, có nhãn tên.
  - Preview cửa sổ trực tiếp.
  - Kéo thả để sắp xếp, có menu chuột phải.
  - Tự ẩn thông minh.
- **Launcher:** tìm gần đúng (fuzzy) trong các app đã cài (Win32 và Store) và các trang Settings.
- **Quản lý cửa sổ:** switcher (Alt+\`), overview (Win+\`) và workspace ảo.
- **Game mode:** khi một app toàn màn hình (game, F11 trên trình duyệt) ở phía trước, các thanh trên màn hình đó ngừng vẽ, ngừng chạy timer, và wdock cũng ngừng theo dõi việc di chuyển bên trong game.

## Cách hoạt động (chế độ lai)

Explorer vẫn là shell của Windows, wdock chạy song song với nó:

- Taskbar của explorer bị ẩn (auto-hide cộng SW_HIDE). Trạng thái gốc được lưu vào `%LOCALAPPDATA%\wdock\taskbar.txt` và trả lại khi wdock thoát.
- Top bar đăng ký làm AppBar, nên cửa sổ maximize không bị che.
- Dock mặc định ở chế độ smart: tự trượt xuống khi có cửa sổ đè lên, hiện lại khi đưa chuột xuống đáy màn hình.
- wdock tự giữ khay hệ thống (tray). Các thông điệp AppBar mà nó không xử lý thì chuyển tiếp cho explorer.
- Win+Shift+S, PrtScn, Win+X, ms-settings và các phím tắt khác của Windows vẫn dùng được như bình thường.

## Cài đặt

Để build cần [Rust toolchain](https://rustup.rs) (stable, edition 2024). Cài đặt không cần quyền admin.

```bash
powershell -ExecutionPolicy Bypass -File scripts\install.ps1
```

Script này làm các việc sau:

1. Build bản release.
2. Copy file chạy vào `%LOCALAPPDATA%\Programs\wdock\wdock.exe`.
3. Tạo scheduled task tên `wdock` cho tài khoản của bạn, chạy `"<exe>" --guard` mỗi lần đăng nhập. Dùng task thay cho khóa registry `Run` vì khi script chạy từ terminal bên trong một app dạng gói (MSIX), Windows chuyển hướng các thay đổi `HKCU` vào gói đó, nên explorer không thấy mục `Run`.
4. Khởi động wdock ngay.

Các tùy chọn:

- `-NoBuild`: dùng bản `target\release` đã có.
- `-NoStart`: chỉ cài, không khởi động.
- `-WithUsage`: cài thêm [agent-usage-bar](https://github.com/Buim1nh/agent-usage-bar), ứng dụng khay tùy chọn hiển thị hạn mức sử dụng của Claude, Codex và Antigravity (kèm biểu đồ đóng góp GitHub) trên thanh trên của wdock. Script dùng thư mục `agent-usage-bar` nằm cạnh repo này, tự clone nếu chưa có (`-UsageRepo <đường dẫn>` để chọn chỗ khác), và ứng dụng tự chạy khi đăng nhập bằng scheduled task riêng. Đây là chương trình tách riêng để bản thân wdock vẫn không giữ thông tin đăng nhập và không gọi mạng. Mặc định tắt.

Cần thoát wdock trước khi cài đè.

Tiến trình `--guard` chạy shell `--shell`:

- Nếu shell crash, guard khởi động lại nó sau khoảng 1 giây.
- Nếu shell crash 3 lần trong 60 giây, guard trả lại taskbar của explorer rồi dừng.

## Phím tắt

Tất cả đều dùng `RegisterHotKey` với tổ hợp không trùng explorer. Win+Alt+/ (hoặc nút bàn phím trên top bar) mở bảng tóm tắt.

| Phím | Chức năng |
|---|---|
| Win+Alt+Space (hoặc Ctrl+Alt+Space nếu còn trống) | Launcher |
| Alt+\` / Alt+Shift+\` | Chuyển cửa sổ (switcher) |
| Win+\` | Overview tất cả cửa sổ |
| Win+Ctrl+Alt+← / → / 1–9 | Chuyển workspace |
| Win+Alt+Shift+← / → / 1–9 | Mang cửa sổ đang mở sang workspace khác |
| Win+Alt+/ | Hiện hoặc ẩn bảng phím tắt (kéo được, nhớ vị trí) |
| Ctrl+Alt+Shift+Q | Thoát wdock, trả lại taskbar của explorer |

## Cấu hình

- File cấu hình là `%APPDATA%\wdock\config.toml`, được tạo tự động từ mẫu có chú thích ở lần chạy đầu.
- Lưu file dưới dạng UTF-8 không có BOM.
- Thay đổi chỉ có hiệu lực sau khi khởi động lại wdock: bấm Ctrl+Alt+Shift+Q rồi chạy `wdock.exe --guard`, hoặc đăng xuất rồi đăng nhập lại.
- Các mục: `[appearance]`, `[topbar]`, `[tray]`, `[dock]`, `[workspaces]`, `[switcher]`, `[performance]`.
- Trong `[dock]`:
  - `icon_size` mặc định là 32.
  - `magnification` mặc định là 2.0.
  - `auto_hide` nhận `"smart"`, `"on"` hoặc `"off"`. Với `"off"`, dock giữ cố định một dải ở đáy màn hình.

Dữ liệu và log nằm trong `%LOCALAPPDATA%\wdock\`:

- `logs\wdock.log` và `logs\wdock-guard.log`
- `pinned.txt` (app ghim)
- `hidden.txt` (cửa sổ ở workspace khác)
- `taskbar.txt`
- `shortcuts.txt`

## Gỡ cài đặt và khôi phục

Gỡ cài đặt:

```bash
powershell -ExecutionPolicy Bypass -File scripts\uninstall.ps1
```

Script này gỡ scheduled task (và mục `Run` của các bản cài cũ), dừng guard rồi dừng shell, sau đó trả lại taskbar của explorer và các cửa sổ đang ẩn ở workspace khác. Các tùy chọn:

- `-RemoveFiles`: xóa luôn file chạy.
- `-RemoveData`: xóa log, cache và trạng thái.

File cấu hình được giữ lại trong mọi trường hợp.

Nếu wdock bị kill mà taskbar không hiện lại, chạy:

```bash
"%LOCALAPPDATA%\Programs\wdock\wdock.exe" --restore-taskbar
```

wdock cũng tự trả lại taskbar khi tắt máy hoặc đăng xuất (WM_QUERYENDSESSION).

## Phát triển

- `cargo run --release -- --dev`: chạy kèm supervisor dev, tự khởi động lại khi crash, ghi log vào `wdock-dev.log`.
- `cargo test --release`: chạy unit test.
- `cargo clippy --release`: hiện không có cảnh báo nào.

Các chế độ dòng lệnh:

| Tham số | Chế độ |
|---|---|
| không có hoặc `--shell` | chạy shell |
| `--guard` | supervisor lúc đăng nhập |
| `--dev` | supervisor dev |
| `--restore-taskbar` | trả lại taskbar của explorer rồi thoát |

Mã nguồn nằm trong `crates/wdock/src`:

- `app.rs`: vòng lặp message, phím tắt, game mode.
- `ui/`: các thanh và popup vẽ bằng Direct2D.
- `platform/`: lớp bọc Win32, tray host, AppBar.
- `services/`: âm thanh, mạng, nguồn, độ sáng, chỉ mục app.
- `model/`: cấu hình, cửa sổ, khay, tìm kiếm.

## Kết quả đo

Đo trên Windows 11 với 2 màn hình 1080p, bản release.

| Hạng mục | Kết quả |
|---|---|
| Working set riêng (shell / guard) | ~13–21 MB / ~1.2 MB |
| CPU khi rảnh | 15–47 ms mỗi 60 giây (dưới 0,1%) |
| CPU khi có app toàn màn hình ở phía trước | 0 ms trong 20 giây |
| Thời gian phát hiện F11/toàn màn hình | ~15 ms |
| Rò rỉ (200 lần mở/đóng bảng phím tắt + 20 lần launcher) | handle ổn định ~614, GDI 40–43, bộ nhớ dừng ở ~32 MB private bytes |
| Shell crash | guard khởi động lại sau ~1 giây, đủ icon khay, taskbar vẫn ẩn |

## Giấy phép

MIT

# wdock

wdock là thanh menu phía trên (top bar) và dock kiểu macOS cho Windows 11, viết bằng Rust.

- Mục tiêu tài nguyên: RAM dưới 25 MB, CPU gần 0% khi rảnh, 0% khi đang chơi game.
- Không inject vào tiến trình khác và không dùng keyboard hook, nên an toàn với anti-cheat.

## Cách hoạt động (chế độ lai)

Explorer vẫn là shell của Windows. wdock chạy song song với nó:

- Taskbar của explorer bị ẩn (auto-hide cộng SW_HIDE). Trạng thái gốc được lưu vào `%LOCALAPPDATA%\wdock\taskbar.txt` và trả lại khi wdock thoát.
- Top bar đăng ký làm AppBar, nên cửa sổ maximize không bị che.
- Dock mặc định ở chế độ smart: nó trượt xuống khi có cửa sổ đè lên và hiện lại khi di chuột xuống đáy màn hình.
- wdock tự giữ khay hệ thống (tray). Các thông điệp AppBar mà nó không xử lý thì chuyển tiếp cho explorer.
- Khi một app toàn màn hình (game, F11) ở phía trước, các thanh trên màn hình đó tạm dừng hẳn: không vẽ, không timer.
- Win+Shift+S, PrtScn, Win+X, ms-settings và các phím tắt khác của Windows vẫn dùng được như bình thường.

## Cài đặt

Không cần quyền admin.

```bash
powershell -ExecutionPolicy Bypass -File scripts\install.ps1
```

Script này làm các việc sau:

1. Build bản release.
2. Copy file chạy vào `%LOCALAPPDATA%\Programs\wdock\wdock.exe`.
3. Thêm `HKCU\...\Run\wdock = "<exe>" --guard` để wdock tự chạy mỗi lần đăng nhập.
4. Khởi động wdock ngay.

Các tùy chọn:

- `-NoBuild`: dùng bản `target\release` đã có.
- `-NoStart`: chỉ cài, không khởi động.

Cần thoát wdock trước khi cài đè.

Tiến trình `--guard` chạy shell `--shell`:

- Nếu shell crash, guard khởi động lại nó sau khoảng 1 giây.
- Nếu shell crash 3 lần trong 60 giây, guard trả lại taskbar của explorer rồi dừng.

## Phím tắt

Tất cả đều dùng `RegisterHotKey` với tổ hợp không trùng explorer. `Win+Alt+/` mở bảng tóm tắt; nút bàn phím trên top bar cũng mở bảng này.

| Phím | Chức năng |
|---|---|
| Win+Alt+Space (hoặc Ctrl+Alt+Space nếu còn trống) | Launcher tìm app |
| Alt+` / Alt+Shift+` | Chuyển cửa sổ (switcher) |
| Win+` | Overview tất cả cửa sổ |
| Win+Ctrl+Alt+← / → / 1–9 | Chuyển workspace |
| Win+Alt+Shift+← / → / 1–9 | Mang cửa sổ đang mở sang workspace khác |
| Win+Alt+/ | Hiện/ẩn bảng phím tắt (kéo được, nhớ vị trí) |
| Ctrl+Alt+Shift+Q | Thoát wdock, trả lại taskbar của explorer |

## Cấu hình

- File cấu hình: `%APPDATA%\wdock\config.toml`. Nó được tạo tự động từ mẫu có chú thích. Sau khi sửa, cần khởi động lại wdock: thoát bằng Ctrl+Alt+Shift+Q rồi chạy `wdock.exe --guard`, hoặc đăng xuất rồi đăng nhập lại.
- File phải là UTF-8 không có BOM.
- Các mục chính: `[appearance]`, `[topbar]`, `[tray]`, `[dock]`, `[workspaces]`, `[switcher]`, `[performance]`.
- Trong `[dock]`:
  - `icon_size` mặc định là 32.
  - `magnification` mặc định là 2.0.
  - `auto_hide` nhận `"smart"`, `"on"` hoặc `"off"`. Với `"off"`, dock giữ chỗ cố định ở đáy màn hình.

Dữ liệu và log nằm ở `%LOCALAPPDATA%\wdock\`:

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

Script gỡ mục tự chạy, dừng guard rồi dừng shell, sau đó trả lại taskbar của explorer và các cửa sổ đang ẩn ở workspace khác. Các tùy chọn:

- `-RemoveFiles`: xóa luôn file chạy.
- `-RemoveData`: xóa log, cache và trạng thái.

File cấu hình được giữ lại trong cả hai trường hợp.

Nếu wdock bị kill mà taskbar không hiện lại, chạy lệnh sau:

```bash
"%LOCALAPPDATA%\Programs\wdock\wdock.exe" --restore-taskbar
```

Khi máy shutdown hoặc sign-out, wdock cũng tự trả lại taskbar (WM_QUERYENDSESSION).

## Phát triển

- `cargo run --release -- --dev`: chạy kèm supervisor dev, tự khởi động lại khi crash và log vào `wdock-dev.log`.
- `cargo clippy --release`: hiện không còn cảnh báo nào.

Các chế độ dòng lệnh:

- Không tham số hoặc `--shell`: chạy shell.
- `--guard`: chạy guard.
- `--dev`: chạy supervisor dev.
- `--restore-taskbar`: trả lại taskbar của explorer.

## Kết quả đo (M8, Windows 11, 2 màn hình)

| Hạng mục | Kết quả |
|---|---|
| Working set riêng (shell / guard) | ~21 MB / ~1.2 MB |
| CPU khi rảnh | ~15 ms trong 60 s |
| CPU khi app toàn màn hình ở trước | 0 ms trong 20 s |
| Phát hiện F11/toàn màn hình | ~15 ms |
| Rò rỉ (200 lần mở/đóng bảng phím tắt + 20 lần launcher) | handle ổn định ~614, GDI 40–43, bộ nhớ dừng ở ~32 MB private bytes |
| Crash shell | guard khởi động lại sau ~1 s, đủ icon khay, taskbar vẫn ẩn |

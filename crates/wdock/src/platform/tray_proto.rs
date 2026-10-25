//! Wire format of the notification-area protocol.
//!
//! `Shell_NotifyIcon` finds the window of class `Shell_TrayWnd` and sends it
//! `WM_COPYDATA`. `dwData` selects the payload:
//!   0 = AppBar message, 1 = `TRAYNOTIFYDATA`, 3 = `Shell_NotifyIconGetRect`.
//! The embedded NOTIFYICONDATA always uses the 32-bit layout (handles are
//! DWORDs, sign-extended back to pointers) even when sent by 64-bit apps.
//!
//! Parsing is pure and bounds-checked so it can be unit tested with byte
//! buffers; truncated (older, smaller) structures yield only the fields present.

pub const COPYDATA_APPBAR: usize = 0;
pub const COPYDATA_TRAY: usize = 1;
pub const COPYDATA_ICON_RECT: usize = 3;

const SIGNATURE: u32 = 0x3475_3423;

pub const NIF_MESSAGE: u32 = 0x01;
pub const NIF_ICON: u32 = 0x02;
pub const NIF_TIP: u32 = 0x04;
pub const NIF_STATE: u32 = 0x08;
pub const NIF_INFO: u32 = 0x10;
pub const NIF_GUID: u32 = 0x20;
pub const NIF_SHOWTIP: u32 = 0x80;

pub const NIS_HIDDEN: u32 = 0x01;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    Add,
    Modify,
    Delete,
    SetFocus,
    SetVersion,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotifyIconData {
    pub hwnd: isize,
    pub uid: u32,
    pub flags: u32,
    pub callback_message: u32,
    pub hicon: isize,
    pub tip: String,
    pub state: u32,
    pub state_mask: u32,
    pub info: String,
    /// `uTimeout` for balloons, `uVersion` for NIM_SETVERSION.
    pub timeout_or_version: u32,
    pub info_title: String,
    pub info_flags: u32,
    pub guid: Option<[u8; 16]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayMessage {
    pub command: TrayCommand,
    pub data: NotifyIconData,
}

/// `Shell_NotifyIconGetRect` asks for one corner per call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconRectQuery {
    /// 1 = top-left, 2 = bottom-right.
    pub corner: u32,
    pub hwnd: isize,
    pub uid: u32,
    pub guid: Option<[u8; 16]>,
}

// Offsets inside the 32-bit NOTIFYICONDATAW (956 bytes in full).
mod nid {
    pub const HWND: usize = 4;
    pub const UID: usize = 8;
    pub const FLAGS: usize = 12;
    pub const CALLBACK: usize = 16;
    pub const HICON: usize = 20;
    pub const TIP: usize = 24;
    pub const TIP_CHARS: usize = 128;
    pub const STATE: usize = 280;
    pub const STATE_MASK: usize = 284;
    pub const INFO: usize = 288;
    pub const INFO_CHARS: usize = 256;
    pub const TIMEOUT: usize = 800;
    pub const INFO_TITLE: usize = 804;
    pub const INFO_TITLE_CHARS: usize = 64;
    pub const INFO_FLAGS: usize = 932;
    pub const GUID: usize = 936;
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    bytes.get(offset..offset + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// 32-bit handle values are sign-extended, as the system does.
fn handle_at(bytes: &[u8], offset: usize) -> Option<isize> {
    u32_at(bytes, offset).map(|v| v as i32 as isize)
}

fn guid_at(bytes: &[u8], offset: usize) -> Option<[u8; 16]> {
    let raw: [u8; 16] = bytes.get(offset..offset + 16)?.try_into().ok()?;
    (raw != [0; 16]).then_some(raw)
}

/// NUL-terminated UTF-16 in a fixed-size field; missing fields read as empty.
fn wstr_at(bytes: &[u8], offset: usize, max_chars: usize) -> String {
    let Some(field) = bytes.get(offset..(offset + max_chars * 2).min(bytes.len())) else {
        return String::new();
    };
    let units: Vec<u16> = field
        .as_chunks::<2>().0.iter()
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

pub fn parse_tray(bytes: &[u8]) -> Option<TrayMessage> {
    if u32_at(bytes, 0)? != SIGNATURE {
        return None;
    }
    let command = match u32_at(bytes, 4)? {
        0 => TrayCommand::Add,
        1 => TrayCommand::Modify,
        2 => TrayCommand::Delete,
        3 => TrayCommand::SetFocus,
        4 => TrayCommand::SetVersion,
        _ => return None,
    };
    let n = bytes.get(8..)?;
    let flags = u32_at(n, nid::FLAGS)?;
    let data = NotifyIconData {
        hwnd: handle_at(n, nid::HWND)?,
        uid: u32_at(n, nid::UID)?,
        flags,
        callback_message: u32_at(n, nid::CALLBACK).unwrap_or(0),
        hicon: handle_at(n, nid::HICON).unwrap_or(0),
        tip: wstr_at(n, nid::TIP, nid::TIP_CHARS),
        state: u32_at(n, nid::STATE).unwrap_or(0),
        state_mask: u32_at(n, nid::STATE_MASK).unwrap_or(0),
        info: wstr_at(n, nid::INFO, nid::INFO_CHARS),
        timeout_or_version: u32_at(n, nid::TIMEOUT).unwrap_or(0),
        info_title: wstr_at(n, nid::INFO_TITLE, nid::INFO_TITLE_CHARS),
        info_flags: u32_at(n, nid::INFO_FLAGS).unwrap_or(0),
        guid: if flags & NIF_GUID != 0 { guid_at(n, nid::GUID) } else { None },
    };
    Some(TrayMessage { command, data })
}

pub fn parse_icon_rect_query(bytes: &[u8]) -> Option<IconRectQuery> {
    if u32_at(bytes, 0)? != SIGNATURE {
        return None;
    }
    Some(IconRectQuery {
        corner: u32_at(bytes, 4)?,
        hwnd: handle_at(bytes, 16)?,
        uid: u32_at(bytes, 20)?,
        guid: guid_at(bytes, 24),
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Builds a TRAYNOTIFYDATA buffer the way shell32 lays it out.
    pub fn encode(command: u32, data: &NotifyIconData) -> Vec<u8> {
        let mut n = vec![0u8; 956];
        let put = |n: &mut Vec<u8>, at: usize, v: u32| n[at..at + 4].copy_from_slice(&v.to_le_bytes());
        let put_str = |n: &mut Vec<u8>, at: usize, s: &str| {
            for (i, unit) in s.encode_utf16().enumerate() {
                n[at + i * 2..at + i * 2 + 2].copy_from_slice(&unit.to_le_bytes());
            }
        };
        put(&mut n, 0, 956);
        put(&mut n, nid::HWND, data.hwnd as u32);
        put(&mut n, nid::UID, data.uid);
        put(&mut n, nid::FLAGS, data.flags);
        put(&mut n, nid::CALLBACK, data.callback_message);
        put(&mut n, nid::HICON, data.hicon as u32);
        put_str(&mut n, nid::TIP, &data.tip);
        put(&mut n, nid::STATE, data.state);
        put(&mut n, nid::STATE_MASK, data.state_mask);
        put_str(&mut n, nid::INFO, &data.info);
        put(&mut n, nid::TIMEOUT, data.timeout_or_version);
        put_str(&mut n, nid::INFO_TITLE, &data.info_title);
        put(&mut n, nid::INFO_FLAGS, data.info_flags);
        if let Some(guid) = data.guid {
            n[nid::GUID..nid::GUID + 16].copy_from_slice(&guid);
        }
        let mut out = Vec::new();
        out.extend_from_slice(&SIGNATURE.to_le_bytes());
        out.extend_from_slice(&command.to_le_bytes());
        out.extend(n);
        out
    }

    #[test]
    fn round_trips_full_structure() {
        let data = NotifyIconData {
            hwnd: 0x1234,
            uid: 7,
            flags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_GUID,
            callback_message: 0x8001,
            hicon: 0x5678,
            tip: "Discord".into(),
            info: "hello".into(),
            info_title: "Title".into(),
            guid: Some([9; 16]),
            ..Default::default()
        };
        let parsed = parse_tray(&encode(0, &data)).unwrap();
        assert_eq!(parsed.command, TrayCommand::Add);
        assert_eq!(parsed.data, data);
    }

    #[test]
    fn handles_are_sign_extended() {
        let data = NotifyIconData { hwnd: -2, uid: 1, ..Default::default() };
        assert_eq!(parse_tray(&encode(2, &data)).unwrap().data.hwnd, -2);
    }

    #[test]
    fn guid_ignored_without_flag() {
        let data = NotifyIconData { guid: Some([1; 16]), ..Default::default() };
        assert_eq!(parse_tray(&encode(1, &data)).unwrap().data.guid, None);
    }

    #[test]
    fn truncated_v1_structure_parses() {
        // NOTIFYICONDATA v1 ends after a 64-char tip (88 bytes).
        let data = NotifyIconData { uid: 3, flags: NIF_TIP, tip: "old app".into(), ..Default::default() };
        let mut bytes = encode(0, &data);
        bytes.truncate(8 + 88);
        let parsed = parse_tray(&bytes).unwrap();
        assert_eq!(parsed.data.tip, "old app");
        assert_eq!(parsed.data.info, "");
    }

    #[test]
    fn rejects_bad_signature_and_command() {
        let mut bytes = encode(0, &NotifyIconData::default());
        bytes[0] = 0;
        assert!(parse_tray(&bytes).is_none());
        assert!(parse_tray(&encode(9, &NotifyIconData::default())).is_none());
    }

    #[test]
    fn parses_icon_rect_query() {
        let mut bytes = vec![0u8; 40];
        bytes[0..4].copy_from_slice(&SIGNATURE.to_le_bytes());
        bytes[4..8].copy_from_slice(&2u32.to_le_bytes());
        bytes[16..20].copy_from_slice(&0x42u32.to_le_bytes());
        bytes[20..24].copy_from_slice(&5u32.to_le_bytes());
        let query = parse_icon_rect_query(&bytes).unwrap();
        assert_eq!(query, IconRectQuery { corner: 2, hwnd: 0x42, uid: 5, guid: None });
    }
}

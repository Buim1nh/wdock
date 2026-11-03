//! Searchable, bounded file/folder bookmarks. Only references are stored.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER};
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_UP, VK_NEXT, VK_PRIOR};
use crate::model::{config::Backdrop, island_files::{self, Bookmark}};
use crate::platform::{bookmarks, clipboard, launch, monitors};
use crate::ui::{bar::{self, Button, PaintCx, View}, gfx::{rect, rounded}, text};

const WIDTH: f32 = 600.0;
const HEIGHT: f32 = 566.0;
const LIST_TOP: f32 = 148.0;
const ROW: f32 = 48.0;
const VISIBLE: usize = 6;
const CMD_INSPECT: usize = 0x424D;
const CMD_PASTE: usize = 0x4250;
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action { Add, NewGroup, Refresh, Filter, Rename, DeleteGroup, Open, Reveal, Copy, Pin, Move, Remove }
const ACTIONS: [Action; 12] = [Action::Add, Action::NewGroup, Action::Refresh, Action::Filter, Action::Rename, Action::DeleteGroup, Action::Open, Action::Reveal, Action::Copy, Action::Pin, Action::Move, Action::Remove];
#[derive(Clone, PartialEq, Eq)]
enum Mode { Search, Add, NewGroup, Rename(String), Move(PathBuf) }
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit { Action(Action), Row(usize) }
type CheckResults = Arc<Mutex<Option<Vec<(PathBuf, bool)>>>>;
type PasteResult = Arc<Mutex<Option<(u64, Option<String>)>>>;
pub struct BookmarksView {
    hwnd: HWND,
    anchor: RECT,
    monitor: RECT,
    scale: f32,
    width: f32,
    rows: usize,
    query: String,
    input: String,
    mode: Mode,
    items: Vec<Bookmark>,
    groups: Vec<String>,
    filter: usize,
    selected: usize,
    scroll: usize,
    hot: Option<Hit>,
    focus: Option<usize>,
    notice: String,
    exists: HashMap<PathBuf, bool>,
    checks: CheckResults,
    lifetime: Arc<()>,
    paste: PasteResult,
    epoch: u64,
    select_all: bool,
}
impl BookmarksView {
    pub fn new(anchor: RECT, scale: f32) -> Self {
        island_files::init();
        let monitors = monitors::enumerate();
        let x = anchor.left + (anchor.right-anchor.left)/2; let y = anchor.top + (anchor.bottom-anchor.top)/2;
        let monitor = monitors.iter().find(|m| x >= m.rect.left && x < m.rect.right && y >= m.rect.top && y < m.rect.bottom)
            .or_else(|| monitors.first()).map(|m| m.rect).unwrap_or(RECT { left:0,top:0,right:1920,bottom:1080 });
        Self::with_monitor(anchor, monitor, scale)
    }
    fn with_monitor(anchor: RECT, monitor: RECT, scale: f32) -> Self {
        let width = WIDTH.min((monitor.right-monitor.left) as f32/scale - 16.0).max(200.0);
        let rows = (((monitor.bottom-monitor.top) as f32/scale - 16.0 - 278.0)/ROW).floor().max(1.0) as usize;
        let rows = rows.min(VISIBLE);
        let mut view = Self {
            hwnd: HWND::default(), anchor, monitor, scale, width, rows, query: String::new(), input: String::new(), mode: Mode::Search,
            items: Vec::new(), groups: Vec::new(), filter: 0, selected: 0, scroll: 0, hot: None, focus: None,
            notice: String::new(), exists: HashMap::new(), checks: Arc::new(Mutex::new(None)), lifetime: Arc::new(()), paste: Arc::new(Mutex::new(None)), epoch: 0, select_all: false,
        };
        view.reload(); view
    }
    fn reload(&mut self) {
        let selected_path = self.items.get(self.selected).map(|i| i.path.clone());
        self.groups = island_files::groups();
        self.filter = self.filter.min(self.groups.len() + 1);
        self.items = island_files::entries(&self.query).into_iter().filter(|i| match self.filter {
            0 => true, 1 => i.group.is_none(), n => i.group.as_ref() == self.groups.get(n - 2),
        }).collect();
        self.selected = selected_path.and_then(|p| self.items.iter().position(|i| i.path == p)).unwrap_or(self.selected).min(self.items.len().saturating_sub(1));
        self.ensure_visible();
    }
    fn ensure_visible(&mut self) {
        if self.selected < self.scroll { self.scroll = self.selected; }
        if self.selected >= self.scroll + self.rows { self.scroll = self.selected + 1 - self.rows; }
        self.scroll = self.scroll.min(self.items.len().saturating_sub(self.rows));
    }
    fn group(&self) -> Option<&str> { self.filter.checked_sub(2).and_then(|n| self.groups.get(n)).map(String::as_str) }
    fn filter_label(&self) -> &str { match self.filter { 0 => "All bookmarks", 1 => "Ungrouped", _ => self.group().unwrap_or("All bookmarks") } }
    fn row_rect(&self, row: usize) -> D2D_RECT_F {
        let top = (LIST_TOP + row as f32 * ROW) * self.scale;
        rect(14.0*self.scale, top, (self.width-14.0)*self.scale, top + ROW*self.scale)
    }
    fn action_rect(&self, action: Action) -> D2D_RECT_F {
        let column = match action {
            Action::Add | Action::Filter | Action::Open | Action::Pin => 0,
            Action::NewGroup | Action::Rename | Action::Reveal | Action::Move => 1,
            _ => 2,
        };
        let w = (self.width-48.0)/3.0;
        let x = 14.0 + column as f32*(w+10.0);
        let footer = LIST_TOP+self.rows as f32*ROW+12.0;
        let y = match action {
            Action::Add | Action::NewGroup | Action::Refresh => 84.0,
            Action::Filter | Action::Rename | Action::DeleteGroup => 116.0,
            Action::Open | Action::Reveal | Action::Copy => footer,
            _ => footer+36.0,
        };
        rect(x*self.scale,y*self.scale,(x+w)*self.scale,(y+28.0)*self.scale)
    }
    fn label(&self, action: Action) -> String {
        match action {
            Action::Add => "Add path…", Action::NewGroup => "New group…", Action::Refresh => "Check paths",
            Action::Filter => return format!("Group: {}  ›", self.filter_label()), Action::Rename => "Rename group…", Action::DeleteGroup => "Delete group",
            Action::Open => "Open ↵", Action::Reveal => "Show in folder", Action::Copy => "Copy path",
            Action::Pin => if self.items.get(self.selected).is_some_and(|i| i.pinned) { "Unpin" } else { "Pin to top" },
            Action::Move => "Move to group…", Action::Remove => "Remove reference",
        }.into()
    }
    fn enabled(&self, action: Action) -> bool {
        match action {
            Action::Refresh => !bookmarks::checking(),
            Action::Filter => true,
            _ if island_files::error().is_some() => false,
            Action::Rename | Action::DeleteGroup => self.group().is_some(),
            Action::Open | Action::Reveal | Action::Copy | Action::Pin | Action::Move | Action::Remove => !self.items.is_empty(),
            _ => true,
        }
    }
    fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        let contains = |r: D2D_RECT_F| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
        if let Some(a) = ACTIONS.into_iter().find(|&a| self.enabled(a) && contains(self.action_rect(a))) { return Some(Hit::Action(a)); }
        (self.scroll..self.items.len()).take(self.rows).enumerate().find(|(r,_)| contains(self.row_rect(*r))).map(|(_,i)| Hit::Row(i))
    }
    fn check_paths(&mut self) {
        let results = self.checks.clone(); let alive = Arc::downgrade(&self.lifetime); let hwnd = self.hwnd.0 as usize;
        let started = bookmarks::inspect(island_files::list(), move |checked| {
            if alive.upgrade().is_none() { return; }
            if let Ok(mut slot) = results.lock() { *slot = Some(checked); }
            bar::post_command(HWND(hwnd as *mut _), CMD_INSPECT);
        });
        self.notice = if started { "Checking local paths…" } else { "A path check is already running, or its worker could not start." }.into();
    }
    fn paste_clipboard(&mut self) {
        let results = self.paste.clone(); let alive = Arc::downgrade(&self.lifetime);
        let hwnd = self.hwnd.0 as usize; let epoch = self.epoch;
        let started = clipboard::read_text_async(move |text| {
            if alive.upgrade().is_none() { return; }
            if let Ok(mut slot) = results.lock() { *slot = Some((epoch,text)); }
            bar::post_command(HWND(hwnd as *mut _), CMD_PASTE);
        });
        self.notice = if started { "Reading clipboard…" } else { "Clipboard is busy. Try pasting again." }.into();
    }
    fn changed(&mut self, result: Result<(), String>, success: &str) {
        match result {
            Ok(()) => { self.notice = success.into(); self.reload(); crate::ui::island::notify_views(); }
            Err(e) => self.notice = e,
        }
    }
    fn start_input(&mut self, mode: Mode, value: String) { self.epoch = self.epoch.wrapping_add(1); self.mode = mode; self.select_all = !value.is_empty(); self.input = value; self.focus = None; self.notice.clear(); }
    fn activate(&mut self, action: Action) {
        if !self.enabled(action) { return; }
        let item = self.items.get(self.selected).cloned();
        match action {
            Action::Add => self.start_input(Mode::Add, String::new()),
            Action::NewGroup => self.start_input(Mode::NewGroup, String::new()),
            Action::Rename => if let Some(group) = self.group().map(str::to_owned) { self.start_input(Mode::Rename(group.clone()), group); },
            Action::DeleteGroup => if let Some(group) = self.group().map(str::to_owned) { self.changed(island_files::delete_group(&group), "Group deleted. Its bookmarks are now Ungrouped."); self.filter = 1; self.reload(); },
            Action::Filter => { self.filter = (self.filter + 1) % (self.groups.len()+2); self.selected = 0; self.scroll = 0; self.reload(); },
            Action::Refresh => self.check_paths(),
            _ => if let Some(item) = item { match action {
                Action::Open | Action::Reveal => {
                    if action == Action::Open { launch::open(&item.path.to_string_lossy()); } else { bookmarks::reveal(item.path); }
                    self.notice = "Opened in Windows. If unavailable, reconnect its drive and check paths.".into();
                }
                Action::Copy => { self.notice = match clipboard::write_text(self.hwnd, &item.path.to_string_lossy()) { Ok(()) => "Path copied.".into(), Err(e) => format!("Cannot copy path: {e}") }; }
                Action::Pin => self.changed(island_files::toggle_pin(&item.path), "Bookmark order updated."),
                Action::Move => self.start_input(Mode::Move(item.path), item.group.unwrap_or_default()),
                Action::Remove => self.changed(island_files::remove_path(&item.path), "Reference removed. The file or folder is unchanged."),
                _ => {}
            } }
        }
    }
    fn submit(&mut self) {
        let input = self.input.trim().to_owned();
        let result = match &self.mode {
            Mode::Search => { self.activate(self.focus.map_or(Action::Open, |i| ACTIONS[i])); return; }
            Mode::Add => island_files::add(PathBuf::from(input.trim_matches('"'))),
            Mode::NewGroup => island_files::add_group(&input),
            Mode::Rename(old) => island_files::rename_group(old, &input),
            Mode::Move(path) => island_files::move_to(path, (!input.is_empty()).then_some(input.as_str())),
        };
        if result.is_ok() { self.epoch=self.epoch.wrapping_add(1); self.mode = Mode::Search; self.input.clear(); self.select_all=false; }
        self.changed(result, "Saved.");
    }
}
impl View for BookmarksView {
    fn attach(&mut self, hwnd: HWND) { self.hwnd = hwnd; }
    fn frame(&self) -> Option<RECT> {
        let width = (self.width*self.scale).round() as i32; let height = ((HEIGHT-(VISIBLE-self.rows) as f32*ROW)*self.scale).round() as i32;
        let m = self.monitor;
        let left = ((self.anchor.left+self.anchor.right-width)/2).clamp(m.left,(m.right-width).max(m.left));
        let top = (self.anchor.bottom+(8.0*self.scale).round() as i32).clamp(m.top,(m.bottom-height).max(m.top));
        Some(RECT { left, top, right:left+width, bottom:top+height })
    }
    fn paint(&mut self, cx: &PaintCx) {
        let s = cx.scale; let t = cx.theme;
        let fill = |r, color| { if let Ok(b) = cx.gfx.brush(color) { unsafe { cx.dc.FillRoundedRectangle(&rounded(r,8.0*s), &b); } } };
        if t.backdrop == Backdrop::Solid { fill(rect(0.0,0.0,cx.width,cx.height),t.solid_fill); }
        let (Ok(title),Ok(body),Ok(detail)) = (
            text::format_px(&cx.gfx.dwrite,text::UI_FONT,17.0*s,DWRITE_FONT_WEIGHT_SEMI_BOLD,DWRITE_TEXT_ALIGNMENT_LEADING,false),
            text::format_px(&cx.gfx.dwrite,text::UI_FONT,13.0*s,DWRITE_FONT_WEIGHT_NORMAL,DWRITE_TEXT_ALIGNMENT_LEADING,false),
            text::format_px(&cx.gfx.dwrite,text::UI_FONT,11.0*s,DWRITE_FONT_WEIGHT_NORMAL,DWRITE_TEXT_ALIGNMENT_LEADING,false),
        ) else { return; };
        if let Ok(sign) = unsafe { cx.gfx.dwrite.CreateEllipsisTrimmingSign(&body) } {
            for format in [&title, &body, &detail] { let _ = unsafe { format.SetTrimming(&DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() }, &sign) }; }
        }
        let draw = |label: &str, format, r, color| text::draw(cx,label,format,r,color);
        draw(match &self.mode { Mode::Search => "Bookmarks", Mode::Add => "Add bookmark", Mode::NewGroup => "New group", Mode::Rename(_) => "Rename group", Mode::Move(_) => "Move bookmark" },&title,rect(18.0*s,10.0*s,(self.width*0.45-10.0)*s,38.0*s),t.text);
        draw(&format!("{} / 32 references · files stay in place",island_files::list().len()),&detail,rect(self.width*0.45*s,10.0*s,(self.width-14.0)*s,38.0*s),t.text_dim);
        let field = rect(14.0*s,42.0*s,(self.width-14.0)*s,78.0*s); fill(field,if self.select_all { t.accent } else { t.control });
        let (value, placeholder) = match &self.mode {
            Mode::Search => (&self.query,"Search name, path or group"), Mode::Add => (&self.input,"Absolute file or folder path · Enter saves"),
            Mode::NewGroup => (&self.input,"New group name · Enter saves"), Mode::Rename(_) => (&self.input,"Rename group"),
            Mode::Move(_) => (&self.input,"Group name · empty = Ungrouped · Enter saves"),
        };
        draw(if value.is_empty() { placeholder } else { value },&body,rect(field.left+10.0*s,field.top,field.right-10.0*s,field.bottom),if self.select_all { t.on_accent } else if value.is_empty() { t.text_dim } else { t.text });
        for (n,a) in ACTIONS.into_iter().enumerate() {
            let r = self.action_rect(a); let enabled = self.enabled(a);
            let focused = self.focus == Some(n);
            fill(r,if focused { t.accent } else if self.hot == Some(Hit::Action(a)) && enabled { t.hover } else { t.control });
            draw(&self.label(a),&detail,rect(r.left+10.0*s,r.top,r.right-5.0*s,r.bottom),if focused { t.on_accent } else if enabled { t.text } else { t.text_dim });
        }
        for (row,index) in (self.scroll..self.items.len()).take(self.rows).enumerate() {
            let item = &self.items[index]; let r = self.row_rect(row); let selected = index == self.selected;
            if selected { fill(r,t.accent); } else if self.hot == Some(Hit::Row(index)) { fill(r,t.hover); }
            let color = if selected { t.on_accent } else { t.text };
            let dim = if selected { t.on_accent } else { t.text_dim };
            let name = item.path.file_name().unwrap_or(item.path.as_os_str()).to_string_lossy();
            let prefix = if item.pinned { "★  " } else { "" };
            let missing = if self.exists.get(&item.path) == Some(&false) { " · Unavailable" } else { "" };
            draw(&format!("{prefix}{name}{missing}"),&body,rect(r.left+10.0*s,r.top+2.0*s,r.right-10.0*s,r.top+26.0*s),color);
            draw(&format!("{} · {}",item.group.as_deref().unwrap_or("Ungrouped"),item.path.display()),&detail,rect(r.left+10.0*s,r.top+24.0*s,r.right-10.0*s,r.bottom-2.0*s),dim);
        }
        if self.items.is_empty() {
            let (headline,help) = if island_files::error().is_some() { ("Bookmarks could not be loaded","Original data preserved. Fix bookmarks.toml and restart wdock.") }
                else if !self.query.is_empty() || self.filter != 0 { ("No matching bookmarks","Clear the search or choose another group.") }
                else { ("Keep useful files and folders close","Add an absolute path above, or drop files onto Island → Files.") };
            let top = LIST_TOP + ((self.rows as f32*ROW-70.0)/2.0).max(0.0);
            draw(headline,&title,rect(28.0*s,top*s,(self.width-28.0)*s,(top+30.0)*s),t.text);
            draw(help,&body,rect(28.0*s,(top+30.0)*s,(self.width-28.0)*s,(top+64.0).min(LIST_TOP+self.rows as f32*ROW)*s),t.text_dim);
        }
        let status = island_files::error().unwrap_or_else(|| if !self.notice.is_empty() { self.notice.clone() } else if self.mode != Mode::Search { "Enter saves · Esc cancels · Move: existing group name, or empty for Ungrouped".into() } else { format!("↑↓ select · Enter open · Tab actions · Esc close · {} shown",self.items.len()) });
        let footer = LIST_TOP+self.rows as f32*ROW+84.0;
        draw(&status,&detail,rect(18.0*s,footer*s,(self.width-18.0)*s,(footer+38.0)*s),t.text_dim);
    }
    fn char_input(&mut self, c: char) -> bool {
        if c == '\u{1}' { self.select_all=true; self.focus=None; return true; }
        if c == '\u{16}' { self.paste_clipboard(); return true; }
        if c == '\u{1b}' { if self.mode != Mode::Search { self.epoch=self.epoch.wrapping_add(1); self.mode=Mode::Search; self.input.clear(); self.select_all=false; } else { bar::close(self.hwnd); } return true; }
        if c == '\r' { self.submit(); return true; }
        if c == '\t' { let start=self.focus.map_or(0, |i| (i+1)%ACTIONS.len()); self.focus=(0..ACTIONS.len()).map(|n|(start+n)%ACTIONS.len()).find(|&i|self.enabled(ACTIONS[i])); return true; }
        let value = if self.mode == Mode::Search { &mut self.query } else { &mut self.input };
        if self.select_all && (!c.is_control() || c=='\u{8}') { value.clear(); self.select_all=false; }
        match c { '\u{8}' => { value.pop(); }, c if c.is_control() => return false, c if value.chars().count() < 32768 => value.push(c), _ => return false }
        self.epoch=self.epoch.wrapping_add(1); self.notice.clear(); self.focus=None; if self.mode == Mode::Search { self.selected=0; self.scroll=0; self.reload(); } true
    }
    fn key_down(&mut self, vk: u16) -> bool {
        let max = self.items.len().saturating_sub(1);
        self.selected = match vk { v if v==VK_DOWN.0 => (self.selected+1).min(max), v if v==VK_UP.0 => self.selected.saturating_sub(1), v if v==VK_NEXT.0 => (self.selected+self.rows).min(max), v if v==VK_PRIOR.0 => self.selected.saturating_sub(self.rows), _ => return false };
        self.select_all=false; self.focus=None; self.ensure_visible(); true
    }
    fn mouse_move(&mut self,x:f32,y:f32)->bool { let hit=self.hit(x,y); std::mem::replace(&mut self.hot,hit)!=hit }
    fn mouse_leave(&mut self)->bool { self.hot.take().is_some() }
    fn mouse_up(&mut self,x:f32,y:f32,button:Button)->bool {
        if button!=Button::Left { return false; } self.focus=None;
        match self.hit(x,y) { Some(Hit::Row(i)) => { self.selected=i; }, Some(Hit::Action(a)) => self.activate(a), None => return false } true
    }
    fn double_click(&mut self,x:f32,y:f32,button:Button)->bool {
        if button==Button::Left && let Some(Hit::Row(i))=self.hit(x,y) { self.selected=i; self.activate(Action::Open); return true; } false
    }
    fn wheel(&mut self,_x:f32,_y:f32,delta:i16)->bool { self.key_down(if delta>0 { VK_UP.0 } else { VK_DOWN.0 }) }
    fn command(&mut self,id:usize)->bool {
        if id == CMD_PASTE {
            let result = self.paste.lock().ok().and_then(|mut r| r.take());
            if let Some((epoch,text)) = result {
                if epoch != self.epoch { return false; }
                let Some(text) = text else { self.notice="Clipboard has no readable text. Try copying the path again.".into(); return true; };
                let text = text.trim();
                if self.mode != Mode::Search && text.chars().any(char::is_control) { self.notice="Paste a single path or group name.".into(); return true; }
                let value = if self.mode == Mode::Search { &mut self.query } else { &mut self.input };
                if self.select_all { value.clear(); self.select_all=false; }
                let available = 32768usize.saturating_sub(value.chars().count());
                value.extend(text.chars().map(|c| if c.is_control() { ' ' } else { c }).take(available));
                self.epoch=self.epoch.wrapping_add(1); self.focus=None; self.notice.clear();
                if self.mode == Mode::Search { self.selected=0; self.scroll=0; self.reload(); }
            }
            return true;
        }
        if id!=CMD_INSPECT { return false; }
        if let Ok(mut results)=self.checks.lock() && let Some(results)=results.take() { self.exists=results.into_iter().collect(); self.notice="Local paths checked. Unavailable references are kept.".into(); }
        true
    }
    fn model_changed(&mut self)->bool { self.reload(); true }
    fn deactivated(&mut self)->bool { true }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn popup_actions_fit_laptops_and_secondary_monitors_at_fractional_dpi() {
        island_files::init_empty();
        for scale in [1.0, 1.25, 1.5, 2.0] {
            for monitor in [RECT { left:0,top:0,right:1366,bottom:768 }, RECT { left:-1920,top:0,right:0,bottom:1080 }] {
                let anchor = RECT { left:monitor.left+100,top:monitor.top+20,right:monitor.left+100,bottom:monitor.top+20 };
                let view = BookmarksView::with_monitor(anchor,monitor,scale);
                let frame = view.frame().unwrap();
                assert!(frame.left >= monitor.left && frame.right <= monitor.right);
                assert!(frame.top >= monitor.top && frame.bottom <= monitor.bottom);
                for action in ACTIONS { let r = view.action_rect(action); assert!(r.left >= 0.0 && r.right <= (frame.right-frame.left) as f32+0.5); assert!(r.top >= 0.0 && r.bottom <= (frame.bottom-frame.top) as f32); }
                assert!(view.row_rect(view.rows-1).bottom < view.action_rect(Action::Open).top);
            }
        }
    }
    #[test]
    fn paste_replaces_selection_and_discards_late_results_for_old_fields() {
        island_files::init_empty();
        let monitor = RECT { left:0,top:0,right:1366,bottom:768 };
        let mut view = BookmarksView::with_monitor(monitor,monitor,1.5);
        view.start_input(Mode::Add, r"C:\old".into());
        *view.paste.lock().unwrap() = Some((view.epoch,Some(r"D:\new".into())));
        assert!(view.command(CMD_PASTE));
        assert_eq!(view.input,r"D:\new");
        let old_epoch=view.epoch;
        view.start_input(Mode::NewGroup,String::new());
        *view.paste.lock().unwrap()=Some((old_epoch,Some("old field text".into())));
        assert!(!view.command(CMD_PASTE));
        assert!(view.input.is_empty());
        view.start_input(Mode::Rename("Work".into()),"Work".into());
        view.char_input('N');
        assert_eq!(view.input,"N");
        view.char_input('\u{1}'); view.char_input('\u{8}');
        assert!(view.input.is_empty());
    }
    #[test]
    fn keyboard_tabs_skip_disabled_actions_and_arrows_scroll_selection() {
        island_files::init_empty();
        let monitor = RECT { left:0,top:0,right:1366,bottom:768 };
        let mut view = BookmarksView::with_monitor(monitor,monitor,1.5);
        assert!(view.char_input('\t'));
        assert!(!view.key_down(0x09));
        assert_eq!(view.focus, Some(0));
        for _ in 0..20 { view.char_input('\t'); assert!(view.enabled(ACTIONS[view.focus.unwrap()])); }
        for i in 0..10 { island_files::add(PathBuf::from(format!(r"C:\{i}"))).unwrap(); }
        view.reload();
        for _ in 0..9 { view.key_down(VK_DOWN.0); }
        assert_eq!(view.selected,9);
        assert_eq!(view.scroll,10-view.rows);
    }
}

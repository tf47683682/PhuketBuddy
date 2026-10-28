use serde::{Deserialize, Serialize};
use tauri::{PhysicalPosition, PhysicalSize, WebviewWindow};
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};

#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
}

/// 取指定屏幕坐标所在显示器的工作区（已排除任务栏），单位：物理像素。
pub fn work_area_at(x: i32, y: i32) -> Rect {
    unsafe {
        let pt = POINT { x, y };
        let hmon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(hmon, &mut info) != 0 {
            let r = info.rcWork;
            Rect { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top }
        } else {
            Rect { x: 0, y: 0, w: 1920, h: 1040 }
        }
    }
}

pub fn left_button_down() -> bool {
    unsafe { (GetAsyncKeyState(VK_LBUTTON as i32) as u16 & 0x8000) != 0 }
}

/// 前台窗口信息（pid + 标题），用于判断焦点去了哪里。
pub fn foreground_info() -> String {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
    };
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return "none".into();
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        let mut buf = [0u16; 128];
        let len = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        let title = String::from_utf16_lossy(&buf[..len.max(0) as usize]);
        format!("pid={pid} ours={} title={title}", pid == std::process::id())
    }
}

pub fn foreground_is_ours() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return false;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        pid == std::process::id()
    }
}

/// 前台是否是桌面 / 任务栏（点这些地方应该收起面板）
pub fn foreground_is_desktop() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetForegroundWindow};
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return false;
        }
        let mut buf = [0u16; 64];
        let len = GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        let class = String::from_utf16_lossy(&buf[..len.max(0) as usize]);
        class == "Progman" || class == "WorkerW" || class == "Shell_TrayWnd"
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
pub struct SavedPos {
    pub x: i32,
    pub y: i32,
}

fn pos_file() -> std::path::PathBuf {
    crate::secure::data_dir().join("position.json")
}

pub fn save_pos(x: i32, y: i32) {
    if let Ok(b) = serde_json::to_vec(&SavedPos { x, y }) {
        let _ = std::fs::write(pos_file(), b);
    }
}

pub fn load_pos() -> Option<SavedPos> {
    let b = std::fs::read(pos_file()).ok()?;
    serde_json::from_slice(&b).ok()
}

/// 气泡默认位置：主屏右侧、偏上。
pub fn default_pos(size: PhysicalSize<u32>) -> PhysicalPosition<i32> {
    let area = work_area_at(0, 0);
    let x = area.right() - size.width as i32 - 24;
    let y = area.y + (area.h as f64 * 0.32) as i32;
    PhysicalPosition::new(x, y)
}

/* ---------- 贴边半隐藏 ---------- */

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

/// 当前最近的一条屏幕边
pub fn nearest_edge(win: &WebviewWindow) -> Option<Edge> {
    let (pos, size) = (win.outer_position().ok()?, win.outer_size().ok()?);
    let area = work_area_at(pos.x + size.width as i32 / 2, pos.y + size.height as i32 / 2);
    let dl = pos.x - area.x;
    let dr = area.right() - (pos.x + size.width as i32);
    let dt = pos.y - area.y;
    let db = area.bottom() - (pos.y + size.height as i32);
    let m = dl.min(dr).min(dt).min(db);
    Some(if m == dl {
        Edge::Left
    } else if m == dr {
        Edge::Right
    } else if m == dt {
        Edge::Top
    } else {
        Edge::Bottom
    })
}

/// 把气泡摆到指定边：完全显示 = 82×82；半隐藏 = 只留一条 14px 宽的窗口贴边。
/// 用「缩窗口」而不是「移出屏幕」，保证露出的部分仍然能接收鼠标事件。
pub fn apply_dock(win: &WebviewWindow, edge: Edge, hidden: bool) {
    let (Ok(pos), Ok(_size), Ok(scale)) =
        (win.outer_position(), win.outer_size(), win.scale_factor())
    else {
        return;
    };
    let area = work_area_at(pos.x + 40, pos.y + 40);
    let gap = (6.0 * scale) as i32;
    let full = 82.0;
    let peek = 14.0;

    let (w_log, h_log) = match (edge, hidden) {
        (Edge::Left, true) | (Edge::Right, true) => (peek, full),
        (Edge::Top, true) | (Edge::Bottom, true) => (full, peek),
        _ => (full, full),
    };
    let _ = win.set_size(tauri::LogicalSize::new(w_log, h_log));

    let w = (w_log * scale) as i32;
    let h = (h_log * scale) as i32;

    let (x, y) = match (edge, hidden) {
        (Edge::Left, false) => (area.x + gap, pos.y),
        (Edge::Left, true) => (area.x, pos.y),
        (Edge::Right, false) => (area.right() - w - gap, pos.y),
        (Edge::Right, true) => (area.right() - w, pos.y),
        (Edge::Top, false) => (pos.x, area.y + gap),
        (Edge::Top, true) => (pos.x, area.y),
        (Edge::Bottom, false) => (pos.x, area.bottom() - h - gap),
        (Edge::Bottom, true) => (pos.x, area.bottom() - h),
    };
    let _ = win.set_position(PhysicalPosition::new(x, y));
}

/// 自动贴边：总是吸附到最近的一条屏幕边（保留 6px 间隙）。
pub fn snap(win: &WebviewWindow) -> bool {
    let (Ok(pos), Ok(size), Ok(scale)) =
        (win.outer_position(), win.outer_size(), win.scale_factor())
    else {
        return false;
    };
    let cx = pos.x + size.width as i32 / 2;
    let cy = pos.y + size.height as i32 / 2;
    let area = work_area_at(cx, cy);

    let gap = (6.0 * scale) as i32;

    let d_left = pos.x - area.x;
    let d_right = area.right() - (pos.x + size.width as i32);
    let d_top = pos.y - area.y;
    let d_bottom = area.bottom() - (pos.y + size.height as i32);

    let mut x = pos.x;
    let mut y = pos.y;
    let m = d_left.min(d_right).min(d_top).min(d_bottom);
    if m == d_left {
        x = area.x + gap;
    } else if m == d_right {
        x = area.right() - size.width as i32 - gap;
    } else if m == d_top {
        y = area.y + gap;
    } else {
        y = area.bottom() - size.height as i32 - gap;
    }

    x = x.clamp(area.x + gap, area.right() - size.width as i32 - gap);
    y = y.clamp(area.y + gap, area.bottom() - size.height as i32 - gap);

    if x != pos.x || y != pos.y {
        let _ = win.set_position(PhysicalPosition::new(x, y));
    }
    save_pos(x, y);
    true
}

/// 按气泡位置把面板贴在其左侧或右侧，并保证不越出工作区。
pub fn place_panel(panel: &WebviewWindow, bubble: &WebviewWindow) {
    let (Ok(bpos), Ok(bsize), Ok(psize)) =
        (bubble.outer_position(), bubble.outer_size(), panel.outer_size())
    else {
        return;
    };
    let scale = panel.scale_factor().unwrap_or(1.0);
    // 半隐藏时气泡窗口只有 14px 宽，按完整尺寸计算面板位置
    let bw = if (bsize.width as f64) < 40.0 * scale {
        (82.0 * scale) as u32
    } else {
        bsize.width
    };
    let gap = (12.0 * scale) as i32;
    let margin = (8.0 * scale) as i32;

    let area = work_area_at(bpos.x + bw as i32 / 2, bpos.y + bsize.height as i32 / 2);

    let right_space = area.right() - (bpos.x + bw as i32);
    let left_space = bpos.x - area.x;

    let mut x = if right_space >= psize.width as i32 + gap || left_space < psize.width as i32 + gap {
        bpos.x + bw as i32 + gap
    } else {
        bpos.x - psize.width as i32 - gap
    };
    let mut y = bpos.y;

    if x + psize.width as i32 > area.right() - margin {
        x = bpos.x - psize.width as i32 - gap;
    }
    if x < area.x + margin {
        x = area.x + margin;
    }
    if y + psize.height as i32 > area.bottom() - margin {
        y = area.bottom() - psize.height as i32 - margin;
    }
    if y < area.y + margin {
        y = area.y + margin;
    }

    let _ = panel.set_position(PhysicalPosition::new(x, y));
}

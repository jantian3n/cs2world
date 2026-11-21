#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod core_manager;
mod models;
mod diagnostics;
mod probe;
mod singbox_config;
mod sysproxy;
mod uri;

use core_manager::{CoreController, Status};
use models::{gen_node_id, A2SProbeResult, Cs2Node, KcpConfig, ShaperConfig};
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};
use std::sync::Arc;
use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateRoundRectRgn, CreateSolidBrush, DeleteObject,
    DrawTextW, EndPaint, FillRect, GetSysColorBrush, InvalidateRect, LineTo, MoveToEx, RoundRect,
    SelectObject, SetBkMode, SetTextColor, SetWindowRgn, COLOR_BTNFACE, DEFAULT_CHARSET,
    DEFAULT_PITCH, DEFAULT_QUALITY, DT_LEFT, DT_SINGLELINE, DT_VCENTER, FF_DONTCARE, FW_BOLD,
    FW_NORMAL, HBRUSH, HDC, HFONT, OUT_DEFAULT_PRECIS, PAINTSTRUCT, PS_SOLID, TRANSPARENT,
};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::UI::Controls::{
    InitCommonControlsEx, ICC_LISTVIEW_CLASSES, ICC_STANDARD_CLASSES, ICC_TAB_CLASSES,
    INITCOMMONCONTROLSEX, LVCFMT_LEFT, LVCF_FMT, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_TEXT,
    LVITEMW, LVM_DELETEALLITEMS, LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMTEXTW, LVNI_SELECTED, LVS_EX_FULLROWSELECT,
    LVS_EX_GRIDLINES, LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL, NMHDR, SB_SETPARTS,
    SB_SETTEXTW, STATUSCLASSNAMEW, TCIF_TEXT, TCITEMW, TCM_GETCURSEL, TCM_INSERTITEMW,
    TCN_SELCHANGE, WC_LISTVIEWW, WC_TABCONTROLW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, ReleaseCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, DrawIconEx, GetClientRect, GetCursorPos, GetMessageW, GetSystemMetrics,
    GetWindowRect, GetWindowTextLengthW, GetWindowTextW, IsZoomed, LoadCursorW, LoadIconW,
    MessageBoxW, MoveWindow, PostMessageW, PostQuitMessage, RegisterClassW, SendMessageW,
    SetForegroundWindow, SetTimer, SetWindowTextW, ShowWindow, SystemParametersInfoW,
    TrackPopupMenu, TranslateMessage, BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX,
    BS_AUTORADIOBUTTON, BS_DEFPUSHBUTTON, BS_GROUPBOX, BS_PUSHBUTTON, CBN_SELCHANGE,
    CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL, CB_RESETCONTENT, CB_SETCURSEL, DI_NORMAL,
    ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY, HICON, HTCAPTION, IDC_ARROW,
    MB_ICONERROR, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_YESNO, MF_SEPARATOR, MF_STRING,
    MINMAXINFO, MSG, SM_CXSCREEN, SM_CYSCREEN, SPI_GETWORKAREA, SW_HIDE, SW_MAXIMIZE, SW_MINIMIZE,
    SW_RESTORE, SW_SHOW, TPM_BOTTOMALIGN, TPM_LEFTALIGN, WM_APP, WM_CLOSE, WM_COMMAND, WM_CREATE,
    WM_CTLCOLORBTN, WM_CTLCOLORDLG, WM_CTLCOLORSTATIC, WM_DESTROY, WM_GETMINMAXINFO,
    WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCLBUTTONDOWN,
    WM_NOTIFY, WM_PAINT, WM_RBUTTONUP, WM_SETFONT, WM_SIZE, WM_TIMER, WNDCLASSW, WS_BORDER,
    WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN, WS_EX_CLIENTEDGE, WS_EX_DLGMODALFRAME, WS_GROUP,
    WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

const TITLEBAR_H: i32 = 30;
const FRAME_BORDER: i32 = 3;
const BTN_SIZE: i32 = 21;
const CF_UNICODETEXT: u32 = 13;
const EM_SETSEL: u32 = 0x00B1;
const WM_MOUSELEAVE: u32 = 0x02A3;
const BST_CHECKED: usize = 1;
const BST_UNCHECKED: usize = 0;

const WM_TRAYICON: u32 = WM_APP + 1;
const WM_A2S_DONE: u32 = WM_APP + 2;
const WM_IP_DONE: u32 = WM_APP + 3;
const TIMER_REFRESH: usize = 1;
const WM_CORE_STATUS: u32 = WM_APP + 4;
const WM_CORE_LOADED: u32 = WM_APP + 5;
const WM_CORE_EXITED: u32 = WM_APP + 6;
static PROBE_RESULTS: std::sync::Mutex<Vec<A2SProbeResult>> = std::sync::Mutex::new(Vec::new());

// Control IDs
const ID_TAB: usize = 100;
const ID_COMBO_NODE: usize = 101;
const ID_BTN_CONNECT: usize = 102;
const ID_BTN_A2S_HOME: usize = 103;
const ID_BTN_CHECK_IP: usize = 104;
const ID_RADIO_SYSPROXY: usize = 105;
const ID_RADIO_TUN: usize = 106;
const ID_RADIO_RULE: usize = 107;
const ID_RADIO_GLOBAL: usize = 108;
const ID_RADIO_DIRECT: usize = 109;
const ID_EDIT_MIXED_PORT: usize = 110;
const ID_BTN_APPLY_CFG: usize = 112;

const ID_LIST_NODES: usize = 201;
const ID_BTN_IMPORT_CLIP: usize = 202;
const ID_BTN_ADD_MANUAL: usize = 203;
const ID_BTN_COPY_URI: usize = 204;
const ID_BTN_A2S_NODE: usize = 205;
const ID_BTN_DEL_NODE: usize = 206;

const ID_EDIT_LOGS: usize = 301;
const ID_BTN_COPY_LOGS: usize = 302;
const ID_BTN_CLEAR_LOGS: usize = 303;

const ID_TRAY_SHOW: usize = 901;
const ID_TRAY_TOGGLE: usize = 902;
const ID_TRAY_QUIT: usize = 903;

const ID_DLG_NAME: usize = 401;
const ID_DLG_SERVER: usize = 402;
const ID_DLG_PORT: usize = 403;
const ID_DLG_PSK: usize = 404;
const ID_DLG_DS: usize = 405;
const ID_DLG_PS: usize = 406;
const ID_DLG_SHAPER: usize = 407;
const ID_DLG_OK: usize = 408;
const ID_DLG_CANCEL: usize = 409;

#[derive(Clone, Copy, PartialEq, Eq)]
enum CaptionBtn {
    None,
    Minimize,
    Maximize,
    Close,
}

struct UiState {
    core: Arc<CoreController>,
    ip_sender: std::sync::mpsc::Sender<Result<String, String>>,
    ip_results: std::sync::mpsc::Receiver<Result<String, String>>,
    main_hwnd: HWND,
    bg_brush: HBRUSH,
    font_ui: HFONT,
    font_title: HFONT,
    font_mono: HFONT,
    app_icon: HICON,
    current_tab: i32,
    hover_btn: CaptionBtn,
    pressed_btn: CaptionBtn,
    tracking_mouse: bool,
    tab_hwnd: HWND,
    status_hwnd: HWND,
    // Tab 0
    tab0: Vec<HWND>,
    grp_server: HWND,
    grp_proxy: HWND,
    grp_stats: HWND,
    combo_node: HWND,
    lbl_node_info: HWND,
    btn_connect: HWND,
    radio_sysproxy: HWND,
    radio_tun: HWND,
    radio_rule: HWND,
    radio_global: HWND,
    radio_direct: HWND,
    edit_mixed_port: HWND,
    btn_apply_cfg: HWND,
    lbl_stat_conn: HWND,
    lbl_stat_a2s: HWND,
    lbl_stat_rate: HWND,
    lbl_stat_total: HWND,
    // Tab 1
    tab1: Vec<HWND>,
    list_nodes: HWND,
    btn_import: HWND,
    btn_add: HWND,
    btn_copy_uri: HWND,
    btn_a2s_node: HWND,
    btn_del_node: HWND,
    // Tab 2
    tab2: Vec<HWND>,
    edit_logs: HWND,
    btn_copy_logs: HWND,
    btn_clear_logs: HWND,
    last_logs_len: usize,
    // Add dialog
    dlg_hwnd: HWND,
    dlg_name: HWND,
    dlg_server: HWND,
    dlg_port: HWND,
    dlg_psk: HWND,
    dlg_ds: HWND,
    dlg_ps: HWND,
    dlg_shaper: HWND,
}

// Accessed strictly on the single Win32 UI thread — completely eliminates re-entrant Mutex deadlocks.
static mut UI_PTR: *mut UiState = null_mut();

unsafe fn ui_ref() -> Option<&'static UiState> {
    if UI_PTR.is_null() {
        None
    } else {
        Some(&*UI_PTR)
    }
}

unsafe fn ui_mut() -> Option<&'static mut UiState> {
    if UI_PTR.is_null() {
        None
    } else {
        Some(&mut *UI_PTR)
    }
}

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    ((b as u32) << 16) | ((g as u32) << 8) | (r as u32)
}

fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

unsafe fn set_text(hwnd: HWND, s: &str) {
    let w = to_wide(s);
    SetWindowTextW(hwnd, w.as_ptr());
}

unsafe fn get_text(hwnd: HWND) -> String {
    let len = GetWindowTextLengthW(hwnd);
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; (len + 1) as usize];
    let got = GetWindowTextW(hwnd, buf.as_mut_ptr(), len + 1);
    String::from_utf16_lossy(&buf[..got as usize])
}

unsafe fn set_font(hwnd: HWND, font: HFONT) {
    SendMessageW(hwnd, WM_SETFONT, font as WPARAM, 1);
}

unsafe fn msg_box(hwnd: HWND, title: &str, text: &str, flags: u32) -> i32 {
    let wt = to_wide(title);
    let wm = to_wide(text);
    MessageBoxW(hwnd, wm.as_ptr(), wt.as_ptr(), flags)
}

unsafe fn clipboard_read(hwnd: HWND) -> Option<String> {
    if OpenClipboard(hwnd) == 0 {
        return None;
    }
    let h = GetClipboardData(CF_UNICODETEXT);
    let result = if !h.is_null() {
        let ptr = GlobalLock(h) as *const u16;
        if !ptr.is_null() {
            let mut len = 0usize;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
            GlobalUnlock(h);
            Some(s)
        } else {
            None
        }
    } else {
        None
    };
    CloseClipboard();
    result
}

unsafe fn clipboard_write(hwnd: HWND, text: &str) -> bool {
    if OpenClipboard(hwnd) == 0 {
        return false;
    }
    EmptyClipboard();
    let wide = to_wide(text);
    let bytes = wide.len() * 2;
    let hmem = GlobalAlloc(GMEM_MOVEABLE, bytes);
    if hmem.is_null() {
        CloseClipboard();
        return false;
    }
    let ptr = GlobalLock(hmem) as *mut u16;
    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
        GlobalUnlock(hmem);
        SetClipboardData(CF_UNICODETEXT, hmem);
    }
    CloseClipboard();
    true
}

fn format_bytes(bytes: u64) -> String {
    if bytes == 0 {
        return "0 B".to_string();
    }
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", units[i])
    }
}

fn format_uptime(secs: u64) -> String {
    format!("{:02}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
}

unsafe fn create_ctrl(
    ex_style: u32,
    class: &str,
    text: &str,
    style: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    parent: HWND,
    id: usize,
    font: HFONT,
) -> HWND {
    let wc = to_wide(class);
    let wt = to_wide(text);
    let hwnd = CreateWindowExW(
        ex_style,
        wc.as_ptr(),
        wt.as_ptr(),
        style,
        x,
        y,
        w,
        h,
        parent,
        id as _,
        GetModuleHandleW(null()),
        null(),
    );
    set_font(hwnd, font);
    hwnd
}

// Title bar button rectangles (XP Luna standard 21x21 buttons, 2px gap, right-aligned)
fn caption_btn_rects(win_w: i32) -> (RECT, RECT, RECT) {
    let top = 5;
    let close_r = win_w - 6;
    let close = RECT { left: close_r - BTN_SIZE, top, right: close_r, bottom: top + BTN_SIZE };
    let max_r = close.left - 2;
    let max = RECT { left: max_r - BTN_SIZE, top, right: max_r, bottom: top + BTN_SIZE };
    let min_r = max.left - 2;
    let min = RECT { left: min_r - BTN_SIZE, top, right: min_r, bottom: top + BTN_SIZE };
    (min, max, close)
}

fn pt_in_rect(rc: &RECT, x: i32, y: i32) -> bool {
    x >= rc.left && x < rc.right && y >= rc.top && y < rc.bottom
}

fn hit_caption_btn(win_w: i32, x: i32, y: i32) -> CaptionBtn {
    let (min_rc, max_rc, close_rc) = caption_btn_rects(win_w);
    if pt_in_rect(&close_rc, x, y) {
        CaptionBtn::Close
    } else if pt_in_rect(&max_rc, x, y) {
        CaptionBtn::Maximize
    } else if pt_in_rect(&min_rc, x, y) {
        CaptionBtn::Minimize
    } else {
        CaptionBtn::None
    }
}

unsafe fn draw_vgradient(hdc: HDC, rc: RECT, top_rgb: (u8, u8, u8), bot_rgb: (u8, u8, u8)) {
    let h = (rc.bottom - rc.top).max(1);
    for i in 0..h {
        let t = i as f32 / h as f32;
        let r = (top_rgb.0 as f32 + (bot_rgb.0 as f32 - top_rgb.0 as f32) * t) as u8;
        let g = (top_rgb.1 as f32 + (bot_rgb.1 as f32 - top_rgb.1 as f32) * t) as u8;
        let b = (top_rgb.2 as f32 + (bot_rgb.2 as f32 - top_rgb.2 as f32) * t) as u8;
        let br = CreateSolidBrush(rgb(r, g, b));
        let strip = RECT { left: rc.left, top: rc.top + i, right: rc.right, bottom: rc.top + i + 1 };
        FillRect(hdc, &strip, br);
        DeleteObject(br as _);
    }
}

unsafe fn draw_xp_caption_button(
    hdc: HDC,
    rc: RECT,
    kind: CaptionBtn,
    hovered: bool,
    pressed: bool,
    maximized: bool,
) {
    let (top_c, bot_c, border_c) = match (kind, hovered, pressed) {
        (CaptionBtn::Close, _, true) => ((165, 36, 18), (210, 74, 52), rgb(120, 20, 10)),
        (CaptionBtn::Close, true, false) => ((252, 126, 98), (222, 56, 32), rgb(255, 255, 255)),
        (CaptionBtn::Close, false, false) => ((235, 101, 71), (197, 43, 17), rgb(255, 255, 255)),
        (_, _, true) => ((18, 66, 178), (48, 118, 232), rgb(160, 200, 255)),
        (_, true, false) => ((92, 165, 255), (36, 108, 235), rgb(255, 255, 255)),
        _ => ((61, 149, 255), (14, 78, 205), rgb(255, 255, 255)),
    };

    // Outer white/highlight rounded frame (iconic XP Luna 3px corner radius)
    let pen = CreatePen(PS_SOLID as i32, 1, border_c);
    let old_pen = SelectObject(hdc, pen as _);
    let fill = CreateSolidBrush(rgb(top_c.0, top_c.1, top_c.2));
    let old_br = SelectObject(hdc, fill as _);
    RoundRect(hdc, rc.left, rc.top, rc.right, rc.bottom, 5, 5);
    SelectObject(hdc, old_br);
    SelectObject(hdc, old_pen);
    DeleteObject(pen as _);
    DeleteObject(fill as _);

    // Inner glossy gradient
    let inner = RECT {
        left: rc.left + 1,
        top: rc.top + 1,
        right: rc.right - 1,
        bottom: rc.bottom - 1,
    };
    draw_vgradient(hdc, inner, top_c, bot_c);

    // Top-half XP gloss sheen line
    let sheen = CreateSolidBrush(if kind == CaptionBtn::Close {
        rgb(248, 168, 148)
    } else {
        rgb(142, 198, 255)
    });
    FillRect(
        hdc,
        &RECT { left: rc.left + 2, top: rc.top + 1, right: rc.right - 2, bottom: rc.top + 2 },
        sheen,
    );
    DeleteObject(sheen as _);

    // Draw crisp white XP glyph
    let ox = if pressed { 1 } else { 0 };
    let oy = if pressed { 1 } else { 0 };
    let white_br = CreateSolidBrush(rgb(255, 255, 255));
    match kind {
        CaptionBtn::Minimize => {
            // Iconic XP bottom-left thick bar
            FillRect(
                hdc,
                &RECT {
                    left: rc.left + 5 + ox,
                    top: rc.bottom - 7 + oy,
                    right: rc.left + 13 + ox,
                    bottom: rc.bottom - 4 + oy,
                },
                white_br,
            );
        }
        CaptionBtn::Maximize => {
            if !maximized {
                // Single window box with thick 3px top bar
                let l = rc.left + 5 + ox;
                let t = rc.top + 5 + oy;
                let r = rc.right - 5 + ox;
                let b = rc.bottom - 5 + oy;
                FillRect(hdc, &RECT { left: l, top: t, right: r, bottom: t + 3 }, white_br);
                FillRect(hdc, &RECT { left: l, top: b - 1, right: r, bottom: b }, white_br);
                FillRect(hdc, &RECT { left: l, top: t, right: l + 1, bottom: b }, white_br);
                FillRect(hdc, &RECT { left: r - 1, top: t, right: r, bottom: b }, white_br);
            } else {
                // Restore double overlapping boxes
                let l1 = rc.left + 7 + ox;
                let t1 = rc.top + 4 + oy;
                let r1 = rc.right - 4 + ox;
                let b1 = rc.bottom - 7 + oy;
                FillRect(hdc, &RECT { left: l1, top: t1, right: r1, bottom: t1 + 2 }, white_br);
                FillRect(hdc, &RECT { left: r1 - 1, top: t1, right: r1, bottom: b1 }, white_br);
                FillRect(hdc, &RECT { left: l1, top: t1, right: l1 + 1, bottom: t1 + 4 }, white_br);

                let l2 = rc.left + 4 + ox;
                let t2 = rc.top + 8 + oy;
                let r2 = rc.right - 7 + ox;
                let b2 = rc.bottom - 4 + oy;
                FillRect(hdc, &RECT { left: l2, top: t2, right: r2, bottom: t2 + 2 }, white_br);
                FillRect(hdc, &RECT { left: l2, top: b2 - 1, right: r2, bottom: b2 }, white_br);
                FillRect(hdc, &RECT { left: l2, top: t2, right: l2 + 1, bottom: b2 }, white_br);
                FillRect(hdc, &RECT { left: r2 - 1, top: t2, right: r2, bottom: b2 }, white_br);
            }
        }
        CaptionBtn::Close => {
            // Bold crisp white X (2px thick diagonals)
            let gpen = CreatePen(PS_SOLID as i32, 2, rgb(255, 255, 255));
            let op = SelectObject(hdc, gpen as _);
            MoveToEx(hdc, rc.left + 6 + ox, rc.top + 6 + oy, null_mut());
            LineTo(hdc, rc.right - 6 + ox, rc.bottom - 6 + oy);
            MoveToEx(hdc, rc.right - 6 + ox, rc.top + 6 + oy, null_mut());
            LineTo(hdc, rc.left + 6 + ox, rc.bottom - 6 + oy);
            SelectObject(hdc, op);
            DeleteObject(gpen as _);
        }
        CaptionBtn::None => {}
    }
    DeleteObject(white_br as _);
}

unsafe fn paint_xp_chrome(hwnd: HWND, ui: &UiState) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);

    let mut rc: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut rc);
    let w = rc.right;
    let h = rc.bottom;

    // 1. Fill interior with XP Luna #ECE9D8
    FillRect(hdc, &rc, ui.bg_brush);

    // 2. Classic Windows XP Luna 3-stop glossy royal-blue title bar
    draw_vgradient(hdc, RECT { left: 0, top: 0, right: w, bottom: 5 }, (61, 149, 255), (20, 104, 240));
    draw_vgradient(hdc, RECT { left: 0, top: 5, right: w, bottom: 16 }, (14, 96, 234), (0, 72, 212));
    draw_vgradient(hdc, RECT { left: 0, top: 16, right: w, bottom: TITLEBAR_H }, (0, 76, 218), (6, 104, 242));

    // 3. Authentic 3px XP Luna Blue outer window border (left, right, bottom)
    let outer_b = CreateSolidBrush(rgb(0, 60, 180));
    let mid_b = CreateSolidBrush(rgb(0, 84, 227));
    let inner_b = CreateSolidBrush(rgb(36, 114, 238));
    // Left
    FillRect(hdc, &RECT { left: 0, top: TITLEBAR_H, right: 1, bottom: h }, outer_b);
    FillRect(hdc, &RECT { left: 1, top: TITLEBAR_H, right: 2, bottom: h - 1 }, mid_b);
    FillRect(hdc, &RECT { left: 2, top: TITLEBAR_H, right: 3, bottom: h - 2 }, inner_b);
    // Right
    FillRect(hdc, &RECT { left: w - 1, top: TITLEBAR_H, right: w, bottom: h }, outer_b);
    FillRect(hdc, &RECT { left: w - 2, top: TITLEBAR_H, right: w - 1, bottom: h - 1 }, mid_b);
    FillRect(hdc, &RECT { left: w - 3, top: TITLEBAR_H, right: w - 2, bottom: h - 2 }, inner_b);
    // Bottom
    FillRect(hdc, &RECT { left: 0, top: h - 1, right: w, bottom: h }, outer_b);
    FillRect(hdc, &RECT { left: 1, top: h - 2, right: w - 1, bottom: h - 1 }, mid_b);
    FillRect(hdc, &RECT { left: 2, top: h - 3, right: w - 2, bottom: h - 2 }, inner_b);
    DeleteObject(outer_b as _);
    DeleteObject(mid_b as _);
    DeleteObject(inner_b as _);

    // 4. App 16x16 icon + shadowed classic XP title text
    if !ui.app_icon.is_null() {
        DrawIconEx(hdc, 8, 7, ui.app_icon, 16, 16, 0, null_mut(), DI_NORMAL);
    }
    SetBkMode(hdc, TRANSPARENT as i32);
    SelectObject(hdc, ui.font_title as _);
    let title = to_wide("CS2World");
    // 1px dark blue drop shadow (signature XP Luna title text)
    SetTextColor(hdc, rgb(0, 42, 130));
    let mut sh_rc = RECT { left: 30, top: 2, right: w - 90, bottom: TITLEBAR_H };
    DrawTextW(hdc, title.as_ptr(), -1, &mut sh_rc, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
    SetTextColor(hdc, rgb(255, 255, 255));
    let mut tx_rc = RECT { left: 29, top: 1, right: w - 91, bottom: TITLEBAR_H - 1 };
    DrawTextW(hdc, title.as_ptr(), -1, &mut tx_rc, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

    // 5. XP Luna Min / Max / Close buttons
    let zoomed = IsZoomed(hwnd) != 0;
    let (min_rc, max_rc, close_rc) = caption_btn_rects(w);
    draw_xp_caption_button(hdc, min_rc, CaptionBtn::Minimize, ui.hover_btn == CaptionBtn::Minimize, ui.pressed_btn == CaptionBtn::Minimize, zoomed);
    draw_xp_caption_button(hdc, max_rc, CaptionBtn::Maximize, ui.hover_btn == CaptionBtn::Maximize, ui.pressed_btn == CaptionBtn::Maximize, zoomed);
    draw_xp_caption_button(hdc, close_rc, CaptionBtn::Close, ui.hover_btn == CaptionBtn::Close, ui.pressed_btn == CaptionBtn::Close, zoomed);

    EndPaint(hwnd, &ps);
}

unsafe fn layout_children(ui: &UiState, w: i32, h: i32) {
    let status_h = 22;
    let inner_l = FRAME_BORDER + 7;
    let inner_r = w - FRAME_BORDER - 7;
    let inner_w = (inner_r - inner_l).max(300);
    let tab_top = TITLEBAR_H + 8;
    let tab_h = (h - FRAME_BORDER - status_h - 6 - tab_top).max(200);

    MoveWindow(ui.tab_hwnd, inner_l, tab_top, inner_w, tab_h, 1);
    MoveWindow(
        ui.status_hwnd,
        FRAME_BORDER,
        h - FRAME_BORDER - status_h,
        w - FRAME_BORDER * 2,
        status_h,
        1,
    );
    let sb_w = w - FRAME_BORDER * 2;
    let parts: [i32; 4] = [95, sb_w / 2, sb_w - 130, -1];
    SendMessageW(ui.status_hwnd, SB_SETPARTS, 4, parts.as_ptr() as LPARAM);

    let pad_l = inner_l + 10;
    let pad_w = inner_w - 20;
    let page_top = tab_top + 28;

    // Tab 0 groups stretch horizontally cleanly
    MoveWindow(ui.grp_server, pad_l, page_top, pad_w, 102, 1);
    MoveWindow(ui.combo_node, pad_l + 76, page_top + 20, (pad_w - 220).max(160), 160, 1);
    MoveWindow(ui.btn_connect, pad_l + pad_w - 134, page_top + 19, 120, 26, 1);
    MoveWindow(ui.lbl_node_info, pad_l + 14, page_top + 50, pad_w - 28, 16, 1);

    MoveWindow(ui.grp_proxy, pad_l, page_top + 110, pad_w, 94, 1);
    MoveWindow(ui.btn_apply_cfg, pad_l + pad_w - 104, page_top + 172, 90, 24, 1);

    let stats_h = (tab_top + tab_h - (page_top + 212) - 10).max(94);
    MoveWindow(ui.grp_stats, pad_l, page_top + 212, pad_w, stats_h, 1);
    MoveWindow(ui.lbl_stat_conn, pad_l + 14, page_top + 232, pad_w - 28, 16, 1);
    MoveWindow(ui.lbl_stat_a2s, pad_l + 14, page_top + 252, pad_w - 28, 16, 1);
    MoveWindow(ui.lbl_stat_rate, pad_l + 14, page_top + 272, pad_w - 28, 16, 1);
    MoveWindow(ui.lbl_stat_total, pad_l + 14, page_top + 292, pad_w - 28, 16, 1);

    // Tab 1 ListView + buttons
    let list_h = (tab_h - 76).max(120);
    let btn_y = page_top + list_h + 8;
    MoveWindow(ui.list_nodes, pad_l, page_top + 2, pad_w, list_h, 1);
    MoveWindow(ui.btn_import, pad_l, btn_y, 108, 25, 1);
    MoveWindow(ui.btn_add, pad_l + 116, btn_y, 80, 25, 1);
    MoveWindow(ui.btn_copy_uri, pad_l + 204, btn_y, 98, 25, 1);
    MoveWindow(ui.btn_a2s_node, pad_l + 310, btn_y, 82, 25, 1);
    MoveWindow(ui.btn_del_node, pad_l + 400, btn_y, 76, 25, 1);

    // Tab 2 Logs
    MoveWindow(ui.edit_logs, pad_l, page_top + 2, pad_w, list_h, 1);
    MoveWindow(ui.btn_copy_logs, pad_l, btn_y, 100, 25, 1);
    MoveWindow(ui.btn_clear_logs, pad_l + 108, btn_y, 82, 25, 1);
}

unsafe fn switch_tab(ui: &mut UiState, idx: i32) {
    ui.current_tab = idx;
    for &h in &ui.tab0 {
        ShowWindow(h, if idx == 0 { SW_SHOW } else { SW_HIDE });
    }
    for &h in &ui.tab1 {
        ShowWindow(h, if idx == 1 { SW_SHOW } else { SW_HIDE });
    }
    for &h in &ui.tab2 {
        ShowWindow(h, if idx == 2 { SW_SHOW } else { SW_HIDE });
    }
    if idx == 2 {
        refresh_logs(ui, true);
    }
}

unsafe fn refresh_nodes_ui(ui: &mut UiState) {
    let persisted = ui.core.get_persisted();
    let active_id = persisted
        .settings
        .active_node_id
        .clone()
        .or_else(|| persisted.nodes.first().map(|n| n.id.clone()));

    SendMessageW(ui.combo_node, CB_RESETCONTENT, 0, 0);
    let mut sel_idx: i32 = -1;
    for (i, n) in persisted.nodes.iter().enumerate() {
        let label = to_wide(&format!("{} ({}:{})", n.name, n.server, n.server_port));
        SendMessageW(ui.combo_node, CB_ADDSTRING, 0, label.as_ptr() as LPARAM);
        if active_id.as_deref() == Some(n.id.as_str()) {
            sel_idx = i as i32;
        }
    }
    if sel_idx >= 0 {
        SendMessageW(ui.combo_node, CB_SETCURSEL, sel_idx as WPARAM, 0);
    }

    if let Some(node) = ui.core.active_node() {
        let shaper_txt = if node.shaper.enabled {
            format!("{}Hz 整形", node.shaper.tick_rate_hz)
        } else {
            "整形关".to_string()
        };
        set_text(
            ui.lbl_node_info,
            &format!(
                "地址: {}:{}  |  FEC {}:{}  |  {}",
                node.server, node.server_port, node.kcp.data_shards, node.kcp.parity_shards, shaper_txt
            ),
        );
        if let Some(a2s) = &node.last_a2s {
            if a2s.ok {
                set_text(
                    ui.lbl_stat_a2s,
                    &format!(
                        "A2S 伪装: 正常 ({} · {} · {}/{} · {} ms)",
                        a2s.game, a2s.map_name, a2s.players, a2s.max_players, a2s.rtt_ms
                    ),
                );
            } else {
                set_text(
                    ui.lbl_stat_a2s,
                    &format!("A2S 伪装: 失败 ({})", a2s.error.as_deref().unwrap_or("超时")),
                );
            }
        } else {
            set_text(ui.lbl_stat_a2s, "A2S 伪装: 未检测");
        }
    } else {
        set_text(ui.lbl_node_info, "尚未添加节点，请点击“节点管理”导入 cs2world:// 链接");
        set_text(ui.lbl_stat_a2s, "A2S 伪装: -");
    }

    SendMessageW(ui.list_nodes, LVM_DELETEALLITEMS, 0, 0);
    for (i, n) in persisted.nodes.iter().enumerate() {
        let mut name_w = to_wide(&n.name);
        let mut item: LVITEMW = std::mem::zeroed();
        item.mask = LVIF_TEXT;
        item.iItem = i as i32;
        item.iSubItem = 0;
        item.pszText = name_w.as_mut_ptr();
        SendMessageW(ui.list_nodes, LVM_INSERTITEMW, 0, &item as *const _ as LPARAM);

        let set_sub = |sub: i32, text: &str| {
            let mut w = to_wide(text);
            let mut si: LVITEMW = std::mem::zeroed();
            si.iSubItem = sub;
            si.pszText = w.as_mut_ptr();
            SendMessageW(ui.list_nodes, LVM_SETITEMTEXTW, i as WPARAM, &si as *const _ as LPARAM);
        };
        set_sub(1, &n.server);
        set_sub(2, &n.server_port.to_string());
        let a2s_str = match &n.last_a2s {
            Some(a) if a.ok => format!("{}ms ({})", a.rtt_ms, a.map_name),
            Some(_) => "失败".to_string(),
            None => "-".to_string(),
        };
        set_sub(3, &a2s_str);
    }

    let s = &persisted.settings;
    SendMessageW(ui.radio_sysproxy, BM_SETCHECK, if s.proxy_mode != "tun" { BST_CHECKED } else { BST_UNCHECKED }, 0);
    SendMessageW(ui.radio_tun, BM_SETCHECK, if s.proxy_mode == "tun" { BST_CHECKED } else { BST_UNCHECKED }, 0);
    SendMessageW(ui.radio_rule, BM_SETCHECK, if s.routing_mode == "rule" { BST_CHECKED } else { BST_UNCHECKED }, 0);
    SendMessageW(ui.radio_global, BM_SETCHECK, if s.routing_mode == "global" { BST_CHECKED } else { BST_UNCHECKED }, 0);
    SendMessageW(ui.radio_direct, BM_SETCHECK, if s.routing_mode == "direct" { BST_CHECKED } else { BST_UNCHECKED }, 0);
    set_text(ui.edit_mixed_port, &s.mixed_port.to_string());
}

unsafe fn refresh_status_ui(ui: &mut UiState) {
    let connected = ui.core.is_connected();
    let settings = ui.core.get_persisted().settings;
    let node_name = ui
        .core
        .active_node()
        .map(|n| n.name)
        .unwrap_or_else(|| "无节点".to_string());

    let pending = matches!(ui.core.status(), Status::Starting | Status::Listening);
    set_text(ui.btn_connect, if pending { "取消连接" } else if connected { "断开(&D)" } else { "连接(&C)" });
    EnableWindow(ui.btn_connect, (ui.core.ready() && ui.core.status() != Status::Stopping) as i32);

    if connected {
        let up = format_uptime(ui.core.uptime_secs());
        set_text(ui.lbl_stat_conn, &format!("连接状态: 已连接  ({up})"));
    } else if pending {
        set_text(ui.lbl_stat_conn, if ui.core.status() == Status::Listening { "连接状态: 本地代理已就绪，正在认证节点" } else { "连接状态: 正在启动" });
    } else if let Some(err) = ui.core.last_error() {
        set_text(ui.lbl_stat_conn, &format!("连接状态: 错误 - {err}"));
    } else {
        set_text(ui.lbl_stat_conn, "连接状态: 未连接");
    }

    let t = ui.core.poll_traffic();
    set_text(
        ui.lbl_stat_rate,
        &format!("实时速率: ↓ {}/s    ↑ {}/s", format_bytes(t.down_bps), format_bytes(t.up_bps)),
    );
    set_text(
        ui.lbl_stat_total,
        &format!(
            "累计流量: ↓ {}    ↑ {}    (连接数: {})",
            format_bytes(t.down_total),
            format_bytes(t.up_total),
            t.active_connections
        ),
    );

    let p0 = to_wide(if connected { " 已连接" } else if pending { " 连接中" } else { " 未连接" });
    let p1 = to_wide(&format!(" 节点: {node_name}"));
    let p2 = to_wide(&format!(" ↓{}/s ↑{}/s", format_bytes(t.down_bps), format_bytes(t.up_bps)));
    let p3 = to_wide(&format!(" 127.0.0.1:{}", settings.mixed_port));
    SendMessageW(ui.status_hwnd, SB_SETTEXTW, 0, p0.as_ptr() as LPARAM);
    SendMessageW(ui.status_hwnd, SB_SETTEXTW, 1, p1.as_ptr() as LPARAM);
    SendMessageW(ui.status_hwnd, SB_SETTEXTW, 2, p2.as_ptr() as LPARAM);
    SendMessageW(ui.status_hwnd, SB_SETTEXTW, 3, p3.as_ptr() as LPARAM);

    if ui.current_tab == 2 {
        refresh_logs(ui, false);
    }
}

unsafe fn refresh_logs(ui: &mut UiState, force: bool) {
    let text = ui.core.get_logs_text();
    if force || text.len() != ui.last_logs_len {
        ui.last_logs_len = text.len();
        set_text(ui.edit_logs, &text);
        let len = text.encode_utf16().count();
        SendMessageW(ui.edit_logs, EM_SETSEL, len, len as LPARAM);
    }
}

unsafe fn add_tray_icon(hwnd: HWND, icon: HICON) {
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WM_TRAYICON;
    nid.hIcon = icon;
    let tip = to_wide("CS2World");
    let copy_len = tip.len().min(nid.szTip.len());
    nid.szTip[..copy_len].copy_from_slice(&tip[..copy_len]);
    Shell_NotifyIconW(NIM_ADD, &nid);
}

unsafe fn remove_tray_icon(hwnd: HWND) {
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    Shell_NotifyIconW(NIM_DELETE, &nid);
}

unsafe fn open_add_node_dialog(parent: HWND) {
    let hinst = GetModuleHandleW(null());
    let cls = to_wide("CS2WorldAddNodeDlg");
    let title = to_wide("添加节点");

    let mut prc: RECT = std::mem::zeroed();
    GetWindowRect(parent, &mut prc);
    let w = 390;
    let h = 275;
    let x = prc.left + ((prc.right - prc.left) - w) / 2;
    let y = prc.top + ((prc.bottom - prc.top) - h) / 2;

    let dlg = CreateWindowExW(
        WS_EX_DLGMODALFRAME,
        cls.as_ptr(),
        title.as_ptr(),
        WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_VISIBLE,
        x,
        y,
        w,
        h,
        parent,
        null_mut(),
        hinst,
        null(),
    );
    EnableWindow(parent, 0);
    if let Some(ui) = ui_mut() {
        ui.dlg_hwnd = dlg;
    }
}

unsafe extern "system" fn add_dlg_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            if let Some(ui) = ui_mut() {
                let f = ui.font_ui;
                let _ = create_ctrl(0, "STATIC", "备注:", WS_CHILD | WS_VISIBLE, 16, 18, 65, 18, hwnd, 0, f);
                ui.dlg_name = create_ctrl(WS_EX_CLIENTEDGE, "EDIT", "CS2 节点", WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL as u32, 88, 15, 275, 22, hwnd, ID_DLG_NAME, f);

                let _ = create_ctrl(0, "STATIC", "服务器:", WS_CHILD | WS_VISIBLE, 16, 48, 65, 18, hwnd, 0, f);
                ui.dlg_server = create_ctrl(WS_EX_CLIENTEDGE, "EDIT", "", WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL as u32, 88, 45, 175, 22, hwnd, ID_DLG_SERVER, f);
                let _ = create_ctrl(0, "STATIC", "端口:", WS_CHILD | WS_VISIBLE, 270, 48, 36, 18, hwnd, 0, f);
                ui.dlg_port = create_ctrl(WS_EX_CLIENTEDGE, "EDIT", "27015", WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL as u32, 308, 45, 55, 22, hwnd, ID_DLG_PORT, f);

                let _ = create_ctrl(0, "STATIC", "PSK:", WS_CHILD | WS_VISIBLE, 16, 78, 65, 18, hwnd, 0, f);
                ui.dlg_psk = create_ctrl(WS_EX_CLIENTEDGE, "EDIT", "", WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL as u32, 88, 75, 275, 22, hwnd, ID_DLG_PSK, f);

                let _ = create_ctrl(0, "STATIC", "KCP FEC:", WS_CHILD | WS_VISIBLE, 16, 110, 65, 18, hwnd, 0, f);
                let _ = create_ctrl(0, "STATIC", "Data:", WS_CHILD | WS_VISIBLE, 88, 110, 36, 18, hwnd, 0, f);
                ui.dlg_ds = create_ctrl(WS_EX_CLIENTEDGE, "EDIT", "10", WS_CHILD | WS_VISIBLE | WS_TABSTOP, 128, 107, 45, 22, hwnd, ID_DLG_DS, f);
                let _ = create_ctrl(0, "STATIC", "Parity:", WS_CHILD | WS_VISIBLE, 188, 110, 44, 18, hwnd, 0, f);
                ui.dlg_ps = create_ctrl(WS_EX_CLIENTEDGE, "EDIT", "3", WS_CHILD | WS_VISIBLE | WS_TABSTOP, 236, 107, 45, 22, hwnd, ID_DLG_PS, f);

                ui.dlg_shaper = create_ctrl(0, "BUTTON", "开启 AI 64-Tick 流量整形", WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_AUTOCHECKBOX as u32, 88, 142, 220, 20, hwnd, ID_DLG_SHAPER, f);
                SendMessageW(ui.dlg_shaper, BM_SETCHECK, BST_CHECKED, 0);

                let _ = create_ctrl(0, "BUTTON", "确定", WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_DEFPUSHBUTTON as u32, 196, 194, 78, 25, hwnd, ID_DLG_OK, f);
                let _ = create_ctrl(0, "BUTTON", "取消", WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32, 285, 194, 78, 25, hwnd, ID_DLG_CANCEL, f);
            }
            0
        }
        WM_CTLCOLORDLG | WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            if let Some(ui) = ui_ref() {
                SetBkMode(wparam as _, TRANSPARENT as i32);
                return ui.bg_brush as LRESULT;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_COMMAND => {
            let id = (wparam & 0xFFFF) as usize;
            if id == ID_DLG_OK {
                if let Some(ui) = ui_mut() {
                    let name = get_text(ui.dlg_name);
                    let server = get_text(ui.dlg_server).trim().to_string();
                    let port: u16 = get_text(ui.dlg_port).trim().parse().unwrap_or(0);
                    let psk = get_text(ui.dlg_psk).trim().to_string();
                    let ds: u32 = get_text(ui.dlg_ds).trim().parse().unwrap_or(10);
                    let ps: u32 = get_text(ui.dlg_ps).trim().parse().unwrap_or(3);
                    let shaper_on = SendMessageW(ui.dlg_shaper, BM_GETCHECK, 0, 0) as usize == BST_CHECKED;
                    let parent = ui.main_hwnd;

                    if server.is_empty() || psk.is_empty() || port == 0 {
                        msg_box(hwnd, "提示", "请填写有效的服务器地址、端口和 PSK。", MB_OK | MB_ICONWARNING);
                        return 0;
                    }
                    let node = Cs2Node {
                        id: gen_node_id(),
                        name: if name.trim().is_empty() { format!("{server}:{port}") } else { name.trim().to_string() },
                        server,
                        server_port: port,
                        psk,
                        kcp: KcpConfig { data_shards: ds, parity_shards: ps, ..KcpConfig::default() },
                        shaper: ShaperConfig { enabled: shaper_on, ..ShaperConfig::default() },
                        last_a2s: None,
                    };
                    let _ = ui.core.mutate_persisted(|p| {
                        if p.settings.active_node_id.is_none() {
                            p.settings.active_node_id = Some(node.id.clone());
                        }
                        p.nodes.push(node.clone());
                    });
                    ui.core.push_log("[Nodes] 已添加节点");
                    refresh_nodes_ui(ui);
                    refresh_status_ui(ui);
                    EnableWindow(parent, 1);
                    DestroyWindow(hwnd);
                    SetForegroundWindow(parent);
                }
                return 0;
            } else if id == ID_DLG_CANCEL {
                if let Some(ui) = ui_ref() {
                    EnableWindow(ui.main_hwnd, 1);
                    DestroyWindow(hwnd);
                    SetForegroundWindow(ui.main_hwnd);
                }
                return 0;
            }
            0
        }
        WM_CLOSE => {
            if let Some(ui) = ui_ref() {
                EnableWindow(ui.main_hwnd, 1);
                DestroyWindow(hwnd);
                SetForegroundWindow(ui.main_hwnd);
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn trigger_a2s_probe(hwnd: HWND, node: Cs2Node) {
    if let Some(ui) = ui_ref() {
        let core = Arc::clone(&ui.core);
        core.push_log("[A2S] 正在查询所选节点");
        let hwnd_raw = hwnd as usize;
        std::thread::spawn(move || {
            let res = probe::probe_a2s_info(&node.server, node.server_port);
            let _ = core.mutate_persisted(|p| {
                if let Some(n) = p.nodes.iter_mut().find(|n| n.id == node.id) {
                    n.last_a2s = Some(res.clone());
                }
            });
            if res.ok {
                core.push_log(&format!(
                    "[A2S] 成功: {} | {} | {}/{} | {}ms",
                    res.game, res.map_name, res.players, res.max_players, res.rtt_ms
                ));
            } else {
                core.push_log(&format!("[A2S] 失败: {}", res.error.as_deref().unwrap_or("超时")));
            }
            PROBE_RESULTS.lock().unwrap().push(res);
            PostMessageW(hwnd_raw as HWND, WM_A2S_DONE, 0, 0);
        });
    }
}

unsafe fn update_window_region(hwnd: HWND, w: i32, h: i32) {
    if IsZoomed(hwnd) != 0 {
        SetWindowRgn(hwnd, null_mut(), 1);
    } else {
        let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, 12, 12);
        SetWindowRgn(hwnd, rgn, 1);
    }
}

unsafe extern "system" fn main_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let core = CoreController::new(hwnd as usize);
            let bg_brush = GetSysColorBrush(COLOR_BTNFACE);
            // Authentic Windows XP Chinese UI font (宋体 SimSun 9pt) + Trebuchet MS/SimSun bold title
            let simsun = to_wide("SimSun");
            let title_font_name = to_wide("Trebuchet MS");
            let mono_name = to_wide("Consolas");
            let font_ui = CreateFontW(-12, 0, 0, 0, FW_NORMAL as i32, 0, 0, 0, DEFAULT_CHARSET as u32, OUT_DEFAULT_PRECIS as u32, 0, DEFAULT_QUALITY as u32, (DEFAULT_PITCH | FF_DONTCARE) as u32, simsun.as_ptr());
            let font_title = CreateFontW(-14, 0, 0, 0, FW_BOLD as i32, 0, 0, 0, DEFAULT_CHARSET as u32, OUT_DEFAULT_PRECIS as u32, 0, DEFAULT_QUALITY as u32, (DEFAULT_PITCH | FF_DONTCARE) as u32, title_font_name.as_ptr());
            let font_mono = CreateFontW(-12, 0, 0, 0, FW_NORMAL as i32, 0, 0, 0, DEFAULT_CHARSET as u32, OUT_DEFAULT_PRECIS as u32, 0, DEFAULT_QUALITY as u32, (DEFAULT_PITCH | FF_DONTCARE) as u32, mono_name.as_ptr());
            let app_icon = LoadIconW(GetModuleHandleW(null()), 1 as *const u16);

            let tab_hwnd = CreateWindowExW(
                0,
                WC_TABCONTROLW,
                null(),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                10,
                38,
                600,
                360,
                hwnd,
                ID_TAB as _,
                GetModuleHandleW(null()),
                null(),
            );
            set_font(tab_hwnd, font_ui);
            for (idx, title) in ["连接", "节点管理", "日志"].iter().enumerate() {
                let mut w = to_wide(title);
                let item = TCITEMW {
                    mask: TCIF_TEXT,
                    dwState: 0,
                    dwStateMask: 0,
                    pszText: w.as_mut_ptr(),
                    cchTextMax: 0,
                    iImage: -1,
                    lParam: 0,
                };
                SendMessageW(tab_hwnd, TCM_INSERTITEMW, idx, &item as *const _ as LPARAM);
            }

            let status_hwnd = CreateWindowExW(
                0,
                STATUSCLASSNAMEW,
                null(),
                WS_CHILD | WS_VISIBLE,
                0,
                0,
                0,
                0,
                hwnd,
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            );
            set_font(status_hwnd, font_ui);

            // --- TAB 0: 连接 ---
            let mut tab0 = Vec::new();
            let grp_server = create_ctrl(0, "BUTTON", "服务器", WS_CHILD | WS_VISIBLE | BS_GROUPBOX as u32, 20, 66, 580, 102, hwnd, 0, font_ui);
            tab0.push(grp_server);
            tab0.push(create_ctrl(0, "STATIC", "节点:", WS_CHILD | WS_VISIBLE, 34, 89, 52, 18, hwnd, 0, font_ui));
            let combo_node = create_ctrl(0, "COMBOBOX", "", WS_CHILD | WS_VISIBLE | WS_TABSTOP | CBS_DROPDOWNLIST as u32 | WS_VSCROLL, 96, 86, 360, 160, hwnd, ID_COMBO_NODE, font_ui);
            tab0.push(combo_node);
            let btn_connect = create_ctrl(0, "BUTTON", "连接(&C)", WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_DEFPUSHBUTTON as u32, 466, 85, 120, 26, hwnd, ID_BTN_CONNECT, font_ui);
            tab0.push(btn_connect);

            let lbl_node_info = create_ctrl(0, "STATIC", "尚未添加节点", WS_CHILD | WS_VISIBLE, 34, 116, 550, 16, hwnd, 0, font_ui);
            tab0.push(lbl_node_info);

            tab0.push(create_ctrl(0, "BUTTON", "A2S 伪装检测", WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32, 34, 138, 108, 24, hwnd, ID_BTN_A2S_HOME, font_ui));
            tab0.push(create_ctrl(0, "BUTTON", "检测出口 IP", WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32, 150, 138, 104, 24, hwnd, ID_BTN_CHECK_IP, font_ui));

            let grp_proxy = create_ctrl(0, "BUTTON", "代理与分流", WS_CHILD | WS_VISIBLE | BS_GROUPBOX as u32, 20, 176, 580, 94, hwnd, 0, font_ui);
            tab0.push(grp_proxy);
            tab0.push(create_ctrl(0, "STATIC", "代理方式:", WS_CHILD | WS_VISIBLE, 34, 199, 62, 16, hwnd, 0, font_ui));
            let radio_sysproxy = create_ctrl(0, "BUTTON", "系统代理", WS_CHILD | WS_VISIBLE | WS_GROUP | WS_TABSTOP | BS_AUTORADIOBUTTON as u32, 100, 197, 92, 18, hwnd, ID_RADIO_SYSPROXY, font_ui);
            let radio_tun = create_ctrl(0, "BUTTON", "TUN 虚拟网卡 (需管理员)", WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_AUTORADIOBUTTON as u32, 200, 197, 175, 18, hwnd, ID_RADIO_TUN, font_ui);
            tab0.push(radio_sysproxy);
            tab0.push(radio_tun);

            tab0.push(create_ctrl(0, "STATIC", "分流模式:", WS_CHILD | WS_VISIBLE, 34, 222, 62, 16, hwnd, 0, font_ui));
            let radio_rule = create_ctrl(0, "BUTTON", "规则分流", WS_CHILD | WS_VISIBLE | WS_GROUP | WS_TABSTOP | BS_AUTORADIOBUTTON as u32, 100, 220, 82, 18, hwnd, ID_RADIO_RULE, font_ui);
            let radio_global = create_ctrl(0, "BUTTON", "全局代理", WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_AUTORADIOBUTTON as u32, 190, 220, 82, 18, hwnd, ID_RADIO_GLOBAL, font_ui);
            let radio_direct = create_ctrl(0, "BUTTON", "直连", WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_AUTORADIOBUTTON as u32, 280, 220, 62, 18, hwnd, ID_RADIO_DIRECT, font_ui);
            tab0.push(radio_rule);
            tab0.push(radio_global);
            tab0.push(radio_direct);

            tab0.push(create_ctrl(0, "STATIC", "本地端口:", WS_CHILD | WS_VISIBLE, 34, 245, 62, 16, hwnd, 0, font_ui));
            let edit_mixed_port = create_ctrl(WS_EX_CLIENTEDGE, "EDIT", "17890", WS_CHILD | WS_VISIBLE | WS_TABSTOP, 100, 242, 62, 20, hwnd, ID_EDIT_MIXED_PORT, font_ui);
            tab0.push(edit_mixed_port);
            let btn_apply_cfg = create_ctrl(0, "BUTTON", "应用", WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32, 496, 238, 90, 24, hwnd, ID_BTN_APPLY_CFG, font_ui);
            tab0.push(btn_apply_cfg);

            let grp_stats = create_ctrl(0, "BUTTON", "状态与统计", WS_CHILD | WS_VISIBLE | BS_GROUPBOX as u32, 20, 278, 580, 96, hwnd, 0, font_ui);
            tab0.push(grp_stats);
            let lbl_stat_conn = create_ctrl(0, "STATIC", "连接状态: 未连接", WS_CHILD | WS_VISIBLE, 34, 298, 550, 16, hwnd, 0, font_ui);
            let lbl_stat_a2s = create_ctrl(0, "STATIC", "A2S 伪装: -", WS_CHILD | WS_VISIBLE, 34, 318, 550, 16, hwnd, 0, font_ui);
            let lbl_stat_rate = create_ctrl(0, "STATIC", "实时速率: ↓ 0 B/s    ↑ 0 B/s", WS_CHILD | WS_VISIBLE, 34, 338, 550, 16, hwnd, 0, font_ui);
            let lbl_stat_total = create_ctrl(0, "STATIC", "累计流量: ↓ 0 B    ↑ 0 B", WS_CHILD | WS_VISIBLE, 34, 358, 550, 16, hwnd, 0, font_ui);
            tab0.extend([lbl_stat_conn, lbl_stat_a2s, lbl_stat_rate, lbl_stat_total]);

            // --- TAB 1: 节点管理 ---
            let mut tab1 = Vec::new();
            let list_nodes = CreateWindowExW(
                WS_EX_CLIENTEDGE,
                WC_LISTVIEWW,
                null(),
                WS_CHILD | LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS | WS_BORDER,
                20,
                68,
                580,
                268,
                hwnd,
                ID_LIST_NODES as _,
                GetModuleHandleW(null()),
                null(),
            );
            set_font(list_nodes, font_ui);
            SendMessageW(list_nodes, LVM_SETEXTENDEDLISTVIEWSTYLE, 0, (LVS_EX_FULLROWSELECT | LVS_EX_GRIDLINES) as LPARAM);
            for (idx, (col_title, col_w)) in [
                ("备注名称", 155),
                ("服务器地址", 180),
                ("端口", 64),
                ("A2S 伪装探测", 160),
            ]
            .iter()
            .enumerate()
            {
                let mut wt = to_wide(col_title);
                let col = LVCOLUMNW {
                    mask: LVCF_FMT | LVCF_WIDTH | LVCF_TEXT,
                    fmt: LVCFMT_LEFT,
                    cx: *col_w,
                    pszText: wt.as_mut_ptr(),
                    cchTextMax: 0,
                    iSubItem: idx as i32,
                    iImage: 0,
                    iOrder: 0,
                    cxMin: 0,
                    cxDefault: 0,
                    cxIdeal: 0,
                };
                SendMessageW(list_nodes, LVM_INSERTCOLUMNW, idx, &col as *const _ as LPARAM);
            }
            tab1.push(list_nodes);
            let btn_import = create_ctrl(0, "BUTTON", "从剪贴板导入", WS_CHILD | WS_TABSTOP | BS_PUSHBUTTON as u32, 20, 344, 108, 25, hwnd, ID_BTN_IMPORT_CLIP, font_ui);
            let btn_add = create_ctrl(0, "BUTTON", "手动添加", WS_CHILD | WS_TABSTOP | BS_PUSHBUTTON as u32, 136, 344, 80, 25, hwnd, ID_BTN_ADD_MANUAL, font_ui);
            let btn_copy_uri = create_ctrl(0, "BUTTON", "复制分享链接", WS_CHILD | WS_TABSTOP | BS_PUSHBUTTON as u32, 224, 344, 98, 25, hwnd, ID_BTN_COPY_URI, font_ui);
            let btn_a2s_node = create_ctrl(0, "BUTTON", "A2S 测速", WS_CHILD | WS_TABSTOP | BS_PUSHBUTTON as u32, 330, 344, 82, 25, hwnd, ID_BTN_A2S_NODE, font_ui);
            let btn_del_node = create_ctrl(0, "BUTTON", "删除", WS_CHILD | WS_TABSTOP | BS_PUSHBUTTON as u32, 420, 344, 76, 25, hwnd, ID_BTN_DEL_NODE, font_ui);
            tab1.extend([btn_import, btn_add, btn_copy_uri, btn_a2s_node, btn_del_node]);

            // --- TAB 2: 日志 ---
            let mut tab2 = Vec::new();
            let edit_logs = create_ctrl(
                WS_EX_CLIENTEDGE,
                "EDIT",
                "",
                WS_CHILD | ES_MULTILINE as u32 | ES_AUTOVSCROLL as u32 | ES_READONLY as u32 | WS_VSCROLL,
                20,
                68,
                580,
                268,
                hwnd,
                ID_EDIT_LOGS,
                font_mono,
            );
            let btn_copy_logs = create_ctrl(0, "BUTTON", "复制日志", WS_CHILD | WS_TABSTOP | BS_PUSHBUTTON as u32, 20, 344, 100, 25, hwnd, ID_BTN_COPY_LOGS, font_ui);
            let btn_clear_logs = create_ctrl(0, "BUTTON", "清空日志", WS_CHILD | WS_TABSTOP | BS_PUSHBUTTON as u32, 128, 344, 82, 25, hwnd, ID_BTN_CLEAR_LOGS, font_ui);
            tab2.extend([edit_logs, btn_copy_logs, btn_clear_logs]);

            let (ip_sender, ip_results) = std::sync::mpsc::channel();
            let ui_box = Box::new(UiState {
                core,
                ip_sender,
                ip_results,
                main_hwnd: hwnd,
                bg_brush,
                font_ui,
                font_title,
                font_mono,
                app_icon,
                current_tab: 0,
                hover_btn: CaptionBtn::None,
                pressed_btn: CaptionBtn::None,
                tracking_mouse: false,
                tab_hwnd,
                status_hwnd,
                tab0,
                grp_server,
                grp_proxy,
                grp_stats,
                combo_node,
                lbl_node_info,
                btn_connect,
                radio_sysproxy,
                radio_tun,
                radio_rule,
                radio_global,
                radio_direct,
                edit_mixed_port,
                btn_apply_cfg,
                lbl_stat_conn,
                lbl_stat_a2s,
                lbl_stat_rate,
                lbl_stat_total,
                tab1,
                list_nodes,
                btn_import,
                btn_add,
                btn_copy_uri,
                btn_a2s_node,
                btn_del_node,
                tab2,
                edit_logs,
                btn_copy_logs,
                btn_clear_logs,
                last_logs_len: 0,
                dlg_hwnd: null_mut(),
                dlg_name: null_mut(),
                dlg_server: null_mut(),
                dlg_port: null_mut(),
                dlg_psk: null_mut(),
                dlg_ds: null_mut(),
                dlg_ps: null_mut(),
                dlg_shaper: null_mut(),
            });
            UI_PTR = Box::into_raw(ui_box);

            if let Some(ui) = ui_mut() {
                let mut rc: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut rc);
                update_window_region(hwnd, rc.right, rc.bottom);
                layout_children(ui, rc.right, rc.bottom);
                refresh_nodes_ui(ui);
                refresh_status_ui(ui);
                switch_tab(ui, 0);
                add_tray_icon(hwnd, ui.app_icon);
            }
            SetTimer(hwnd, TIMER_REFRESH, 1000, None);
            0
        }
        WM_GETMINMAXINFO => {
            let mmi = &mut *(lparam as *mut MINMAXINFO);
            let mut work: RECT = std::mem::zeroed();
            if SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut work as *mut _ as _, 0) != 0 {
                mmi.ptMaxPosition.x = work.left;
                mmi.ptMaxPosition.y = work.top;
                mmi.ptMaxSize.x = work.right - work.left;
                mmi.ptMaxSize.y = work.bottom - work.top;
            }
            0
        }
        WM_SIZE => {
            let w = (lparam & 0xFFFF) as i16 as i32;
            let h = ((lparam >> 16) & 0xFFFF) as i16 as i32;
            if w > 0 && h > 0 {
                update_window_region(hwnd, w, h);
                if let Some(ui) = ui_ref() {
                    layout_children(ui, w, h);
                }
                InvalidateRect(hwnd, null(), 0);
            }
            0
        }
        WM_PAINT => {
            if let Some(ui) = ui_ref() {
                paint_xp_chrome(hwnd, ui);
                0
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        WM_MOUSEMOVE => {
            if let Some(ui) = ui_mut() {
                let x = (lparam & 0xFFFF) as i16 as i32;
                let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
                let mut rc: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut rc);
                let hit = if y < TITLEBAR_H { hit_caption_btn(rc.right, x, y) } else { CaptionBtn::None };
                if hit != ui.hover_btn {
                    ui.hover_btn = hit;
                    InvalidateRect(hwnd, &RECT { left: 0, top: 0, right: rc.right, bottom: TITLEBAR_H }, 0);
                }
                if !ui.tracking_mouse {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    TrackMouseEvent(&mut tme);
                    ui.tracking_mouse = true;
                }
            }
            0
        }
        WM_MOUSELEAVE => {
            if let Some(ui) = ui_mut() {
                ui.tracking_mouse = false;
                if ui.hover_btn != CaptionBtn::None || ui.pressed_btn != CaptionBtn::None {
                    ui.hover_btn = CaptionBtn::None;
                    ui.pressed_btn = CaptionBtn::None;
                    let mut rc: RECT = std::mem::zeroed();
                    GetClientRect(hwnd, &mut rc);
                    InvalidateRect(hwnd, &RECT { left: 0, top: 0, right: rc.right, bottom: TITLEBAR_H }, 0);
                }
            }
            0
        }
        WM_LBUTTONDOWN => {
            let x = (lparam & 0xFFFF) as i16 as i32;
            let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
            if y < TITLEBAR_H {
                let mut rc: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut rc);
                let hit = hit_caption_btn(rc.right, x, y);
                if hit != CaptionBtn::None {
                    if let Some(ui) = ui_mut() {
                        ui.pressed_btn = hit;
                        InvalidateRect(hwnd, &RECT { left: 0, top: 0, right: rc.right, bottom: TITLEBAR_H }, 0);
                    }
                } else {
                    // Drag window by XP title bar
                    ReleaseCapture();
                    SendMessageW(hwnd, WM_NCLBUTTONDOWN, HTCAPTION as WPARAM, 0);
                }
            }
            0
        }
        WM_LBUTTONDBLCLK => {
            let x = (lparam & 0xFFFF) as i16 as i32;
            let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
            if y < TITLEBAR_H {
                let mut rc: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut rc);
                if hit_caption_btn(rc.right, x, y) == CaptionBtn::None {
                    ShowWindow(hwnd, if IsZoomed(hwnd) != 0 { SW_RESTORE } else { SW_MAXIMIZE });
                }
            }
            0
        }
        WM_LBUTTONUP => {
            let x = (lparam & 0xFFFF) as i16 as i32;
            let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
            let mut rc: RECT = std::mem::zeroed();
            GetClientRect(hwnd, &mut rc);
            let clicked = if let Some(ui) = ui_mut() {
                let prev = ui.pressed_btn;
                ui.pressed_btn = CaptionBtn::None;
                InvalidateRect(hwnd, &RECT { left: 0, top: 0, right: rc.right, bottom: TITLEBAR_H }, 0);
                if prev != CaptionBtn::None && hit_caption_btn(rc.right, x, y) == prev {
                    prev
                } else {
                    CaptionBtn::None
                }
            } else {
                CaptionBtn::None
            };
            match clicked {
                CaptionBtn::Minimize => {
                    ShowWindow(hwnd, SW_MINIMIZE);
                }
                CaptionBtn::Maximize => {
                    ShowWindow(hwnd, if IsZoomed(hwnd) != 0 { SW_RESTORE } else { SW_MAXIMIZE });
                }
                CaptionBtn::Close => {
                    PostMessageW(hwnd, WM_CLOSE, 0, 0);
                }
                CaptionBtn::None => {}
            }
            0
        }
        WM_CTLCOLORDLG | WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            if let Some(ui) = ui_ref() {
                if lparam as HWND == ui.edit_logs {
                    return DefWindowProcW(hwnd, msg, wparam, lparam);
                }
                SetBkMode(wparam as _, TRANSPARENT as i32);
                return ui.bg_brush as LRESULT;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_NOTIFY => {
            let nm = &*(lparam as *const NMHDR);
            if nm.idFrom == ID_TAB && nm.code == TCN_SELCHANGE {
                if let Some(ui) = ui_mut() {
                    let idx = SendMessageW(ui.tab_hwnd, TCM_GETCURSEL, 0, 0) as i32;
                    switch_tab(ui, idx);
                }
            }
            0
        }
        WM_CORE_LOADED => {
            if let Some(ui) = ui_mut() { refresh_nodes_ui(ui); refresh_status_ui(ui); }
            0
        }
        WM_CORE_STATUS => {
            if let Some(ui) = ui_mut() { refresh_status_ui(ui); }
            0
        }
        WM_CORE_EXITED => { DestroyWindow(hwnd); 0 }
        WM_TIMER => {
            if wparam == TIMER_REFRESH {
                if let Some(ui) = ui_mut() {
                    refresh_status_ui(ui);
                }
            }
            0
        }
        WM_A2S_DONE => {
            let Some(res) = PROBE_RESULTS.lock().unwrap().pop() else {return 0;};
            if let Some(ui) = ui_mut() {
                refresh_nodes_ui(ui);
                refresh_status_ui(ui);
            }
            if res.ok {
                msg_box(
                    hwnd,
                    "A2S 伪装探测成功",
                    &format!(
                        "游戏: {} (AppID {})\r\n房间名: {}\r\n当前地图: {}\r\n模拟在线: {}/{}\r\nUDP 往返延迟: {} ms",
                        res.game, res.app_id, res.server_name, res.map_name, res.players, res.max_players, res.rtt_ms
                    ),
                    MB_OK | MB_ICONINFORMATION,
                );
            } else {
                msg_box(hwnd, "A2S 伪装探测失败", res.error.as_deref().unwrap_or("无响应"), MB_OK | MB_ICONWARNING);
            }
            0
        }
        WM_IP_DONE => {
            let Some(res) = ui_ref().and_then(|ui| ui.ip_results.try_recv().ok()) else {return 0;};
            match res {
                Ok(ip) => {
                    msg_box(hwnd, "代理出口 IP", &format!("当前代理出口 IP:\r\n\r\n{ip}"), MB_OK | MB_ICONINFORMATION);
                }
                Err(e) => {
                    msg_box(hwnd, "检测失败", &e, MB_OK | MB_ICONWARNING);
                }
            }
            0
        }
        WM_TRAYICON => {
            let ev = (lparam & 0xFFFF) as u32;
            if ev == WM_LBUTTONUP || ev == WM_LBUTTONDBLCLK {
                ShowWindow(hwnd, SW_RESTORE);
                SetForegroundWindow(hwnd);
            } else if ev == WM_RBUTTONUP {
                let connected = ui_ref().map(|u| u.core.is_connected()).unwrap_or(false);
                let menu = CreatePopupMenu();
                AppendMenuW(menu, MF_STRING, ID_TRAY_SHOW, to_wide("显示主窗口").as_ptr());
                AppendMenuW(
                    menu,
                    MF_STRING,
                    ID_TRAY_TOGGLE,
                    to_wide(if connected { "断开连接" } else { "立即连接" }).as_ptr(),
                );
                AppendMenuW(menu, MF_SEPARATOR, 0, null());
                AppendMenuW(menu, MF_STRING, ID_TRAY_QUIT, to_wide("退出并还原系统代理").as_ptr());
                let mut pt: POINT = std::mem::zeroed();
                GetCursorPos(&mut pt);
                SetForegroundWindow(hwnd);
                TrackPopupMenu(menu, TPM_LEFTALIGN | TPM_BOTTOMALIGN, pt.x, pt.y, 0, hwnd, null());
                DestroyMenu(menu);
            }
            0
        }
        WM_COMMAND => {
            let id = (wparam & 0xFFFF) as usize;
            let code = ((wparam >> 16) & 0xFFFF) as u32;

            match id {
                ID_COMBO_NODE if code == CBN_SELCHANGE => {
                    if let Some(ui) = ui_mut() {
                        let sel = SendMessageW(ui.combo_node, CB_GETCURSEL, 0, 0) as i32;
                        let nodes = ui.core.get_persisted().nodes;
                        if let Some(node) = nodes.get(sel as usize) {
                            let node_id = node.id.clone();
                            let was_conn = ui.core.desired();
                            let _ = ui.core.mutate_persisted(|p| {
                                p.settings.active_node_id = Some(node_id);
                            });
                            if was_conn {
                                let _ = ui.core.connect();
                            }
                            refresh_nodes_ui(ui);
                            refresh_status_ui(ui);
                        }
                    }
                }
                ID_BTN_CONNECT | ID_TRAY_TOGGLE => {
                    if let Some(ui) = ui_mut() {
                        let res = if matches!(ui.core.status(), Status::Starting | Status::Listening | Status::Running) {
                            ui.core.disconnect();
                            Ok(())
                        } else {
                            ui.core.connect().map(|_| ())
                        };
                        refresh_status_ui(ui);
                        if let Err(e) = res {
                            msg_box(hwnd, "连接失败", &e, MB_OK | MB_ICONERROR);
                        }
                    }
                }
                ID_BTN_A2S_HOME => {
                    if let Some(ui) = ui_ref() {
                        if let Some(n) = ui.core.active_node() {
                            trigger_a2s_probe(hwnd, n);
                        } else {
                            msg_box(hwnd, "提示", "请先添加节点。", MB_OK | MB_ICONWARNING);
                        }
                    }
                }
                ID_BTN_CHECK_IP => {
                    if let Some(ui) = ui_ref() {
                        if !ui.core.is_connected() {
                            msg_box(hwnd, "提示", "请先连接节点后再检测出口 IP。", MB_OK | MB_ICONWARNING);
                        } else {
                            let port = ui.core.get_persisted().settings.mixed_port;
                            let hwnd_raw = hwnd as usize;
                            probe::start_exit_ip_check(port, ui.ip_sender.clone(), move || {
                                PostMessageW(hwnd_raw as HWND, WM_IP_DONE, 0, 0);
                            });
                        }
                    }
                }
                ID_BTN_APPLY_CFG => {
                    if let Some(ui) = ui_mut() {
                        let mut s = ui.core.get_persisted().settings;
                        s.proxy_mode = if SendMessageW(ui.radio_tun, BM_GETCHECK, 0, 0) as usize == BST_CHECKED {
                            "tun".to_string()
                        } else {
                            "system".to_string()
                        };
                        s.routing_mode = if SendMessageW(ui.radio_global, BM_GETCHECK, 0, 0) as usize == BST_CHECKED {
                            "global".to_string()
                        } else if SendMessageW(ui.radio_direct, BM_GETCHECK, 0, 0) as usize == BST_CHECKED {
                            "direct".to_string()
                        } else {
                            "rule".to_string()
                        };
                        s.mixed_port = get_text(ui.edit_mixed_port).trim().parse().unwrap_or(0);
                        let res = ui.core.apply_settings(s);
                        refresh_nodes_ui(ui);
                        refresh_status_ui(ui);
                        match res {
                            Ok(()) => 0,
                            Err(e) => msg_box(hwnd, "应用设置失败", &e, MB_OK | MB_ICONERROR),
                        };
                    }
                }
                ID_BTN_IMPORT_CLIP => {
                    if let Some(clip) = clipboard_read(hwnd) {
                        let (nodes, failed) = uri::parse_batch_uris(&clip);
                        if nodes.is_empty() {
                            msg_box(hwnd, "未检测到有效链接", "剪贴板中未找到 cs2world:// 分享链接。", MB_OK | MB_ICONWARNING);
                        } else if let Some(ui) = ui_mut() {
                            let count = nodes.len();
                            let _ = ui.core.mutate_persisted(|p| {
                                if p.settings.active_node_id.is_none() {
                                    p.settings.active_node_id = nodes.first().map(|n| n.id.clone());
                                }
                                p.nodes.extend(nodes);
                            });
                            ui.core.push_log(&format!("[Nodes] 从剪贴板导入 {count} 个节点"));
                            refresh_nodes_ui(ui);
                            refresh_status_ui(ui);
                            msg_box(hwnd, "导入成功", &format!("已导入 {count} 个 cs2world 节点，跳过 {failed} 条无效记录。"), MB_OK | MB_ICONINFORMATION);
                        }
                    } else {
                        msg_box(hwnd, "提示", "剪贴板为空或不包含文本。", MB_OK | MB_ICONWARNING);
                    }
                }
                ID_BTN_ADD_MANUAL => {
                    open_add_node_dialog(hwnd);
                }
                ID_BTN_COPY_URI => {
                    if let Some(ui) = ui_ref() {
                        let idx = SendMessageW(ui.list_nodes, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as LPARAM) as i32;
                        let nodes = ui.core.get_persisted().nodes;
                        let selected = if idx >= 0 { nodes.get(idx as usize).cloned() } else { ui.core.active_node() };
                        if let Some(node) = selected {
                            let link = match uri::encode_node_uri(&node) {
                                Ok(link) => link,
                                Err(e) => { msg_box(hwnd, "无法无损导出链接", &e, MB_OK | MB_ICONWARNING); return 0; }
                            };
                            clipboard_write(hwnd, &link);
                            msg_box(hwnd, "已复制分享链接", "节点分享链接已复制到剪贴板。", MB_OK | MB_ICONINFORMATION);
                        } else {
                            msg_box(hwnd, "提示", "请先选中一个节点。", MB_OK | MB_ICONWARNING);
                        }
                    }
                }
                ID_BTN_A2S_NODE => {
                    if let Some(ui) = ui_ref() {
                        let idx = SendMessageW(ui.list_nodes, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as LPARAM) as i32;
                        let nodes = ui.core.get_persisted().nodes;
                        let selected = if idx >= 0 { nodes.get(idx as usize).cloned() } else { ui.core.active_node() };
                        if let Some(node) = selected {
                            trigger_a2s_probe(hwnd, node);
                        } else {
                            msg_box(hwnd, "提示", "请先选中一个节点。", MB_OK | MB_ICONWARNING);
                        }
                    }
                }
                ID_BTN_DEL_NODE => {
                    if let Some(ui) = ui_mut() {
                        let idx = SendMessageW(ui.list_nodes, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as LPARAM) as i32;
                        let nodes = ui.core.get_persisted().nodes;
                        if let Some(node) = if idx >= 0 { nodes.get(idx as usize).cloned() } else { None } {
                            if msg_box(hwnd, "确认删除", &format!("确定要删除节点 \"{}\" 吗？", node.name), MB_YESNO | MB_ICONWARNING) == 6 {
                                let _ = ui.core.mutate_persisted(|p| {
                                    p.nodes.retain(|n| n.id != node.id);
                                    if p.settings.active_node_id.as_deref() == Some(node.id.as_str()) {
                                        p.settings.active_node_id = p.nodes.first().map(|n| n.id.clone());
                                    }
                                });
                                refresh_nodes_ui(ui);
                                refresh_status_ui(ui);
                            }
                        } else {
                            msg_box(hwnd, "提示", "请先选中要删除的节点。", MB_OK | MB_ICONWARNING);
                        }
                    }
                }
                ID_BTN_COPY_LOGS => {
                    if let Some(ui) = ui_ref() {
                        let txt = ui.core.get_logs_text();
                        clipboard_write(hwnd, &txt);
                        msg_box(hwnd, "已复制", "日志已复制到剪贴板。", MB_OK | MB_ICONINFORMATION);
                    }
                }
                ID_BTN_CLEAR_LOGS => {
                    if let Some(ui) = ui_mut() {
                        ui.core.clear_logs();
                        refresh_logs(ui, true);
                    }
                }
                ID_TRAY_SHOW => {
                    ShowWindow(hwnd, SW_RESTORE);
                    SetForegroundWindow(hwnd);
                }
                ID_TRAY_QUIT => {
                    if let Some(ui) = ui_ref() { ui.core.shutdown(); }
                }
                _ => {}
            }
            0
        }
        WM_CLOSE => {
            ShowWindow(hwnd, SW_HIDE);
            0
        }
        WM_DESTROY => {
            if let Some(ui) = ui_ref() {
                ui.core.shutdown();
                remove_tray_icon(hwnd);
            }
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn main() {
    if let Some(code) = diagnostics::run_if_requested() { std::process::exit(code); }
    unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_STANDARD_CLASSES | ICC_TAB_CLASSES | ICC_LISTVIEW_CLASSES,
        };
        InitCommonControlsEx(&icc);

        let hinst = GetModuleHandleW(null());
        let app_icon = LoadIconW(hinst, 1 as *const u16);
        let bg_brush = GetSysColorBrush(COLOR_BTNFACE);

        let main_cls = to_wide("CS2WorldWin32XPMain");
        let wc = WNDCLASSW {
            style: 0x0008, // CS_DBLCLKS so double-clicking XP title bar maximizes/restores
            lpfnWndProc: Some(main_wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: app_icon,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: bg_brush,
            lpszMenuName: null(),
            lpszClassName: main_cls.as_ptr(),
        };
        RegisterClassW(&wc);

        let dlg_cls = to_wide("CS2WorldAddNodeDlg");
        let wc_dlg = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(add_dlg_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: app_icon,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: bg_brush,
            lpszMenuName: null(),
            lpszClassName: dlg_cls.as_ptr(),
        };
        RegisterClassW(&wc_dlg);

        let win_w = 626;
        let win_h = 442;
        let x = (GetSystemMetrics(SM_CXSCREEN) - win_w) / 2;
        let y = (GetSystemMetrics(SM_CYSCREEN) - win_h) / 2;

        let title = to_wide("CS2World");
        let hwnd = CreateWindowExW(
            0,
            main_cls.as_ptr(),
            title.as_ptr(),
            WS_POPUP | WS_CLIPCHILDREN | WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_SYSMENU | WS_VISIBLE,
            x,
            y,
            win_w,
            win_h,
            null_mut(),
            null_mut(),
            hinst,
            null(),
        );
        ShowWindow(hwnd, SW_SHOW);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if !UI_PTR.is_null() {
            let ui = Box::from_raw(UI_PTR);
            UI_PTR = null_mut();
            ui.core.shutdown();
            ui.core.join();
            for font in [ui.font_ui,ui.font_title,ui.font_mono] { DeleteObject(font as _); }
        }
    }
}

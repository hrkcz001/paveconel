//! Tray icon + control window - the client half of paveconel.
//!
//! This process never touches the EC. It talks to the `paveconel` Windows service over a
//! named pipe ([`crate::ipc`]), which owns the EC and runs the control loop. A background
//! poller follows both the service state and the live telemetry.
//!
//! The tray icon is present **exactly while the service is running**: it is added when the
//! service is detected running and removed when it is not, so the icon means "the service
//! works" and its absence means it does not.
//!
//! The icon itself is one of three pre-rendered emoji (snowflake / steam / fire), picked
//! from the hottest sensor; see [`crate::icons`].

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::COLOR_WINDOW;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NIM_SETVERSION, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::icons::{self, Band};
use crate::ipc::{IpcClient, Snapshot};
use crate::service::{self, SvcState};

const CLASS_NAME: &str = "paveconel_main";
const WINDOW_TITLE: &str = "paveconel";
const TRAY_UID: u32 = 1;
const WM_TRAY: u32 = WM_APP + 1;
const TIMER_REFRESH: usize = 1;

/// Tray click notifications in `NOTIFYICON_VERSION_4`: the event arrives in the low word of
/// `lParam` (the high word carries the icon id), and a left click is `NIN_SELECT`
/// (`WM_USER`), not `WM_LBUTTONUP`.
const NIN_SELECT: u32 = 0x0400;

// Control IDs.
const ID_CPU: i32 = 101;
const ID_GPU: i32 = 102;
const ID_RPM: i32 = 103;
const ID_DUTY: i32 = 104;
const ID_MODE: i32 = 105;
const ID_CURVE: i32 = 201;
const ID_AUTO: i32 = 202;
const ID_EXIT: i32 = 203;
const ID_REG_LBL: i32 = 301;
const ID_REG_EDIT: i32 = 302;
const ID_VAL_EDIT: i32 = 303;
const ID_READ: i32 = 304;
const ID_WRITE: i32 = 305;
const ID_RESULT: i32 = 306;
const ID_FN_LBL: i32 = 404;
const ID_FN_EDIT: i32 = 401;
const ID_APPLY: i32 = 402;
const ID_FN_CUR: i32 = 403;
const ID_UNIFY: i32 = 501;
const ID_KILL_SVC: i32 = 204;

// Menu command IDs.
const CMD_CURVE: usize = 1001;
const CMD_AUTO: usize = 1002;
const CMD_SHOW: usize = 1003;
const CMD_EXIT: usize = 1004;

// Shell_NotifyIcon / button bits kept explicit locally.
const NOTIFYICON_VERSION_4: u32 = 4;
const BM_SETCHECK: u32 = 0x00F1;
const BM_GETCHECK: u32 = 0x00F0;
const BST_UNCHECKED: usize = 0;
const BST_CHECKED: usize = 1;

static ICON_PRESENT: AtomicBool = AtomicBool::new(false);
/// Which emoji is currently installed, so the shell is only poked on a band change.
/// `0xFF` means "nothing installed yet".
static ICON_BAND: AtomicU8 = AtomicU8::new(0xFF);

/// Client-side state: the last telemetry snapshot plus the service state.
pub struct AppState {
    pub snapshot: Mutex<Snapshot>,
    pub running: AtomicBool,
    pub svc: AtomicU8,
    pub client: Mutex<Option<IpcClient>>,
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            snapshot: Mutex::new(Snapshot::default()),
            running: AtomicBool::new(false),
            svc: AtomicU8::new(SvcState::Pending as u8),
            client: Mutex::new(None),
        }
    }

    fn snapshot(&self) -> Snapshot {
        self.snapshot.lock().unwrap().clone()
    }

    fn svc_state(&self) -> SvcState {
        SvcState::from_u8(self.svc.load(Ordering::SeqCst))
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// Follow the service: poll its state and pull live telemetry while it runs.
fn poll_loop(app: Arc<AppState>) {
    loop {
        let st = service::query_state();
        app.svc.store(st as u8, Ordering::SeqCst);
        let running = st == SvcState::Running;
        app.running.store(running, Ordering::SeqCst);

        let mut guard = app.client.lock().unwrap();
        if running {
            if guard.is_none() {
                *guard = IpcClient::connect().ok();
            }
            if let Some(c) = guard.as_ref() {
                match c.status() {
                    Ok(s) => *app.snapshot.lock().unwrap() = s,
                    Err(_) => *guard = None,
                }
            }
        } else {
            *guard = None;
        }
        drop(guard);

        std::thread::sleep(Duration::from_millis(1000));
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

type AppPtr = *const Arc<AppState>;

unsafe fn app(hwnd: HWND) -> Option<&'static AppState> {
    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as AppPtr;
    if p.is_null() {
        None
    } else {
        Some(&*(p))
    }
}

fn set_label(hwnd: HWND, id: i32, text: &str) {
    let control = unsafe { GetDlgItem(hwnd, id) };
    if control != 0 as HWND {
        let w = wide(text);
        unsafe { SetWindowTextW(control, w.as_ptr()) };
    }
}

fn get_text(hwnd: HWND, id: i32) -> String {
    let control = unsafe { GetDlgItem(hwnd, id) };
    if control == 0 as HWND {
        return String::new();
    }
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(control, buf.as_mut_ptr(), buf.len() as i32) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

fn parse_hex_u8(s: &str) -> Option<u8> {
    let t = s.trim();
    let t = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t);
    u8::from_str_radix(t, 16).ok()
}

fn svc_label(state: SvcState) -> &'static str {
    match state {
        SvcState::Running => "running",
        SvcState::Stopped => "stopped",
        SvcState::NotInstalled => "not installed",
        SvcState::Pending => "starting/stopping",
    }
}

fn refresh(hwnd: HWND, state: &AppState) {
    if !state.running.load(Ordering::SeqCst) {
        let svc = state.svc_state();
        set_label(
            hwnd,
            ID_CPU,
            &format!("service {} - no telemetry", svc_label(svc)),
        );
        set_label(hwnd, ID_GPU, "");
        set_label(hwnd, ID_RPM, "");
        set_label(hwnd, ID_DUTY, "");
        set_label(hwnd, ID_MODE, "start it with: sc start paveconel");
        set_label(hwnd, ID_FN_CUR, "");
        return;
    }

    let t = state.snapshot();
    let gpu = if t.gpu_temp == 0 {
        "GPU temp: dGPU asleep".to_string()
    } else {
        format!("GPU temp: {:>3} C", t.gpu_temp)
    };
    set_label(hwnd, ID_CPU, &format!("CPU temp: {:>3} C", t.cpu_temp));
    set_label(hwnd, ID_GPU, &gpu);
    set_label(
        hwnd,
        ID_RPM,
        &format!("RPM: cpu {:>5}  gpu {:>5}", t.cpu_rpm, t.gpu_rpm),
    );
    set_label(
        hwnd,
        ID_DUTY,
        &format!(
            "duty: set cpu {:>3}% gpu {:>3}%  |  EC reads {:>3}% / {:>3}%",
            t.cpu_set, t.gpu_set, t.cpu_duty, t.gpu_duty
        ),
    );
    let mode = if t.mode == 1 { "curve" } else { "EC auto" };
    let unify = if t.unify { "unified" } else { "independent" };
    set_label(
        hwnd,
        ID_MODE,
        &format!("mode: {mode}   fans: {unify}   (service running)"),
    );
    let f: String = t.curve.chars().take(44).collect();
    set_label(hwnd, ID_FN_CUR, &format!("active: {f}"));

    // Keep the coupling checkbox in sync with the service.
    let check = if t.unify { BST_CHECKED } else { BST_UNCHECKED };
    unsafe { SendMessageW(GetDlgItem(hwnd, ID_UNIFY), BM_SETCHECK, check, 0) };
}

/// Which emoji the hottest sensor currently calls for.
fn current_band(state: &AppState) -> Band {
    if !state.running.load(Ordering::SeqCst) {
        return Band::Cold;
    }
    let t = state.snapshot();
    Band::for_temps(t.cpu_temp, t.gpu_temp)
}

fn base_tray_data(hwnd: HWND, icon: HICON) -> NOTIFYICONDATAW {
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = TRAY_UID;
    data.uCallbackMessage = WM_TRAY;
    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
    data.hIcon = icon;
    data
}

fn set_tip(data: &mut NOTIFYICONDATAW, text: &str) {
    let t = wide(text);
    let n = t.len().min(127);
    data.szTip[..n].copy_from_slice(&t[..n]);
    for slot in data.szTip[n..].iter_mut() {
        *slot = 0;
    }
}

/// Add or remove the tray icon so that its presence tracks the service.
fn sync_tray_icon(hwnd: HWND, state: &AppState) {
    let running = state.running.load(Ordering::SeqCst);
    let present = ICON_PRESENT.load(Ordering::SeqCst);
    if running && !present {
        let band = current_band(state);
        let mut data = base_tray_data(hwnd, icons::icon(band));
        set_tip(&mut data, "paveconel - service running");
        let added = unsafe { Shell_NotifyIconW(NIM_ADD, &data) };
        if added == 0 {
            eprintln!(
                "paveconel: Shell_NotifyIcon NIM_ADD failed (win32 error {})",
                unsafe { GetLastError() }
            );
            return;
        }
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) };
        ICON_BAND.store(band as u8, Ordering::SeqCst);
        ICON_PRESENT.store(true, Ordering::SeqCst);
    } else if !running && present {
        let data = base_tray_data(hwnd, std::ptr::null_mut());
        unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
        ICON_BAND.store(0xFF, Ordering::SeqCst);
        ICON_PRESENT.store(false, Ordering::SeqCst);
    }
}

/// Refresh the hover tooltip, switching the emoji when the temperature band changes.
fn update_tray_tooltip(hwnd: HWND, state: &AppState) {
    if !ICON_PRESENT.load(Ordering::SeqCst) {
        return;
    }
    let t = state.snapshot();
    let gpu = if t.gpu_temp == 0 {
        "GPU --".to_string()
    } else {
        format!("GPU {} C", t.gpu_temp)
    };
    let text = format!(
        "CPU {} C | {} | fans {} / {} rpm",
        t.cpu_temp, gpu, t.cpu_rpm, t.gpu_rpm
    );
    // Re-apply the emoji only when the band changes; a plain tooltip refresh leaves the
    // icon alone (no NIF_ICON) so the shell is not asked to reload it every second.
    let band = current_band(state);
    let changed = ICON_BAND.load(Ordering::SeqCst) != band as u8;
    let mut data = base_tray_data(
        hwnd,
        if changed { icons::icon(band) } else { std::ptr::null_mut() },
    );
    if !changed {
        data.uFlags &= !NIF_ICON;
    }
    set_tip(&mut data, &text);
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
    if changed {
        ICON_BAND.store(band as u8, Ordering::SeqCst);
    }
}

fn show_tray_menu(hwnd: HWND) {
    let menu = unsafe { CreatePopupMenu() };
    if menu.is_null() {
        return;
    }
    unsafe {
        for (id, label) in [
            (CMD_SHOW, "Show paveconel"),
            (CMD_CURVE, "Curve Mode"),
            (CMD_AUTO, "EC Auto Mode"),
            (CMD_EXIT, "Kill GUI"),
        ] {
            if id == CMD_EXIT {
                AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
            }
            let w = wide(label);
            AppendMenuW(menu, MF_STRING, id, w.as_ptr());
        }
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            0,
            hwnd,
            std::ptr::null(),
        );
        DestroyMenu(menu);
        if cmd != 0 {
            handle_command(hwnd, cmd as usize);
        }
    }
}

fn handle_command(hwnd: HWND, cmd: usize) {
    let Some(state) = (unsafe { app(hwnd) }) else {
        return;
    };
    let id = cmd as i32;
    let unify_checked = || unsafe {
        SendMessageW(GetDlgItem(hwnd, ID_UNIFY), BM_GETCHECK, 0, 0) as usize == BST_CHECKED
    };

    match cmd {
        CMD_SHOW => unsafe {
            ShowWindow(hwnd, SW_RESTORE);
            SetForegroundWindow(hwnd);
        },
        CMD_EXIT => unsafe {
            DestroyWindow(hwnd);
        },
        CMD_CURVE => {
            if let Some(c) = state.client.lock().unwrap().as_ref() {
                let _ = c.set_mode(true);
            }
        }
        CMD_AUTO => {
            if let Some(c) = state.client.lock().unwrap().as_ref() {
                let _ = c.set_mode(false);
            }
        }
        _ if id == ID_CURVE => {
            if let Some(c) = state.client.lock().unwrap().as_ref() {
                let _ = c.set_mode(true);
            }
        }
        _ if id == ID_AUTO => {
            if let Some(c) = state.client.lock().unwrap().as_ref() {
                let _ = c.set_mode(false);
            }
        }
        _ if id == ID_EXIT => unsafe {
            DestroyWindow(hwnd);
        },
        _ if id == ID_KILL_SVC => {
            // The service stops itself through the SCM; the tray only asks.
            let stopped = match state.client.lock().unwrap().as_ref() {
                Some(c) => c.stop_service().is_ok(),
                None => false,
            };
            set_label(
                hwnd,
                ID_RESULT,
                if stopped {
                    "result: stopping service, fans -> EC auto"
                } else {
                    "result: cannot reach the service"
                },
            );
        },
        _ if id == ID_APPLY => {
            let f = get_text(hwnd, ID_FN_EDIT);
            if !f.trim().is_empty() {
                if let Some(c) = state.client.lock().unwrap().as_ref() {
                    let _ = c.set_curve(&f);
                }
            }
        }
        _ if id == ID_UNIFY => {
            let on = unify_checked();
            if let Some(c) = state.client.lock().unwrap().as_ref() {
                let _ = c.set_unify(on);
            }
        }
        _ if id == ID_READ => {
            if let Some(reg) = parse_hex_u8(&get_text(hwnd, ID_REG_EDIT)) {
                let msg = {
                    let mut msg = format!("result: 0x{reg:02X} read failed");
                    if let Some(c) = state.client.lock().unwrap().as_ref() {
                        if let Ok(Some(v)) = c.reg_read(reg) {
                            msg = format!("result: 0x{reg:02X} = 0x{v:02X} ({v})");
                        }
                    }
                    msg
                };
                set_label(hwnd, ID_RESULT, &msg);
            }
        }
        _ if id == ID_WRITE => {
            if let (Some(reg), Some(val)) = (
                parse_hex_u8(&get_text(hwnd, ID_REG_EDIT)),
                parse_hex_u8(&get_text(hwnd, ID_VAL_EDIT)),
            ) {
                let msg = {
                    let mut msg = format!("result: write of 0x{reg:02X} failed");
                    if let Some(c) = state.client.lock().unwrap().as_ref() {
                        if let Ok(Some(v)) = c.reg_write(reg, val) {
                            msg = format!(
                                "result: wrote 0x{val:02X} -> 0x{reg:02X}; read back 0x{v:02X}"
                            );
                        }
                    }
                    msg
                };
                set_label(hwnd, ID_RESULT, &msg);
            }
        }
        _ => {}
    }
    refresh(hwnd, state);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let hinstance = GetModuleHandleW(std::ptr::null());
            let ctl =
                |class: &str, text: &str, style: u32, x: i32, y: i32, w: i32, h: i32, id: i32| {
                    let c = wide(class);
                    let t = wide(text);
                    CreateWindowExW(
                        0,
                        c.as_ptr(),
                        t.as_ptr(),
                        WS_CHILD | WS_VISIBLE | style,
                        x,
                        y,
                        w,
                        h,
                        hwnd,
                        id as isize as *mut core::ffi::c_void,
                        hinstance,
                        std::ptr::null(),
                    );
                };
            // Status block.
            ctl(
                "STATIC",
                "waiting for the paveconel service...",
                0,
                12,
                12,
                360,
                22,
                ID_CPU,
            );
            ctl("STATIC", "", 0, 12, 38, 360, 22, ID_GPU);
            ctl("STATIC", "", 0, 12, 64, 360, 22, ID_RPM);
            ctl("STATIC", "", 0, 12, 90, 360, 22, ID_DUTY);
            ctl("STATIC", "", 0, 12, 116, 360, 22, ID_MODE);
            // Fan coupling.
            ctl(
                "BUTTON",
                "Drive both fans from the hotter temperature",
                BS_AUTOCHECKBOX as u32,
                12,
                146,
                360,
                22,
                ID_UNIFY,
            );
            // Curve.
            ctl("STATIC", "curve f(T):", 0, 12, 178, 90, 22, ID_FN_LBL);
            ctl("EDIT", "", WS_BORDER, 108, 176, 170, 22, ID_FN_EDIT);
            ctl("BUTTON", "Apply", BS_PUSHBUTTON as u32, 286, 175, 76, 24, ID_APPLY);
            ctl("STATIC", "active: --", 0, 12, 206, 360, 22, ID_FN_CUR);
            // Mode and lifecycle: two rows of two.
            ctl("BUTTON", "Curve Mode", BS_PUSHBUTTON as u32, 12, 238, 176, 26, ID_CURVE);
            ctl("BUTTON", "EC Auto Mode", BS_PUSHBUTTON as u32, 196, 238, 176, 26, ID_AUTO);
            ctl("BUTTON", "Kill GUI", BS_PUSHBUTTON as u32, 12, 270, 176, 26, ID_EXIT);
            ctl("BUTTON", "Kill Service", BS_PUSHBUTTON as u32, 196, 270, 176, 26, ID_KILL_SVC);
            // EC prober.
            ctl("STATIC", "EC prober:", 0, 12, 306, 90, 22, ID_REG_LBL);
            ctl("EDIT", "", WS_BORDER, 108, 304, 60, 22, ID_REG_EDIT);
            ctl("EDIT", "", WS_BORDER, 176, 304, 60, 22, ID_VAL_EDIT);
            ctl("BUTTON", "Read", BS_PUSHBUTTON as u32, 244, 303, 55, 24, ID_READ);
            ctl("BUTTON", "Write", BS_PUSHBUTTON as u32, 304, 303, 55, 24, ID_WRITE);
            ctl("STATIC", "result: --", 0, 12, 334, 360, 22, ID_RESULT);
            // Independent fans are the default, so start unchecked and let telemetry drive it.
            SendMessageW(GetDlgItem(hwnd, ID_UNIFY), BM_SETCHECK, BST_UNCHECKED, 0);
            SetTimer(hwnd, TIMER_REFRESH, 1000, None);
            0
        }
        WM_TIMER => {
            if let Some(state) = app(hwnd) {
                refresh(hwnd, state);
                sync_tray_icon(hwnd, state);
                update_tray_tooltip(hwnd, state);
            }
            0
        }
        WM_COMMAND => {
            handle_command(hwnd, wparam & 0xFFFF);
            0
        }
        WM_TRAY => {
            // v4 packs the icon id into the high word, so the event is the LOWORD only -
            // and a left click arrives as NIN_SELECT, never WM_LBUTTONUP.
            let event = (lparam as u32) & 0xFFFF;
            match event {
                NIN_SELECT | WM_LBUTTONUP => unsafe {
                    ShowWindow(hwnd, SW_RESTORE);
                    SetForegroundWindow(hwnd);
                },
                WM_CONTEXTMENU | WM_RBUTTONUP => show_tray_menu(hwnd),
                _ => {}
            }
            0
        }
        WM_CLOSE => {
            // Closing the window hides it to the tray; the tray app exits via "Exit tray".
            ShowWindow(hwnd, SW_HIDE);
            0
        }
        WM_DESTROY => {
            if ICON_PRESENT.swap(false, Ordering::SeqCst) {
                let data = base_tray_data(hwnd, std::ptr::null_mut());
                unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
            }
            icons::destroy();
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Run the tray client. `start_hidden` keeps the control window out of the way when this
/// process is launched at logon.
pub fn run(app: Arc<AppState>, start_hidden: bool) -> ! {
    {
        let app = app.clone();
        std::thread::spawn(move || poll_loop(app));
    }

    unsafe {
        let hinstance = GetModuleHandleW(std::ptr::null());
        let class = wide(CLASS_NAME);
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: icons::app_icon(),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: (COLOR_WINDOW + 1) as *mut core::ffi::c_void,
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
        };
        RegisterClassW(&wc);

        let title = wide(WINDOW_TITLE);
        let ptr = Box::into_raw(Box::new(app));
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            400,
            420,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            ptr as *const core::ffi::c_void,
        );
        if hwnd.is_null() {
            eprintln!("paveconel: failed to create window");
            std::process::exit(1);
        }
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);
        ShowWindow(hwnd, if start_hidden { SW_HIDE } else { SW_SHOW });

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        std::process::exit(0);
    }
}

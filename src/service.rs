//! Windows service wrapper.
//!
//! `paveconel service` runs under the Service Control Manager: it owns the EC, runs the
//! control loop, and serves the named pipe the tray client talks to. It is registered with
//! `start= auto`, so it comes up at boot, before anyone logs in.
//!
//! The tray icon is a separate, user-session process: services run in session 0 and cannot
//! own a notification-area icon.

use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows_sys::Win32::Foundation::NO_ERROR;
use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows_sys::Win32::System::Services::*;

use crate::state::Shared;

pub const SERVICE_NAME: &str = "paveconel";
pub const SERVICE_DISPLAY: &str = "paveconel EC fan control";

static STOP_FLAG: AtomicBool = AtomicBool::new(false);
static STATUS_HANDLE: AtomicUsize = AtomicUsize::new(0);
static CONFIG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// Directory containing this executable - a service's working directory is System32, so
/// its config must be resolved relative to the binary, not the cwd.
pub fn exe_dir() -> PathBuf {
    let mut buf = [0u16; 512];
    let n = unsafe { GetModuleFileNameW(std::ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32) };
    let s = String::from_utf16_lossy(&buf[..n as usize]);
    PathBuf::from(s)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

unsafe fn set_status(state: u32, wait_hint: u32) {
    let h = STATUS_HANDLE.load(Ordering::SeqCst) as SERVICE_STATUS_HANDLE;
    if h.is_null() {
        return;
    }
    let mut ss: SERVICE_STATUS = std::mem::zeroed();
    ss.dwServiceType = SERVICE_WIN32_OWN_PROCESS;
    ss.dwCurrentState = state;
    ss.dwControlsAccepted = SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN;
    ss.dwWin32ExitCode = 0;
    ss.dwCheckPoint = 0;
    ss.dwWaitHint = wait_hint;
    SetServiceStatus(h, &ss);
}

unsafe extern "system" fn ctrl_handler(
    control: u32,
    _event_type: u32,
    _event_data: *mut c_void,
    _context: *mut c_void,
) -> u32 {
    match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            set_status(SERVICE_STOP_PENDING, 5000);
            STOP_FLAG.store(true, Ordering::SeqCst);
            NO_ERROR
        }
        _ => NO_ERROR,
    }
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut *mut u16) {
    let name = wide(SERVICE_NAME);
    let h = RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(ctrl_handler), std::ptr::null());
    if h.is_null() {
        return;
    }
    STATUS_HANDLE.store(h as usize, Ordering::SeqCst);
    set_status(SERVICE_START_PENDING, 3000);

    let cfg_path = CONFIG_PATH
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(|| exe_dir().join("paveconel.toml"));
    let cfg = crate::config::load(&cfg_path);
    let shared = Arc::new(Shared::new(
        cfg.cpu.function.clone(),
        cfg.gpu.function.clone(),
        cfg.unify_fans,
    ));

    let io = match crate::ec::open(&cfg.ec_driver) {
        Ok(io) => io,
        Err(e) => {
            eprintln!("paveconel service: cannot open EC backend: {e}");
            set_status(SERVICE_STOPPED, 0);
            return;
        }
    };

    // Named-pipe server for the tray and CLI clients (own thread; only needs Arc<Shared>).
    let srv_stop = Arc::new(AtomicBool::new(false));
    {
        let shared = shared.clone();
        let stop = srv_stop.clone();
        std::thread::spawn(move || crate::ipc::run_server(shared, stop));
    }

    // Translate the SCM STOP signal into a loop shutdown request.
    {
        let shared = shared.clone();
        std::thread::spawn(move || {
            while !STOP_FLAG.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(200));
            }
            shared.request_shutdown();
        });
    }

    set_status(SERVICE_RUNNING, 0);

    // Run the control loop on this (service_main) thread: it blocks until shutdown is
    // requested and applies the exit policy on the way out.
    crate::control::run(io.as_ref(), &cfg, &shared, false);

    srv_stop.store(true, Ordering::SeqCst);
    set_status(SERVICE_STOPPED, 0);
}

/// Entry point for `paveconel service`. Must run on the process's main thread.
pub fn run_service(config_path: PathBuf) -> ! {
    *CONFIG_PATH.lock().unwrap() = Some(config_path);
    let mut name = wide(SERVICE_NAME);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_mut_ptr(),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: std::ptr::null_mut(),
            lpServiceProc: None,
        },
    ];
    let ok = unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) };
    if ok == 0 {
        eprintln!(
            "paveconel: not started by the Service Control Manager - run it as a service \
             (`sc start paveconel`), or use `paveconel` for the tray."
        );
        std::process::exit(1);
    }
    std::process::exit(0)
}

/// What the service is currently doing, as seen by the tray client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvcState {
    /// Not registered with the SCM.
    NotInstalled,
    Stopped,
    Running,
    /// Installed but in a transitional state (start/stop pending).
    Pending,
}

impl SvcState {
    pub fn from_u8(v: u8) -> SvcState {
        match v {
            0 => SvcState::NotInstalled,
            1 => SvcState::Stopped,
            2 => SvcState::Running,
            _ => SvcState::Pending,
        }
    }
}

/// Ask the SCM to stop this service, so a client can shut it down without elevation.
///
/// The service runs as LocalSystem, so it can open its own service object and post itself a
/// stop; the SCM then delivers `SERVICE_CONTROL_STOP` to `ctrl_handler`, and the normal
/// stop path (control loop exit, exit policy, `SERVICE_STOPPED`) runs.
pub fn request_self_stop() {
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return;
        }
        let name = wide(SERVICE_NAME);
        let svc = OpenServiceW(scm, name.as_ptr(), SERVICE_STOP);
        if !svc.is_null() {
            let mut st: SERVICE_STATUS = std::mem::zeroed();
            ControlService(svc, SERVICE_CONTROL_STOP, &mut st);
            CloseServiceHandle(svc);
        }
        CloseServiceHandle(scm);
    }
}

/// Query the SCM for our service's state. Cheap enough to call every second.
pub fn query_state() -> SvcState {
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return SvcState::NotInstalled;
        }
        let name = wide(SERVICE_NAME);
        let h = OpenServiceW(scm, name.as_ptr(), SERVICE_QUERY_STATUS);
        if h.is_null() {
            CloseServiceHandle(scm);
            return SvcState::NotInstalled;
        }
        let mut st: SERVICE_STATUS = std::mem::zeroed();
        let ok = QueryServiceStatus(h, &mut st);
        CloseServiceHandle(h);
        CloseServiceHandle(scm);
        if ok == 0 {
            return SvcState::Pending;
        }
        match st.dwCurrentState as u32 {
            SERVICE_RUNNING => SvcState::Running,
            SERVICE_STOPPED => SvcState::Stopped,
            _ => SvcState::Pending,
        }
    }
}

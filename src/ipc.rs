//! Inter-process plumbing between the paveconel Windows service and its tray client.
//!
//! The service is the single owner of the EC; the tray (and any CLI helper) talks to it
//! over a byte-mode named pipe with a tiny line protocol:
//!
//!   client -> server
//!     STATUS             -> "OK <cpu> <gpu> <cpuset> <gpuset> <cpud> <gpud> <cpurpm> <gpurpm> <mode> <unify> <curve>\n"
//!     MODE curve|auto    -> "OK\n"
//!     CURVE <expr>       -> "OK\n" | "ERR <why>\n"
//!     UNIFY on|off       -> "OK\n"
//!     REG r <NN>         -> "OK <val>\n" | "ERR\n"
//!     REG w <NN> <MM>    -> "OK <readback>\n" | "ERR\n"
//!     BYE                -> (close)
//!
//! Every field is decimal. The curve is the remainder of the line, so it may contain
//! spaces. `mode` is 0 = EC auto, 1 = curve. `unify` is 0/1. `<cpuset>`/`<gpuset>` are the
//! duties the loop wrote; `<cpud>`/`<gpud>` are the EC's own readbacks of them, which the EC
//! smooths and which therefore differ.

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::{
    InitializeSecurityDescriptor, SetSecurityDescriptorDacl, SECURITY_ATTRIBUTES,
    SECURITY_DESCRIPTOR,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

use crate::state::{Mode, Shared};

pub const PIPE_NAME: &str = r"\\.\pipe\paveconel";

const GENERIC_READ_WRITE: u32 = 0xC000_0000;

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// A telemetry snapshot pushed to clients. Mirrors the loop's live state.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub cpu_temp: u8,
    pub gpu_temp: u8,
    /// Duty the loop wrote (curve output).
    pub cpu_set: u8,
    pub gpu_set: u8,
    /// Duty the EC reports back; smoothed, so lower than `cpu_set`/`gpu_set` while ramping.
    pub cpu_duty: u8,
    pub gpu_duty: u8,
    pub cpu_rpm: u16,
    pub gpu_rpm: u16,
    pub mode: u8,
    pub unify: bool,
    pub curve: String,
}

fn read_line(pipe: HANDLE) -> io::Result<String> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let mut byte: u8 = 0;
        let mut n: u32 = 0;
        let ok = unsafe { ReadFile(pipe, &mut byte, 1, &mut n, std::ptr::null_mut()) };
        if ok == 0 || n == 0 {
            return Err(io::Error::last_os_error());
        }
        if byte == b'\n' {
            break;
        }
        if byte != b'\r' {
            buf.push(byte);
        }
    }
    String::from_utf8(buf).map_err(|_| io::Error::other("non-utf8 pipe data"))
}

fn write_all(pipe: HANDLE, data: &[u8]) -> io::Result<()> {
    let mut written = 0usize;
    while written < data.len() {
        let mut n: u32 = 0;
        let ok = unsafe {
            WriteFile(
                pipe,
                data[written..].as_ptr(),
                (data.len() - written) as u32,
                &mut n,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        written += n as usize;
    }
    Ok(())
}

fn reply(pipe: HANDLE, line: &str) {
    let mut b = line.as_bytes().to_vec();
    b.push(b'\n');
    let _ = write_all(pipe, &b);
}

fn snapshot_line(shared: &Shared) -> String {
    let t = shared.telemetry();
    let mode = if shared.mode() == Mode::Curve { 1u8 } else { 0u8 };
    let unify = if shared.unify() { 1u8 } else { 0u8 };
    format!(
        "OK {} {} {} {} {} {} {} {} {} {} {}",
        t.cpu_temp,
        t.gpu_temp,
        t.cpu_set,
        t.gpu_set,
        t.cpu_duty,
        t.gpu_duty,
        t.cpu_rpm,
        t.gpu_rpm,
        mode,
        unify,
        shared.curve_cpu()
    )
}

fn parse_snapshot(line: &str) -> io::Result<Snapshot> {
    // "OK 63 43 31 31 31 31 1524 1577 1 1 (T - 45) * (T - 45) / 12"
    let mut it = line.splitn(2, ' ');
    if it.next() != Some("OK") {
        return Err(io::Error::other("bad STATUS reply"));
    }
    let rest = it.next().unwrap_or("");
    // Ten decimal fields, then the curve - which may itself contain spaces, so it is
    // captured whole as the eleventh (and final) `splitn` element.
    let fields: Vec<&str> = rest.splitn(11, ' ').collect();
    if fields.len() < 10 {
        return Err(io::Error::other("short STATUS reply"));
    }
    let n = |i: usize| -> io::Result<u32> {
        fields[i]
            .parse::<u32>()
            .map_err(|_| io::Error::other("bad number in STATUS reply"))
    };
    Ok(Snapshot {
        cpu_temp: n(0)? as u8,
        gpu_temp: n(1)? as u8,
        cpu_set: n(2)? as u8,
        gpu_set: n(3)? as u8,
        cpu_duty: n(4)? as u8,
        gpu_duty: n(5)? as u8,
        cpu_rpm: n(6)? as u16,
        gpu_rpm: n(7)? as u16,
        mode: n(8)? as u8,
        unify: n(9)? != 0,
        curve: fields.get(10).copied().unwrap_or("").to_string(),
    })
}

fn handle_connection(pipe: HANDLE, shared: &Shared) {
    loop {
        let line = match read_line(pipe) {
            Ok(l) => l,
            Err(_) => break, // client disconnected
        };
        let mut parts = line.splitn(2, char::is_whitespace);
        let cmd = parts.next().unwrap_or("");
        let arg = parts.next().unwrap_or("").trim();
        match cmd {
            "STATUS" => reply(pipe, &snapshot_line(shared)),
            "MODE" => {
                shared.set_mode(match arg {
                    "curve" => Mode::Curve,
                    _ => Mode::Auto,
                });
                reply(pipe, "OK");
            }
            "CURVE" => {
                if arg.is_empty() {
                    reply(pipe, "ERR empty curve");
                } else {
                    shared.set_curve_cpu(arg.to_string());
                    shared.set_curve_gpu(arg.to_string());
                    reply(pipe, "OK");
                }
            }
            "UNIFY" => {
                shared.set_unify(arg == "on");
                reply(pipe, "OK");
            }
            "STOP" => {
                // The "Kill Service" path: hand the fans back to EC auto on the way out,
                // rather than the configured latch, then stop through the SCM (this process
                // is LocalSystem, so it may).
                reply(pipe, "OK");
                shared.set_exit_auto();
                crate::service::request_self_stop();
            }
            "REG" => {
                // arg like "r 90" or "w 44 100"
                let mut rp = arg.split_whitespace();
                match (rp.next(), rp.next(), rp.next()) {
                    (Some("r"), Some(reg), None) => {
                        if let Ok(reg) = u8::from_str_radix(reg.trim_start_matches("0x"), 16) {
                            shared.post_reg_request(crate::state::RegRequest { reg, write: None });
                            if let Some(rb) = wait_reg(shared, reg) {
                                reply(pipe, &format!("OK {rb}"));
                                continue;
                            }
                        }
                        reply(pipe, "ERR");
                    }
                    (Some("w"), Some(reg), Some(val)) => {
                        if let (Ok(reg), Ok(val)) = (
                            u8::from_str_radix(reg.trim_start_matches("0x"), 16),
                            u8::from_str_radix(val.trim_start_matches("0x"), 16),
                        ) {
                            shared.post_reg_request(crate::state::RegRequest {
                                reg,
                                write: Some(val),
                            });
                            if let Some(rb) = wait_reg(shared, reg) {
                                reply(pipe, &format!("OK {rb}"));
                                continue;
                            }
                        }
                        reply(pipe, "ERR");
                    }
                    _ => reply(pipe, "ERR"),
                }
            }
            "BYE" => break,
            _ => reply(pipe, "ERR unknown command"),
        }
    }
}

/// Wait for the control loop to answer a register request for `reg`; give up after ~2 s.
fn wait_reg(shared: &Shared, reg: u8) -> Option<u8> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        if let Some((r, _w, rb)) = shared.reg_response() {
            if r == reg {
                return rb;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

/// Named-pipe server. Runs until `stop` is set, serving one connection at a time.
pub fn run_server(shared: Arc<Shared>, stop: Arc<AtomicBool>) {
    let name = wide(PIPE_NAME);
    // A NULL DACL is deliberate: the pipe only carries fan-control traffic between the
    // LocalSystem service and a user-session tray app, and the default DACL of a service
    // would deny the interactive user. There is nothing secret on this pipe.
    let mut sd: SECURITY_DESCRIPTOR = unsafe { std::mem::zeroed() };
    unsafe {
        let p = &mut sd as *mut SECURITY_DESCRIPTOR as *mut core::ffi::c_void;
        InitializeSecurityDescriptor(p, 1); // SECURITY_DESCRIPTOR_REVISION
        SetSecurityDescriptorDacl(p, 1, std::ptr::null(), 0);
    }
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: &mut sd as *mut SECURITY_DESCRIPTOR as *mut core::ffi::c_void,
        bInheritHandle: 0,
    };
    while !stop.load(Ordering::SeqCst) {
        let pipe = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                4096,
                4096,
                2000,
                &sa,
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }
        let connected = unsafe { ConnectNamedPipe(pipe, std::ptr::null_mut()) != 0 }
            || unsafe { GetLastError() == 0x217 }; // ERROR_PIPE_CONNECTED
        if !connected {
            unsafe { CloseHandle(pipe) };
            continue;
        }
        // Serve each client on its own thread, so the tray holding a connection does not
        // lock out the CLI (`paveconel status`); PIPE_UNLIMITED_INSTANCES lifts the cap.
        let conn_shared = shared.clone();
        let conn_pipe = SafeHandle(pipe);
        std::thread::spawn(move || {
            let pipe = conn_pipe.get();
            handle_connection(pipe, &conn_shared);
            unsafe {
                DisconnectNamedPipe(pipe);
                CloseHandle(pipe);
            }
        });
    }
}

/// A `HANDLE` is a raw pointer, so it is `!Send`/`!Sync` by default. A process handle from
/// `CreateFileW` is process-scoped and safe to use from any thread, so this wrapper
/// restores the auto traits for `IpcClient`.
struct SafeHandle(HANDLE);
unsafe impl Send for SafeHandle {}
unsafe impl Sync for SafeHandle {}

impl SafeHandle {
    /// Access via a method, not `.0`: an edition-2021 closure would otherwise capture the
    /// raw `*mut c_void` field directly and skip the `Send` impl above.
    fn get(&self) -> HANDLE {
        self.0
    }
}

/// Client side of the pipe.
pub struct IpcClient {
    handle: SafeHandle,
}

impl IpcClient {
    pub fn connect() -> io::Result<Self> {
        let name = wide(PIPE_NAME);
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ_WRITE,
                0,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(IpcClient {
            handle: SafeHandle(handle),
        })
    }

    fn request(&self, cmd: &str) -> io::Result<String> {
        let mut b = cmd.as_bytes().to_vec();
        b.push(b'\n');
        write_all(self.handle.0, &b)?;
        read_line(self.handle.0)
    }

    pub fn status(&self) -> io::Result<Snapshot> {
        parse_snapshot(&self.request("STATUS")?)
    }

    pub fn set_mode(&self, curve: bool) -> io::Result<()> {
        self.request(if curve { "MODE curve" } else { "MODE auto" })?;
        Ok(())
    }

    pub fn set_curve(&self, expr: &str) -> io::Result<()> {
        self.request(&format!("CURVE {expr}"))?;
        Ok(())
    }

    pub fn set_unify(&self, on: bool) -> io::Result<()> {
        self.request(if on { "UNIFY on" } else { "UNIFY off" })?;
        Ok(())
    }

    /// Ask the service to stop itself, without needing elevation on the client side.
    pub fn stop_service(&self) -> io::Result<()> {
        self.request("STOP")?;
        Ok(())
    }

    pub fn reg_read(&self, reg: u8) -> io::Result<Option<u8>> {
        let r = self.request(&format!("REG r {reg}"))?;
        Ok(r.strip_prefix("OK ")
            .and_then(|v| v.parse::<u8>().ok()))
    }

    pub fn reg_write(&self, reg: u8, val: u8) -> io::Result<Option<u8>> {
        let r = self.request(&format!("REG w {reg} {val}"))?;
        Ok(r.strip_prefix("OK ")
            .and_then(|v| v.parse::<u8>().ok()))
    }
}

impl Drop for IpcClient {
    fn drop(&mut self) {
        let _ = self.request("BYE");
        unsafe { CloseHandle(self.handle.0) };
    }
}
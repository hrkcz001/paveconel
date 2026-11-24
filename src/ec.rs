//! Embedded-controller access abstracted over a privileged I/O-port backend.
//!
//! The default backend is the WinRing0 kernel driver (shipped by NoteBook FanControl). The
//! EC itself is reached with the standard ACPI handshake
//! (same byte-level protocol as OmenMon for this board):
//!   read  reg: WR cmd 0x80 -> CMD port (0x66) ; addr -> DATA port (0x62) ; data <- DATA
//!   write reg: WR cmd 0x81 -> CMD port (0x66) ; addr -> DATA port (0x62) ; data -> DATA
//!
//! NOTE the address byte goes to the DATA port (0x62), not the CMD port (0x66) - this is
//! the HP/OmenMon addressing and differs from some classic ACPI-EC examples.

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE, HANDLE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

use crate::regs::{EC_CMD, EC_DATA, EC_IBF, EC_OBF, EC_READ, EC_WRITE};

/// Privileged I/O-port access. Every EC operation goes through exactly these methods, so
/// swapping the underlying kernel driver (WinRing0 -> inpoutx64, etc.) means providing
/// another implementation of this trait - the EC protocol and all control logic stay
/// untouched.
pub trait IoBackend {
    fn read_port(&self, port: u16) -> io::Result<u8>;
    fn write_port(&self, port: u16, value: u8) -> io::Result<()>;
    #[allow(dead_code)] // part of the backend API surface; used for diagnostics
    fn name(&self) -> &'static str;
}

const DEVICE: &str = r"\\.\WinRing0_1_2_0";
const GENERIC_READ_WRITE: u32 = 0xC000_0000; // GENERIC_READ | GENERIC_WRITE

const OLS_TYPE: u32 = 40000;

/// WinRing0 IOCTL a.k.a. OlsIoctl.h CTL_CODE(40000, func, METHOD_BUFFERED, access).
const fn ioctl(function: u32, access: u32) -> u32 {
    (OLS_TYPE << 16) | (access << 14) | (function << 2)
}
const IOCTL_READ_PORT: u32 = ioctl(0x833, 0x0001); // IOCTL_OLS_READ_IO_PORT_BYTE
const IOCTL_WRITE_PORT: u32 = ioctl(0x836, 0x0002); // IOCTL_OLS_WRITE_IO_PORT_BYTE

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// Handle to the WinRing0 driver device. One instance per process is enough.
pub struct WinRing0 {
    handle: HANDLE,
    _owner: crate::ownership::Owner,
}

impl WinRing0 {
    pub fn open() -> io::Result<Self> {
        let owner = crate::ownership::Owner::acquire()?;
        let path = wide(DEVICE);
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(WinRing0 { handle, _owner: owner })
    }
}

impl IoBackend for WinRing0 {
    fn read_port(&self, port: u16) -> io::Result<u8> {
        // Driver reads the low byte(s) of this buffer as the port number.
        let mut port_in: u32 = port as u32;
        let mut value: u8 = 0;
        let mut returned: u32 = 0;
        let ok = unsafe {
            DeviceIoControl(
                self.handle,
                IOCTL_READ_PORT,
                &mut port_in as *mut u32 as *const _,
                std::mem::size_of::<u32>() as u32,
                &mut value as *mut u8 as *mut _,
                1,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 { return Err(io::Error::last_os_error()); }
        validate_read_length(returned)?;
        Ok(value)
    }

    fn write_port(&self, port: u16, value: u8) -> io::Result<()> {
        #[repr(C)]
        struct WriteIoPortInput {
            port: u32,
            data: u32,
        }
        let input = WriteIoPortInput {
            port: port as u32,
            data: value as u32,
        };
        let mut returned: u32 = 0;
        let ok = unsafe {
            DeviceIoControl(
                self.handle,
                IOCTL_WRITE_PORT,
                &input as *const WriteIoPortInput as *const _,
                std::mem::size_of::<WriteIoPortInput>() as u32,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 { return Err(io::Error::last_os_error()); }
        if returned != 0 { return Err(io::Error::new(io::ErrorKind::InvalidData, "unexpected write response length")); }
        Ok(())
    }

    fn name(&self) -> &'static str {
        "winring0"
    }
}

impl Drop for WinRing0 {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

/// Build the configured I/O-port backend. Today only WinRing0 is implemented; an
/// inpoutx64 backend slots in here without touching anything else.
pub fn open(driver: &str) -> io::Result<Box<dyn IoBackend>> {
    match driver {
        "winring0" => Ok(Box::new(WinRing0::open()?)),
        other => Err(io::Error::other(format!(
            "unknown EC backend '{other}' (supported: winring0)"
        ))),
    }
}

fn validate_read_length(returned: u32) -> io::Result<()> {
    if returned != 1 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "short/invalid WinRing0 read response"));
    }
    Ok(())
}

/// One EC session. Any transport error latches it permanently: no retries or exit writes.
/// A new session is NOT a recovery procedure for a partially sent command.
pub struct Ec<'a> {
    io: &'a dyn IoBackend,
    faulted: std::cell::Cell<bool>,
}
const POLL_LIMIT: usize = 10_000;
impl<'a> Ec<'a> {
    pub fn new(io: &'a dyn IoBackend) -> Self {
        Self { io, faulted: std::cell::Cell::new(false) }
    }
    pub fn is_faulted(&self) -> bool { self.faulted.get() }
    fn transaction<T>(&self, f: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        if self.is_faulted() { return Err(io::Error::other("EC transport fault latched; no further I/O allowed")); }
        // Latch before touching hardware, including in the event of panic unwind.
        self.faulted.set(true);
        let result = f();
        if result.is_ok() { self.faulted.set(false); }
        result
    }
    fn wait(&self, mask: u8, set: bool, phase: &str) -> io::Result<()> {
        for _ in 0..POLL_LIMIT {
            if (self.io.read_port(EC_CMD)? & mask != 0) == set { return Ok(()); }
            std::hint::spin_loop();
        }
        Err(io::Error::new(io::ErrorKind::TimedOut, phase.to_string()))
    }
    pub fn read(&self, reg: u8) -> io::Result<u8> {
        self.transaction(|| {
            self.wait(EC_IBF, false, "read: before command")?;
            self.io.write_port(EC_CMD, EC_READ)?;
            self.wait(EC_IBF, false, "read: before address")?;
            self.io.write_port(EC_DATA, reg)?;
            self.wait(EC_IBF, false, "read: after address")?;
            self.wait(EC_OBF, true, "read: awaiting data")?;
            self.io.read_port(EC_DATA)
        })
    }
    pub fn write(&self, reg: u8, val: u8) -> io::Result<()> {
        self.transaction(|| {
            self.wait(EC_IBF, false, "write: before command")?;
            self.io.write_port(EC_CMD, EC_WRITE)?;
            self.wait(EC_IBF, false, "write: before address")?;
            self.io.write_port(EC_DATA, reg)?;
            self.wait(EC_IBF, false, "write: before value")?;
            self.io.write_port(EC_DATA, val)?;
            self.wait(EC_IBF, false, "write: completion")
        })
    }
    pub fn read_all(&self) -> io::Result<[u8; 256]> {
        let mut buf = [0; 256];
        for (i,b) in buf.iter_mut().enumerate() { *b = self.read(i as u8)?; }
        Ok(buf)
    }
    pub fn read_word(&self, lo: u8) -> io::Result<u16> {
        let l = self.read(lo)? as u16;
        Ok(l | ((self.read(lo + 1)? as u16) << 8))
    }
}

#[cfg(test)]
#[path = "ec_tests.rs"]
mod regression_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    struct Stuck { busy: Cell<bool>, writes: Cell<usize> }
    impl IoBackend for Stuck {
        fn read_port(&self, _: u16) -> io::Result<u8> { Ok(if self.busy.get() { EC_IBF } else { 0 }) }
        fn write_port(&self, _: u16, _: u8) -> io::Result<()> { self.writes.set(self.writes.get()+1); Ok(()) }
        fn name(&self) -> &'static str { "fake" }
    }
    #[test]
    fn timeout_latches_and_prevents_further_writes() {
        let io = Stuck { busy: Cell::new(true), writes: Cell::new(0) };
        let ec = Ec::new(&io);
        assert!(ec.read(0x5a).is_err());
        io.busy.set(false);
        assert!(ec.write(0x2c, 100).is_err());
        assert_eq!(io.writes.get(), 0, "failed transport must never resume I/O");
    }
}

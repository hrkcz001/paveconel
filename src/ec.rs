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
    fn read_port(&self, port: u16) -> u8;
    fn write_port(&self, port: u16, value: u8);
    /// Raw read returning `(value, win32 error code or 0)` - for diagnostics.
    fn raw_read(&self, port: u16) -> (u8, i32);
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
}

impl WinRing0 {
    pub fn open() -> io::Result<Self> {
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
        Ok(WinRing0 { handle })
    }
}

impl IoBackend for WinRing0 {
    fn read_port(&self, port: u16) -> u8 {
        self.raw_read(port).0
    }

    fn raw_read(&self, port: u16) -> (u8, i32) {
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
        let err = if ok == 0 {
            std::io::Error::last_os_error().raw_os_error().unwrap_or(-1)
        } else {
            0
        };
        (value, err)
    }

    fn write_port(&self, port: u16, value: u8) {
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
        if ok == 0 {
            eprintln!("warn: WriteIoPort(0x{port:02X}, 0x{value:02X}) failed: {}", io::Error::last_os_error());
        }
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

/// Embedded-controller byte read/write over an [`IoBackend`].
pub struct Ec<'a> {
    io: &'a dyn IoBackend,
    verbose: bool,
}

impl<'a> Ec<'a> {
    pub fn new(io: &'a dyn IoBackend) -> Self {
        Ec {
            io,
            verbose: std::env::var("PAVECONEL_DEBUG").is_ok(),
        }
    }

    fn status(&self) -> u8 {
        self.io.read_port(EC_CMD)
    }

    /// Wait until the EC input buffer is free (IBF bit clear) - ready to accept a byte.
    fn wait_write(&self) -> bool {
        for _ in 0..10_000 {
            if self.status() & EC_IBF == 0 {
                return true;
            }
        }
        false
    }

    /// Wait until the EC output buffer is full (OBF bit set) - data ready to read.
    fn wait_read(&self) -> bool {
        for _ in 0..10_000 {
            if self.status() & EC_OBF != 0 {
                return true;
            }
        }
        false
    }

    pub fn read(&self, reg: u8) -> Option<u8> {
        let dbg = self.verbose;
        if dbg {
            eprintln!("read 0x{reg:02X}: status0=0x{:02X}", self.status());
        }
        if !self.wait_write() {
            if dbg {
                eprintln!("  wait_write #1 FAILED status=0x{:02X}", self.status());
            }
            return None;
        }
        self.io.write_port(EC_CMD, EC_READ);
        if dbg {
            eprintln!("  wrote cmd 0x80 -> 0x66; status=0x{:02X}", self.status());
        }
        if !self.wait_write() {
            if dbg {
                eprintln!("  wait_write #2 FAILED status=0x{:02X}", self.status());
            }
            return None;
        }
        self.io.write_port(EC_DATA, reg);
        if dbg {
            eprintln!("  wrote addr 0x{reg:02X} -> 0x62; status=0x{:02X}", self.status());
        }
        let ww = self.wait_write();
        let wr = self.wait_read();
        if dbg {
            eprintln!("  wait_write={ww} wait_read={wr} status=0x{:02X}", self.status());
        }
        if !(ww && wr) {
            return None;
        }
        let v = self.io.read_port(EC_DATA);
        if dbg {
            eprintln!("  value=0x{v:02X}");
        }
        Some(v)
    }

    pub fn write(&self, reg: u8, val: u8) {
        if !self.wait_write() {
            return;
        }
        self.io.write_port(EC_CMD, EC_WRITE);
        if !self.wait_write() {
            return;
        }
        self.io.write_port(EC_DATA, reg);
        if !self.wait_write() {
            return;
        }
        self.io.write_port(EC_DATA, val);
    }

    pub fn read_all(&self) -> [u8; 256] {
        let mut buf = [0u8; 256];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = self.read(i as u8).unwrap_or(0);
        }
        buf
    }

    /// 16-bit little-endian word starting at `lo`; `None` if either byte read failed.
    pub fn read_word(&self, lo: u8) -> Option<u16> {
        let l = self.read(lo)? as u16;
        let h = self.read(lo + 1)? as u16;
        Some(l | (h << 8))
    }
}
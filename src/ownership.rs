//! Cooperating paveconel owners only; this does NOT lock Windows ACPI/firmware EC access.
use std::io;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows_sys::Win32::System::Threading::CreateMutexW;

pub struct Owner { handle: HANDLE }
impl Owner {
    pub fn acquire() -> io::Result<Self> { Self::named(r"Global\paveconel.EcOwner.v1") }
    fn named(name: &str) -> io::Result<Self> {
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        // Existence is the lease, not recursive thread ownership: even same-thread second
        // opens must fail. Keep this handle for the full driver lifetime, across sessions.
        let h = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if h.is_null() { return Err(io::Error::last_os_error()); }
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe { CloseHandle(h); }
            return Err(io::Error::new(io::ErrorKind::AlreadyExists,
                "another paveconel EC owner exists; stop it before direct diagnostics"));
        }
        Ok(Self { handle: h })
    }
}
impl Drop for Owner { fn drop(&mut self) { unsafe { CloseHandle(self.handle); } } }
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owner_child_probe() {
        if let Ok(name) = std::env::var("PAVECONEL_TEST_OWNER") {
            assert!(Owner::named(&name).is_err());
        }
    }
    #[test]
    fn owner_refuses_competitors_and_releases_on_drop() {
        let name = format!(r"Local\paveconel.test.{}", std::process::id());
        let first = Owner::named(&name).unwrap();
        assert!(Owner::named(&name).is_err());
        let other = name.clone();
        assert!(std::thread::spawn(move || Owner::named(&other).is_err()).join().unwrap());
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "ownership::tests::owner_child_probe", "--nocapture"])
            .env("PAVECONEL_TEST_OWNER", &name).status().unwrap();
        assert!(child.success());
        drop(first);
        assert!(Owner::named(&name).is_ok());
    }
}

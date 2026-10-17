//! paveconel - Pavilion EC Control Panel.
//!
//! The tool runs in two halves:
//!
//! * `paveconel service` - a Windows service that owns the EC and runs the fan loop. It is
//!   registered with `start= auto`, so it comes up at boot.
//! * `paveconel` (default) - the tray client, a user-session process that shows the icon
//!   and controls the service over a named pipe. It never touches the EC.
//!
//! The CLI helpers (`dump`/`read`/`write`/`raw`/`monitor`/`restore`/`maxfans`) talk to the
//! EC directly and are meant for diagnostics while the service is stopped.

// A console window would flash every time the tray is launched from Explorer or the Run
// key, and Windows would also list a stray `conhost` startup entry. Building as a GUI
// subsystem removes that; CLI subcommands re-attach to the parent console in
// `attach_console`.
#![windows_subsystem = "windows"]

mod config;
mod control;
mod ec;
mod icons;
mod ipc;
mod regs;
mod service;
mod state;
mod ui;

use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::Arc;

use ec::{Ec, IoBackend};

fn parse_hex(s: &str) -> Option<u8> {
    let t = s.trim();
    let t = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t);
    u8::from_str_radix(t, 16).ok()
}

fn config_path(args: &[String], idx: usize) -> PathBuf {
    args.get(idx)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("paveconel.toml"))
}

fn usage() -> ! {
    eprintln!(
        "paveconel - Pavilion EC Control Panel

USAGE:
  paveconel [--hidden]           tray client (default); --hidden starts without the window
  paveconel service              run as a Windows service (started by the SCM)
  paveconel install-service      install the WinRing0 driver + service (run as admin)
  paveconel uninstall-service    remove the service and the driver
  paveconel install-tray         start the tray automatically at logon
  paveconel uninstall-tray       stop starting the tray at logon

  paveconel dump                 dump all 256 EC bytes (hex)      [service stopped]
  paveconel read  <REG>          read one register                [service stopped]
  paveconel write <REG> <VAL>    write one register (raw)         [service stopped]
  paveconel raw   <PORT>         raw I/O-port read (diagnostics)  [service stopped]
  paveconel restore              hand fan control back to the EC  [service stopped]
  paveconel maxfans              latch manual control, fans 100%  [service stopped]
  paveconel monitor [CONFIG]     headless fan-curve loop          [service stopped]

Config defaults to ./paveconel.toml (the service uses <exe dir>/paveconel.toml)."
    );
    exit(2)
}

fn open_backend(driver: &str) -> Box<dyn IoBackend> {
    match ec::open(driver) {
        Ok(io) => io,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("  expects the WinRing0 kernel driver to be loaded");
            eprintln!("  (run `paveconel install-service` as admin to install it)");
            exit(1);
        }
    }
}

/// Tray client. Holds no EC access: it follows the service and mirrors it.
fn run_tray(args: &[String]) -> ! {
    let hidden = args.iter().any(|a| a == "--hidden");
    let app = Arc::new(ui::AppState::new());
    ui::run(app, hidden)
}

fn current_exe() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "paveconel.exe".to_string())
}

fn run_checked(program: &str, args: &[String]) -> bool {
    match std::process::Command::new(program).args(args).status() {
        Ok(s) if s.success() => true,
        Ok(s) => {
            eprintln!("{program} exited with {s}");
            false
        }
        Err(e) => {
            eprintln!("cannot run {program}: {e}");
            false
        }
    }
}

fn install_service() -> ! {
    let exe = current_exe();
    let dir = Path::new(&exe)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();

    if !install_driver(&dir) {
        eprintln!("error: could not install the WinRing0 kernel driver; fan control needs it");
        exit(1);
    }

    // Repoint (don't delete) an existing registration. Deleting a running service leaves the
    // name marked for deletion (1072) for as long as any client - the tray polls the SCM
    // every second - still holds a handle on it, so `sc create` after a delete can fail.
    let bin_path = format!("\"{exe}\" service");
    let exists = service_registered(service::SERVICE_NAME);
    let ok = if exists {
        run_checked(
            "sc",
            &[
                "config".into(),
                service::SERVICE_NAME.into(),
                "binPath=".into(),
                bin_path,
                "start=".into(),
                "auto".into(),
                "DisplayName=".into(),
                service::SERVICE_DISPLAY.into(),
            ],
        )
    } else {
        run_checked(
            "sc",
            &[
                "create".into(),
                service::SERVICE_NAME.into(),
                "binPath=".into(),
                bin_path,
                "start=".into(),
                "auto".into(),
                "DisplayName=".into(),
                service::SERVICE_DISPLAY.into(),
            ],
        )
    };

    if ok {
        // Restart if it was running so a changed binPath takes effect, otherwise just start.
        if exists {
            let _ = std::process::Command::new("sc")
                .args(["stop", service::SERVICE_NAME])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        let _ = std::process::Command::new("sc")
            .args(["start", service::SERVICE_NAME])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        println!(
            "registered the WinRing0 driver and the '{}' service; it starts at boot",
            service::SERVICE_NAME
        );
    } else {
        eprintln!(
            "error: could not register the '{}' service. If an earlier run left it marked for\n\
             deletion, close other paveconel processes or reboot, then retry.",
            service::SERVICE_NAME
        );
    }
    exit(if ok { 0 } else { 1 })
}

fn uninstall_service() -> ! {
    // Stop first; deleting a running service otherwise fails.
    let _ = std::process::Command::new("sc")
        .args(["stop", service::SERVICE_NAME])
        .status();
    let ok = run_checked("sc", &["delete".into(), service::SERVICE_NAME.into()]);
    uninstall_driver();
    exit(if ok { 0 } else { 1 })
}

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

/// Name of the WinRing0 kernel service. The driver creates `\\.\WinRing0_1_2_0`, which is
/// what [`crate::ec`] opens, so this name has to match.
const DRIVER_SERVICE: &str = "WinRing0_1_2_0";
const DRIVER_FILE: &str = "WinRing0x64.sys";

/// Whether the service is registered. `sc qc` exits non-zero when it is not.
fn service_registered(name: &str) -> bool {
    std::process::Command::new("sc")
        .args(["qc", name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Install the bundled WinRing0 kernel driver as its own service.
///
/// The driver is BSD-licensed and ships next to the executable, so paveconel does not need
/// NoteBook FanControl to have installed it.
///
/// An existing registration is *repointed*, never deleted: a loaded kernel driver cannot be
/// stopped (`sc stop` fails with 1061), and deleting it leaves the service name marked for
/// deletion, so `sc create` then fails with 1072 until the machine reboots. `sc config` only
/// changes the registry and a loaded instance picks the new path up on the next boot.
fn install_driver(exe_dir: &Path) -> bool {
    let driver = exe_dir.join(DRIVER_FILE);
    if !driver.exists() {
        eprintln!("error: {DRIVER_FILE} is missing next to the executable");
        return false;
    }

    // `sc` wants a native path for a kernel driver's binPath.
    let bin_path = format!(r"\??\{}", driver.display());
    let already = service_registered(DRIVER_SERVICE);

    let ok = if already {
        run_checked(
            "sc",
            &[
                "config".into(),
                DRIVER_SERVICE.into(),
                "binPath=".into(),
                bin_path,
                "start=".into(),
                "system".into(),
            ],
        )
    } else {
        run_checked(
            "sc",
            &[
                "create".into(),
                DRIVER_SERVICE.into(),
                "type=".into(),
                "kernel".into(),
                "start=".into(),
                "system".into(),
                "binPath=".into(),
                bin_path,
                "DisplayName=".into(),
                "WinRing0 (paveconel)".into(),
            ],
        )
    };

    if !ok {
        eprintln!(
            "error: could not register the '{DRIVER_SERVICE}' driver.\n\
             If an earlier run left it marked for deletion, reboot and run this again."
        );
        return false;
    }

    // Load it now if it is not loaded. A system-start driver is usually already up, in which
    // case this returns "already running" - harmless, and the path above applies next boot.
    let _ = std::process::Command::new("sc")
        .args(["start", DRIVER_SERVICE])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    println!("WinRing0 driver registered at {}", driver.display());
    if already {
        println!("  (an already-loaded driver switches to this path on the next reboot)");
    }
    true
}

fn uninstall_driver() {
    let _ = std::process::Command::new("sc")
        .args(["stop", DRIVER_SERVICE])
        .status();
    let _ = std::process::Command::new("sc")
        .args(["delete", DRIVER_SERVICE])
        .status();
    println!("WinRing0 driver removed (a loaded one goes away on the next reboot)");
}

fn install_tray() -> ! {
    let exe = current_exe();
    let ok = run_checked(
        "reg",
        &[
            "add".into(),
            RUN_KEY.into(),
            "/v".into(),
            "paveconel".into(),
            "/t".into(),
            "REG_SZ".into(),
            "/d".into(),
            format!("\"{exe}\" --hidden"),
            "/f".into(),
        ],
    );
    if ok {
        println!("tray client will start at logon (HKCU ...\\Run\\paveconel)");
    }
    exit(if ok { 0 } else { 1 })
}

fn uninstall_tray() -> ! {
    let ok = run_checked(
        "reg",
        &[
            "delete".into(),
            RUN_KEY.into(),
            "/v".into(),
            "paveconel".into(),
            "/f".into(),
        ],
    );
    exit(if ok { 0 } else { 1 })
}

/// Point stdout/stderr at the parent console, but only when they are not already wired up.
///
/// The binary is GUI-subsystem, so nothing would print for a console-launched
/// `paveconel status`. When stdout is already a valid handle (a pipe, or a console opened by
/// the launcher) leave it alone - re-pointing it at `CONOUT$` would steal output from the
/// pipe it was redirected into. Started from Explorer or the Run key there is no parent
/// console, the attach fails, and staying silent is the point.
fn attach_console() {
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{
        AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE,
        STD_OUTPUT_HANDLE,
    };
    unsafe {
        let existing = GetStdHandle(STD_OUTPUT_HANDLE);
        if !existing.is_null() && existing != INVALID_HANDLE_VALUE {
            return;
        }
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        let name: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
        let h = CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if h != INVALID_HANDLE_VALUE {
            SetStdHandle(STD_OUTPUT_HANDLE, h);
            SetStdHandle(STD_ERROR_HANDLE, h);
        }
    }
}

fn main() {
    attach_console();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("");

    // A bare invocation, or a leading flag (`--hidden` is what the Run key passes), is the
    // tray client. Without this the logon entry matched no arm, printed usage and exited.
    if cmd.is_empty() || cmd.starts_with('-') {
        run_tray(&args);
    }

    match cmd {
        "tray" => run_tray(&args),
        "service" => service::run_service(service::exe_dir().join("paveconel.toml")),
        "install-service" => install_service(),
        "uninstall-service" => uninstall_service(),
        "install-tray" => install_tray(),
        "uninstall-tray" => uninstall_tray(),
        "status" => {
            match ipc::IpcClient::connect() {
                Ok(c) => match c.status() {
                    Ok(s) => {
                        println!("service running");
                        println!("  cpu temp : {} C", s.cpu_temp);
                        println!("  gpu temp : {} C", s.gpu_temp);
                        println!("  duty set : cpu {}%  gpu {}%", s.cpu_set, s.gpu_set);
                        println!("  duty EC  : cpu {}%  gpu {}%", s.cpu_duty, s.gpu_duty);
                        println!("  rpm      : cpu {}  gpu {}", s.cpu_rpm, s.gpu_rpm);
                        println!("  mode     : {}", if s.mode == 1 { "curve" } else { "EC auto" });
                        println!("  fans     : {}", if s.unify { "unified" } else { "independent" });
                        println!("  curve    : {}", s.curve);
                        exit(0);
                    }
                    Err(e) => {
                        eprintln!("status read failed: {e}");
                        exit(1);
                    }
                },
                Err(e) => {
                    eprintln!("cannot connect to the paveconel service: {e}");
                    exit(1);
                }
            }
        }
        "dump" | "read" | "write" | "monitor" | "raw" | "restore" | "maxfans" => {}
        _ => usage(),
    }

    let cfg = config::load(&config_path(&args, 1));
    let io = open_backend(&cfg.ec_driver);
    let ec = Ec::new(io.as_ref());

    match cmd {
        "dump" => {
            let buf = ec.read_all();
            println!("   | 00 01 02 03 04 05 06 07 08 09 0A 0B 0C 0D 0E 0F");
            println!("---|------------------------------------------------");
            for row in 0..16 {
                print!("{:02X} |", row * 16);
                for col in 0..16 {
                    print!(" {:02X}", buf[row * 16 + col]);
                }
                println!();
            }
        }
        "read" => {
            let reg = args.get(1).and_then(|s| parse_hex(s)).unwrap_or_else(|| usage());
            match ec.read(reg) {
                Some(v) => println!("0x{reg:02X} = 0x{v:02X} = {v}"),
                None => {
                    eprintln!("error: EC read of 0x{reg:02X} failed (handshake timeout)");
                    exit(1);
                }
            }
        }
        "write" => {
            let reg = args.get(1).and_then(|s| parse_hex(s)).unwrap_or_else(|| usage());
            let val = args.get(2).and_then(|s| parse_hex(s)).unwrap_or_else(|| usage());
            ec.write(reg, val);
            match ec.read(reg) {
                Some(v) => println!("wrote 0x{val:02X} -> 0x{reg:02X}; readback 0x{v:02X}"),
                None => println!("wrote 0x{val:02X} -> 0x{reg:02X}; readback failed"),
            }
        }
        "raw" => {
            let port = args.get(1).and_then(|s| parse_hex(s)).unwrap_or_else(|| usage());
            for _ in 0..5 {
                let (v, err) = io.raw_read(port as u16);
                println!("port 0x{port:02X} -> 0x{v:02X} (err {err})");
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        }
        "monitor" => {
            let shared = Arc::new(state::Shared::new(
                cfg.cpu.function.clone(),
                cfg.gpu.function.clone(),
                cfg.unify_fans,
            ));
            {
                let s = shared.clone();
                if ctrlc::set_handler(move || s.request_shutdown()).is_err() {
                    eprintln!("warn: could not install Ctrl-C handler");
                }
            }
            // Optional bounded run for testing/scripting: PAVECONEL_SECONDS=N makes the
            // loop exit gracefully after N seconds, exercising the exit-policy path.
            if let Ok(Ok(secs)) = std::env::var("PAVECONEL_SECONDS").map(|v| v.parse::<u64>()) {
                let s = shared.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(secs));
                    s.request_shutdown();
                });
            }
            control::run(io.as_ref(), &cfg, &shared, true);
        }
        "restore" => {
            control::ExitGuard::new(io.as_ref(), config::ExitPolicy::Auto).apply();
            let rb = |reg| match ec.read(reg) {
                Some(v) => format!("0x{v:02X}"),
                None => "fail".to_string(),
            };
            println!(
                "auto recipe written: 0x2C={} 0x2D={} 0x62={}",
                rb(regs::XSS1),
                rb(regs::XSS2),
                rb(regs::OMCC)
            );
        }
        "maxfans" => {
            control::ExitGuard::new(io.as_ref(), config::ExitPolicy::MaxFans).apply();
            let rb = |reg| match ec.read(reg) {
                Some(v) => format!("0x{v:02X}"),
                None => "fail".to_string(),
            };
            println!(
                "max-fans recipe written: 0x2C={} 0x2D={} 0x62={}",
                rb(regs::XSS1),
                rb(regs::XSS2),
                rb(regs::OMCC)
            );
        }
        _ => unreachable!(),
    }
}

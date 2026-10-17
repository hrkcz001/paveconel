//! Fan control loop and safety.
//!
//! This unit's EC automatic fan control is unreliable (observed: duty 0 / 0 RPM while
//! the true CPU temperature climbed past 86 C at idle). A running software loop is
//! therefore mandatory; the EC is put into manual mode for the duration and restored
//! to automatic on ANY exit path (see [`RestoreGuard`]).

use std::cell::Cell;
use std::time::Duration;

use crate::config::{self, Config, ExitPolicy};
use crate::ec::{Ec, IoBackend};
use crate::regs::*;
use crate::state::{Mode, Shared};

#[derive(Debug, Clone, Copy, Default)]
pub struct Telemetry {
    pub cpu_temp: u8,
    pub gpu_temp: u8,
    /// Duty the loop last wrote to the EC - the curve output. This is NOT the same as the
    /// EC's own applied-duty readback (`cpu_duty`), which the EC smooths and which therefore
    /// lags the setpoint and reads lower; showing only the readback makes an aggressive
    /// curve look weak.
    pub cpu_set: u8,
    pub gpu_set: u8,
    /// Duty read back from the EC (`0x2E`/`0x2F`).
    pub cpu_duty: u8,
    pub gpu_duty: u8,
    pub cpu_rpm: u16,
    pub gpu_rpm: u16,
    pub mode: u8,
}

impl Telemetry {
    /// Sample the EC. Returns `None` if any read failed: an EC read timeout yields `None`
    /// rather than a fake 0, so the control loop never feeds a 0 temperature into the curve.
    pub fn sample(ec: &Ec) -> Option<Self> {
        Some(Telemetry {
            cpu_temp: ec.read(CPU_TEMP)?,
            gpu_temp: ec.read(GPU_TEMP)?,
            cpu_set: 0,
            gpu_set: 0,
            cpu_duty: ec.read(XGS1)?,
            gpu_duty: ec.read(XGS2)?,
            cpu_rpm: ec.read_word(RPM_CPU_LO)?,
            gpu_rpm: ec.read_word(RPM_GPU_LO)?,
            mode: ec.read(OMCC)?,
        })
    }

    #[allow(dead_code)] // kept for future UI status text
    pub fn is_manual(&self) -> bool {
        self.mode & 0x0F == MANUAL_MODE
    }
}

/// Applies the configured exit policy when dropped. Covers normal return, early `?`, and
/// panic unwind, so a crash never leaves the EC in an unintended state.
pub struct ExitGuard<'a> {
    io: &'a dyn IoBackend,
    policy: Cell<ExitPolicy>,
}

impl<'a> ExitGuard<'a> {
    pub fn new(io: &'a dyn IoBackend, policy: ExitPolicy) -> Self {
        ExitGuard {
            io,
            policy: Cell::new(policy),
        }
    }

    /// Override the exit behaviour at the last moment - e.g. a client asked to stop into EC
    /// auto instead of the configured latch.
    pub fn set_policy(&self, policy: ExitPolicy) {
        self.policy.set(policy);
    }

    /// Write the register recipe for the configured exit policy.
    pub fn apply(&self) {
        let ec = Ec::new(self.io);
        match self.policy.get() {
            // Hand control back to the EC's own automatic control.
            ExitPolicy::Auto => {
                enter_auto(&ec);
            }
            // Latch manual control and pin both fans at maximum.
            ExitPolicy::MaxFans => {
                ec.write(OMCC, MANUAL_MODE);
                ec.write(XSS1, FAN_MAX_DUTY);
                ec.write(XSS2, FAN_MAX_DUTY);
            }
            // Leave the EC exactly as the loop left it.
            ExitPolicy::Keep => {}
        }
    }
}

impl<'a> Drop for ExitGuard<'a> {
    fn drop(&mut self) {
        self.apply();
    }
}

/// Enables manual fan control (latch) - required for duty writes to take effect.
pub fn enter_manual(ec: &Ec) {
    ec.write(OMCC, MANUAL_MODE);
}

/// Hands fan control back to the EC's own automatic control.
pub fn enter_auto(ec: &Ec) {
    ec.write(XSS1, AUTO_DUTY);
    ec.write(XSS2, AUTO_DUTY);
    ec.write(OMCC, AUTO_MODE);
}

/// One control step. Returns the duties that were written.
///
/// Without `unify` (the default) each fan follows its own curve on its own sensor: the CPU
/// fan tracks `0x5A`, the GPU fan `0xB7`. With `unify` both are driven from the hotter of
/// the two through the CPU curve, which keeps the whole region cool as one unit.
pub fn step(ec: &Ec, cfg: &Config, t: &Telemetry, unify: bool) -> (f64, f64) {
    let cpu_t = t.cpu_temp as f64;
    // 0xB7 reads 0 while the dGPU sleeps in P8; fall back to the CPU sensor then.
    let gpu_t = if t.gpu_temp > 0 { t.gpu_temp as f64 } else { cpu_t };

    let (mut cpu_duty, mut gpu_duty) = if unify {
        let hottest = cpu_t.max(gpu_t);
        let d = config::eval(&cfg.cpu, hottest);
        (d, d)
    } else {
        (config::eval(&cfg.cpu, cpu_t), config::eval(&cfg.gpu, gpu_t))
    };

    let hottest = cpu_t.max(if t.gpu_temp > 0 { gpu_t } else { 0.0 });
    if hottest >= cfg.safety.max_safe_temp {
        eprintln!("ALARM: {hottest:.0} C >= max_safe_temp; forcing 100%");
        cpu_duty = 100.0;
        gpu_duty = 100.0;
    } else if hottest >= cfg.safety.critical_temp {
        cpu_duty = 100.0;
        gpu_duty = 100.0;
    }

    ec.write(XSS1, cpu_duty.round().clamp(0.0, 100.0) as u8);
    ec.write(XSS2, gpu_duty.round().clamp(0.0, 100.0) as u8);

    if cpu_duty >= 30.0 && t.cpu_rpm == 0 {
        eprintln!("warn: cpu duty {cpu_duty:.0}% but tacho reads 0 RPM - stalled?");
    }
    if gpu_duty >= 30.0 && t.gpu_rpm == 0 {
        eprintln!("warn: gpu duty {gpu_duty:.0}% but tacho reads 0 RPM - stalled?");
    }

    (cpu_duty, gpu_duty)
}

/// The control loop. Runs until `shared` is asked to shut down, honouring `shared.mode`:
/// in [`Mode::Curve`] it drives the fans from the curve, in [`Mode::Auto`] it hands
/// control back to the EC. Always restores auto on exit.
pub fn run(io: &dyn IoBackend, cfg: &Config, shared: &Shared, verbose: bool) {
    let ec = Ec::new(io);
    let guard = ExitGuard::new(io, cfg.exit_policy);
    let mut driving = false;

    if verbose {
        println!(
            "paveconel: curve control at {} ms poll; Ctrl-C restores EC auto.",
            cfg.poll_interval_ms
        );
    }

    while !shared.is_shutting_down() {
        // A failed EC read must not become a 0 temperature: retry once, and if it still
        // fails skip the tick entirely, leaving the last written duty in place.
        let mut t = match Telemetry::sample(&ec).or_else(|| Telemetry::sample(&ec)) {
            Some(t) => t,
            None => {
                if verbose {
                    println!("paveconel: EC read failed twice; holding current duty.");
                }
                std::thread::sleep(Duration::from_millis(cfg.poll_interval_ms));
                continue;
            }
        };

        // Serve a pending one-shot register access from the UI, if any.
        if let Some(req) = shared.take_reg_request() {
            let read_back = match req.write {
                Some(v) => {
                    ec.write(req.reg, v);
                    ec.read(req.reg)
                }
                None => ec.read(req.reg),
            };
            shared.set_reg_response((req.reg, req.write, read_back));
        }

        // Pick up live curve edits from the UI.
        let mut cfg_now = cfg.clone();
        cfg_now.cpu.function = shared.curve_cpu();
        cfg_now.gpu.function = shared.curve_gpu();

        match shared.mode() {
            Mode::Curve => {
                if !driving {
                    enter_manual(&ec);
                    driving = true;
                }
                let (cpu_duty, gpu_duty) = step(&ec, &cfg_now, &t, shared.unify());
                t.cpu_set = cpu_duty.round().clamp(0.0, 100.0) as u8;
                t.gpu_set = gpu_duty.round().clamp(0.0, 100.0) as u8;
                if verbose {
                    println!(
                        "T cpu {:>3} C gpu {:>3} C | duty set {:>5.1}/{:>5.1} applied {:>3}/{:>3} | rpm {:>5}/{:>5} | omcc 0x{:02X}",
                        t.cpu_temp, t.gpu_temp, cpu_duty, gpu_duty, t.cpu_duty, t.gpu_duty,
                        t.cpu_rpm, t.gpu_rpm, t.mode
                    );
                }
            }
            Mode::Auto => {
                if driving {
                    enter_auto(&ec);
                    driving = false;
                    if verbose {
                        println!("paveconel: control handed back to the EC (auto).");
                    }
                }
            }
        }

        // Published after the step so clients see this tick's setpoint alongside the (older)
        // EC readback, instead of a duty value one tick behind.
        shared.set_telemetry(t);

        std::thread::sleep(Duration::from_millis(cfg.poll_interval_ms));
    }

    // A client-requested stop exits into EC auto rather than the configured latch.
    if shared.exit_as_auto() {
        guard.set_policy(ExitPolicy::Auto);
    }

    drop(guard); // explicit exit-policy write before returning
    if verbose {
        let what = match cfg.exit_policy {
            ExitPolicy::Auto => "EC auto fan control restored",
            ExitPolicy::MaxFans => "fans left at maximum (manual latch)",
            ExitPolicy::Keep => "EC left as-is",
        };
        println!("paveconel: {what}.");
    }
}
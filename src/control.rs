//! Fan control loop and safety.
//!
//! This unit's EC automatic fan control is unreliable (observed: duty 0 / 0 RPM while
//! the true CPU temperature climbed past 86 C at idle). A running software loop is
//! therefore mandatory; the EC is put into manual mode for the duration and restored
//! to the configured exit policy (MaxFans by default) unless the transport has faulted.

use std::cell::Cell;
use std::io;
use std::time::Duration;

use crate::config::{self, Config, ExitPolicy};
use crate::ec::{Ec, IoBackend};
use crate::regs::*;
use crate::state::{Mode, Shared};

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;

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
    /// Broad board-specific sanity bounds; GPU zero is valid in P8. Plausible but
    /// incorrect bytes cannot be detected by this filter.
    pub fn plausible(&self) -> bool {
        (10..=115).contains(&self.cpu_temp)
            && (self.gpu_temp == 0 || (10..=110).contains(&self.gpu_temp))
            && self.cpu_duty <= 100 && self.gpu_duty <= 100
            && self.cpu_rpm <= 10_000 && self.gpu_rpm <= 10_000
            && matches!(self.mode, AUTO_MODE | MANUAL_MODE)
    }

    /// Sample the EC, propagating any error rather than inventing zero telemetry.
    pub fn sample(ec: &Ec) -> io::Result<Self> {
        Ok(Telemetry {
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
/// panic unwind. A faulted transport suppresses the recipe; hard kills cannot run Drop.
pub struct ExitGuard<'a> {
    ec: &'a Ec<'a>,
    policy: ExitPolicy,
    applied: Cell<bool>,
}

impl<'a> ExitGuard<'a> {
    pub fn new(ec: &'a Ec<'a>, policy: ExitPolicy) -> Self {
        ExitGuard {
            ec,
            policy,
            applied: Cell::new(false),
        }
    }

    /// Write the register recipe for the configured exit policy.
    pub fn apply(&self) -> io::Result<()> {
        if self.applied.replace(true) { return Ok(()); }
        if self.ec.is_faulted() { return Err(io::Error::other("faulted EC: exit writes suppressed")); }
        let ec = self.ec;
        match self.policy {
            // Hand control back to the EC's own automatic control.
            ExitPolicy::Auto => {
                enter_auto(ec)?;
            }
            // Latch manual control and pin both fans at maximum.
            ExitPolicy::MaxFans => {
                ec.write(OMCC, MANUAL_MODE)?;
                ec.write(XSS1, FAN_MAX_DUTY)?;
                ec.write(XSS2, FAN_MAX_DUTY)?;
            }
            // Leave the EC exactly as the loop left it.
            ExitPolicy::Keep => {}
        }
        Ok(())
    }
}

impl<'a> Drop for ExitGuard<'a> {
    fn drop(&mut self) {
        if let Err(e) = self.apply() { eprintln!("EC exit policy: {e}"); }
    }
}

/// Enables manual fan control (latch) - required for duty writes to take effect.
pub fn enter_manual(ec: &Ec) -> io::Result<()> {
    ec.write(OMCC, MANUAL_MODE)
}

/// Hands fan control back to the EC's own automatic control.
pub fn enter_auto(ec: &Ec) -> io::Result<()> {
    ec.write(XSS1, AUTO_DUTY)?;
    ec.write(XSS2, AUTO_DUTY)?;
    ec.write(OMCC, AUTO_MODE)
}

/// One control step. Returns the duties that were written.
///
/// Without `unify` each fan follows its own curve on its own sensor: the CPU
/// fan tracks `0x5A`, the GPU fan `0xB7`. With `unify` both are driven from the hotter of
/// the two through the CPU curve (the default), cooling the whole region as one unit.
pub fn step(ec: &Ec, cfg: &Config, t: &Telemetry, unify: bool) -> io::Result<(f64, f64)> {
    if !t.plausible() {
        enter_manual(ec)?;
        ec.write(XSS1, FAN_MAX_DUTY)?;
        ec.write(XSS2, FAN_MAX_DUTY)?;
        return Ok((100.0, 100.0));
    }
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

    ec.write(XSS1, cpu_duty.round().clamp(0.0, 100.0) as u8)?;
    ec.write(XSS2, gpu_duty.round().clamp(0.0, 100.0) as u8)?;

    if cpu_duty >= 30.0 && t.cpu_rpm == 0 {
        eprintln!("warn: cpu duty {cpu_duty:.0}% but tacho reads 0 RPM - stalled?");
    }
    if gpu_duty >= 30.0 && t.gpu_rpm == 0 {
        eprintln!("warn: gpu duty {gpu_duty:.0}% but tacho reads 0 RPM - stalled?");
    }

    Ok((cpu_duty, gpu_duty))
}

/// The control loop. Runs until `shared` is asked to shut down, honouring `shared.mode`:
/// in [`Mode::Curve`] it drives the fans from the curve, in [`Mode::Auto`] it hands
/// control back to the EC. Applies the configured exit policy only on healthy transport.
pub fn run(io: &dyn IoBackend, cfg: &Config, shared: &Shared, verbose: bool) {
    let ec = Ec::new(io);
    let guard = ExitGuard::new(&ec, cfg.exit_policy);
    let mut driving = false;

    if verbose {
        println!(
            "paveconel: curve control at {} ms poll; Ctrl-C applies configured exit policy.",
            cfg.poll_interval_ms
        );
    }

    while !shared.is_shutting_down() {
        let mut t = match Telemetry::sample(&ec) {
            Ok(t) => t,
            Err(e) => {
                shared.set_error(format!("EC transport fault; stopped without further writes: {e}"));
                eprintln!("{}", shared.error());
                break;
            }
        };

        // Register writes are deliberately unavailable, even if posted internally.
        if let Some(req) = shared.take_reg_request() {
            let rb = if req.write.is_none() { ec.read(req.reg).ok() } else { None };
            shared.set_reg_response((req.reg, req.write, rb));
            if ec.is_faulted() { break; }
        }

        // Pick up live curve edits from the UI.
        let mut cfg_now = cfg.clone();
        cfg_now.cpu.function = shared.curve_cpu();
        cfg_now.gpu.function = shared.curve_gpu();

        // Bad telemetry must not select EC auto or reduce cooling. The same checked
        // step forces manual + maximum, even if a client requested auto.
        let mode = if t.plausible() { shared.mode() } else {
            shared.set_error("implausible EC telemetry; forcing maximum fans");
            Mode::Curve
        };
        match mode {
            Mode::Curve => {
                if !driving {
                    if let Err(e) = enter_manual(&ec) { shared.set_error(e.to_string()); break; }
                    driving = true;
                }
                let (cpu_duty, gpu_duty) = match step(&ec, &cfg_now, &t, shared.unify()) {
                    Ok(d) => d,
                    Err(e) => { shared.set_error(e.to_string()); break; }
                };
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
                    if let Err(e) = enter_auto(&ec) { shared.set_error(e.to_string()); break; }
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

    // STOP uses the configured exit policy; it must not silently select unsafe EC auto.

    drop(guard); // explicit exit-policy write before returning
    if verbose && !ec.is_faulted() {
        let what = match cfg.exit_policy {
            ExitPolicy::Auto => "EC auto fan control restored",
            ExitPolicy::MaxFans => "fans left at maximum (manual latch)",
            ExitPolicy::Keep => "EC left as-is",
        };
        println!("paveconel: {what}.");
    }
}
//! Shared state between the UI and the control loop.
#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Mutex;

use crate::control::Telemetry;

/// Which controller owns the fans.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Hand control back to the EC (its own automatic control).
    Auto = 0,
    /// paveconel drives the fans from the configured curve.
    Curve = 1,
}

impl Mode {
    pub fn from_u8(v: u8) -> Mode {
        if v == Mode::Curve as u8 {
            Mode::Curve
        } else {
            Mode::Auto
        }
    }
}

/// State shared between the UI thread and the control-loop thread.
pub struct Shared {
    pub telemetry: Mutex<Telemetry>,
    pub mode: AtomicU8,
    pub shutdown: AtomicBool,
    pub last_error: Mutex<String>,
    /// Pending register access posted by the UI and executed by the control loop.
    pub reg_req: Mutex<Option<RegRequest>>,
    /// Result of the last register access: (register, written value, value read back).
    /// The read-back is `None` when the EC read failed.
    pub reg_resp: Mutex<Option<(u8, Option<u8>, Option<u8>)>>,
    /// Live fan-curve expressions (temperature -> duty %), editable from the UI.
    pub curve_cpu: Mutex<String>,
    pub curve_gpu: Mutex<String>,
    /// Drive both fans from the hotter of the two sensors (UI checkbox).
    pub unify: AtomicBool,
    /// A client asked for the service to stop into EC auto instead of the configured exit
    /// policy (the "Kill Service" button).
    pub exit_auto: AtomicBool,
}

/// A one-shot register access asked for by the UI.
#[derive(Clone, Copy, Debug)]
pub struct RegRequest {
    pub reg: u8,
    /// `Some(value)` writes the register before reading it back; `None` only reads.
    pub write: Option<u8>,
}

impl Shared {
    pub fn new(curve_cpu: String, curve_gpu: String, unify: bool) -> Self {
        Shared {
            telemetry: Mutex::new(Telemetry::default()),
            mode: AtomicU8::new(Mode::Curve as u8),
            shutdown: AtomicBool::new(false),
            last_error: Mutex::new(String::new()),
            reg_req: Mutex::new(None),
            reg_resp: Mutex::new(None),
            curve_cpu: Mutex::new(curve_cpu),
            curve_gpu: Mutex::new(curve_gpu),
            unify: AtomicBool::new(unify),
            exit_auto: AtomicBool::new(false),
        }
    }

    pub fn mode(&self) -> Mode {
        Mode::from_u8(self.mode.load(Ordering::SeqCst))
    }

    pub fn set_mode(&self, m: Mode) {
        self.mode.store(m as u8, Ordering::SeqCst);
    }

    pub fn telemetry(&self) -> Telemetry {
        *self.telemetry.lock().unwrap()
    }

    pub fn set_telemetry(&self, t: Telemetry) {
        *self.telemetry.lock().unwrap() = t;
    }

    pub fn set_error(&self, e: impl Into<String>) {
        *self.last_error.lock().unwrap() = e.into();
    }

    pub fn error(&self) -> String {
        self.last_error.lock().unwrap().clone()
    }

    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }

    pub fn post_reg_request(&self, req: RegRequest) {
        *self.reg_req.lock().unwrap() = Some(req);
    }

    pub fn take_reg_request(&self) -> Option<RegRequest> {
        self.reg_req.lock().unwrap().take()
    }

    pub fn set_reg_response(&self, r: (u8, Option<u8>, Option<u8>)) {
        *self.reg_resp.lock().unwrap() = Some(r);
    }

    pub fn reg_response(&self) -> Option<(u8, Option<u8>, Option<u8>)> {
        *self.reg_resp.lock().unwrap()
    }

    pub fn curve_cpu(&self) -> String {
        self.curve_cpu.lock().unwrap().clone()
    }

    pub fn set_curve_cpu(&self, f: String) {
        *self.curve_cpu.lock().unwrap() = f;
    }

    pub fn curve_gpu(&self) -> String {
        self.curve_gpu.lock().unwrap().clone()
    }

    pub fn set_curve_gpu(&self, f: String) {
        *self.curve_gpu.lock().unwrap() = f;
    }

    pub fn unify(&self) -> bool {
        self.unify.load(Ordering::SeqCst)
    }

    pub fn set_unify(&self, v: bool) {
        self.unify.store(v, Ordering::SeqCst);
    }

    /// Ask for the next exit to hand the fans back to EC auto, whatever the config says.
    pub fn set_exit_auto(&self) {
        self.exit_auto.store(true, Ordering::SeqCst);
    }

    pub fn exit_as_auto(&self) -> bool {
        self.exit_auto.load(Ordering::SeqCst)
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }
}

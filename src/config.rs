//! Configuration loading (TOML). Fan curve is expressed as a function of temperature
//! `T` in Celsius, evaluated with `evalexpr`.

use std::path::Path;

use evalexpr::{ContextWithMutableVariables, EvalexprResult, HashMapContext, Value};

#[derive(Debug, Clone)]
pub struct FanCurve {
    pub function: String,
    pub min_duty: f64,
    pub max_duty: f64,
}

#[derive(Debug, Clone)]
pub struct Safety {
    pub critical_temp: f64, // force 100% above this (C)
    pub max_safe_temp: f64, // log an alarm above this (C)
    pub stall_rpm: u16,     // warn if duty > 0 but a fan reports below this RPM
}

/// What to write to the EC when control stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitPolicy {
    /// Hand control back to the EC's own automatic control.
    Auto,
    /// Latch manual control and leave both fans at 100%.
    MaxFans,
    /// Leave whatever the loop last wrote.
    Keep,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub poll_interval_ms: u64,
    pub cpu: FanCurve,
    pub gpu: FanCurve,
    /// Drive both fans from the single hotter of the two sensors instead of giving each
    /// fan its own curve on its own sensor. Off by default: the CPU and GPU fans are
    /// independently driven on this board.
    pub unify_fans: bool,
    pub safety: Safety,
    /// Which I/O-port backend to use (`winring0` today).
    pub ec_driver: String,
    pub exit_policy: ExitPolicy,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            poll_interval_ms: 1000,
            // Deliberately aggressive: this board idles warm and its EC auto control is
            // broken, so the loop alone has to keep it cool. With the default (below) this
            // is ~25% at 60 C, ~52% at 70 C, ~100% at 80 C. Clamped.
            cpu: FanCurve {
                function: "(T - 45) * (T - 45) / 12".into(),
                min_duty: 25.0,
                max_duty: 100.0,
            },
            // The GPU sensor sits far below the CPU, so the same shape gives the GPU fan
            // its own, quieter ramp off its own sensor.
            gpu: FanCurve {
                function: "(T - 45) * (T - 45) / 12".into(),
                min_duty: 25.0,
                max_duty: 100.0,
            },
            unify_fans: false,
            safety: Safety {
                critical_temp: 90.0,
                max_safe_temp: 100.0,
                stall_rpm: 700,
            },
            ec_driver: "winring0".into(),
            // Default to keeping the fans at maximum: this board's native automatic fan
            // control is broken, so handing control back to it can leave the fans stopped.
            exit_policy: ExitPolicy::MaxFans,
        }
    }
}

pub fn load(path: &Path) -> Config {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Config::default(),
    };
    let mut cfg = Config::default();

    // Manual, dependency-light TOML walk (keeps the config surface small and obvious).
    if let Ok(t) = text.parse::<toml::Value>() {
        if let Some(tab) = t.as_table() {
            if let Some(v) = tab.get("poll_interval_ms").and_then(|x| x.as_integer()) {
                cfg.poll_interval_ms = v.max(100) as u64;
            }
            if let Some(s) = tab.get("safety").and_then(|x| x.as_table()) {
                if let Some(v) = s.get("critical_temp").and_then(|x| x.as_float()) {
                    cfg.safety.critical_temp = v;
                }
                if let Some(v) = s.get("max_safe_temp").and_then(|x| x.as_float()) {
                    cfg.safety.max_safe_temp = v;
                }
                if let Some(v) = s.get("stall_rpm").and_then(|x| x.as_integer()) {
                    cfg.safety.stall_rpm = v as u16;
                }
            }
            if let Some(fan) = tab.get("fan").and_then(|x| x.as_table()) {
                if let Some(v) = fan.get("unify").and_then(|x| x.as_bool()) {
                    cfg.unify_fans = v;
                }
                for (key, dst) in [("cpu", &mut cfg.cpu), ("gpu", &mut cfg.gpu)] {
                    if let Some(table) = fan.get(key).and_then(|x| x.as_table()) {
                        if let Some(v) = table.get("function").and_then(|x| x.as_str()) {
                            dst.function = v.to_string();
                        }
                        if let Some(v) = table.get("min_duty").and_then(|x| x.as_float()) {
                            dst.min_duty = v;
                        }
                        if let Some(v) = table.get("max_duty").and_then(|x| x.as_float()) {
                            dst.max_duty = v;
                        }
                    }
                }
            }
            if let Some(v) = tab
                .get("ec")
                .and_then(|x| x.as_table())
                .and_then(|t| t.get("driver"))
                .and_then(|x| x.as_str())
            {
                cfg.ec_driver = v.to_string();
            }
            if let Some(v) = tab
                .get("on_exit")
                .and_then(|x| x.as_table())
                .and_then(|t| t.get("policy"))
                .and_then(|x| x.as_str())
            {
                cfg.exit_policy = match v {
                    "auto" => ExitPolicy::Auto,
                    "max_fans" => ExitPolicy::MaxFans,
                    "keep" => ExitPolicy::Keep,
                    other => {
                        eprintln!("warn: unknown on_exit.policy '{other}'; keeping default");
                        cfg.exit_policy
                    }
                };
            }
        }
    }

    cfg
}

/// Evaluate `curve.function` at temperature `t`, clamped to [min_duty, max_duty].
pub fn eval(curve: &FanCurve, t: f64) -> f64 {
    let mut ctx = HashMapContext::new();
    let _ = ctx.set_value("T".into(), Value::Float(t));
    let result: EvalexprResult<Value> = evalexpr::eval_with_context(&curve.function, &ctx);
    let duty = match result {
        Ok(Value::Float(v)) => v,
        Ok(Value::Int(v)) => v as f64,
        Ok(_) => 0.0,
        Err(e) => {
            eprintln!("warn: curve function error: {e}");
            0.0
        }
    };
    duty.clamp(curve.min_duty, curve.max_duty)
}
//! EC register map for HP Pavilion Gaming 15-ec1xxx (Quanta G35D, BIOS F.35).
//!
//! This module is a reference map: it intentionally declares the full set of known
//! registers even when the current code only uses some of them.
#![allow(dead_code)]
//!
//! CONFIDENCE POLICY - do not blur these two groups:
//!
//!   [VERIFIED] measured on THIS board by us across an all-night session
//!              (256-byte dumps vs true Tctl, duty sweeps, injection tests).
//!              These are fact. Trust them over any external documentation.
//!
//!   [HYPO]     taken from OmenMon's `EcData.cs` register table (other HP models,
//!              board 87B3 family). NOT tested on this board. Treat as a
//!              hypothesis to be validated one register at a time before use in
//!              any control decision.
//!
//! Never drive a fan curve or a safety decision from a [HYPO] register.

// --- Fan control -----------------------------------------------------
pub const XSS1: u8 = 0x2C; // CPU fan set duty [%] (write); 0xFF = auto
pub const XSS2: u8 = 0x2D; // GPU fan set duty [%] (write); 0xFF = auto
pub const XGS1: u8 = 0x2E; // CPU fan applied duty [%] (read)
pub const XGS2: u8 = 0x2F; // GPU fan applied duty [%] (read)
pub const SRP1: u8 = 0x34; // CPU fan set speed [krpm] - VERIFIED INERT on this board
pub const SRP2: u8 = 0x35; // GPU fan set speed [krpm] - VERIFIED INERT on this board
pub const OMCC: u8 = 0x62; // manual fan control: 0x06 = manual ON, 0x00 = auto
pub const XFCD: u8 = 0x63; // manual auto countdown [s]; does not tick on F.35
pub const FFFF: u8 = 0xEC; // "Max Fan Speed Toggle" (OmenMon) - PENDING TEST
pub const SFAN: u8 = 0xF4; // "Fan Toggle" (OmenMon) - PENDING TEST

// --- Temperature -----------------------------------------------------
pub const CPU_TEMP: u8 = 0x5A; // true CPU temp [C]  [empirical, r=0.995 vs Tctl]
pub const GPU_TEMP: u8 = 0xB7; // GPU temp [C] (reads 0 while dGPU in P8)
pub const CPUT: u8 = 0x57; // OmenMon CPU temp - reads 0 on this board
pub const RTMP: u8 = 0x58; // NOT CPU temp (power/charge-headroom-like) - do not use
pub const TMP1: u8 = 0x59; // crawls 41-51 regardless of real load - do not use

// --- RPM (16-bit little-endian) -------------------------------------
pub const RPM_CPU_LO: u8 = 0xB0;
pub const RPM_CPU_HI: u8 = 0xB1;
pub const RPM_GPU_LO: u8 = 0xB2;
pub const RPM_GPU_HI: u8 = 0xB3;

// --- EC I/O ports (ACPI EC, HP/OmenMon addressing) ------------------
pub const EC_DATA: u16 = 0x62; // data / status-data port
pub const EC_CMD: u16 = 0x66; // command / status port
pub const EC_READ: u8 = 0x80; // RD_EC
pub const EC_WRITE: u8 = 0x81; // WR_EC
pub const EC_OBF: u8 = 0x01; // status bit 0: output buffer full
pub const EC_IBF: u8 = 0x02; // status bit 1: input buffer full

// --- Fan duty reset / auto ------------------------------------------
pub const AUTO_DUTY: u8 = 0xFF; // write to XSS1/XSS2 to hand control back to EC
pub const FAN_MAX_DUTY: u8 = 100; // duty value for the "leave fans at maximum" policy
pub const AUTO_MODE: u8 = 0x00; // OMCC value for automatic (EC) control
pub const MANUAL_MODE: u8 = 0x06; // OMCC value to latch manual fan control
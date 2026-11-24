# pavEConel

**Pav**ilion **EC** Control Panel — direct embedded-controller control for HP Pavilion Gaming
laptops. Rust, Windows.

Written by **Hermes Agent** (Nous Research) at the direction of **hrkcz001**, who specified,
tested and debugged it.

## What it is

pavEConel talks to the laptop's embedded controller directly and drives the cooling fans from
temperature curves. It runs in two halves joined by a local named pipe:

- a **Windows service** that owns the EC and runs the control loop, started at boot;
- a **tray client** in the user session that shows state and changes settings, and never
  touches the EC itself.

The EC exposes more than fan control. This is a personal tool — features get added as they are
needed, not against a roadmap.

## Target hardware

Developed and tested on the **HP Pavilion Gaming 15-ec1xxx** (Quanta G35D mainboard) running
**BIOS F.35**. Register offsets, fan limits and part of the EC behaviour are board- and
BIOS-specific, so a different model or BIOS revision needs its own register map; that lives in
`src/regs.rs`.

Porting in principle is not hard. I/O-port access sits behind a trait, the curve evaluation
and control loop are generic, and the handshake is the standard ACPI EC one:

```
read:  wait IBF=0 ; write 0x80 -> 0x66 ; wait IBF=0 ; write reg -> 0x62 ; wait OBF=1 ; read 0x62
write: wait IBF=0 ; write 0x81 -> 0x66 ; wait IBF=0 ; write reg -> 0x62 ; wait IBF=0 ; write val -> 0x62
```

Privileged port access comes from **WinRing0** (`\\.\WinRing0_1_2_0`) today; a different
kernel driver drops in behind the same trait.

## Features

- Fan curves as functions of temperature, evaluated with `evalexpr`.
- **Unified fans by default:** both follow the hotter sensor through the CPU curve.
  Disable the checkbox for independent CPU/GPU curves.
- Curve mode and EC auto mode, switchable while running.
- A configured exit policy on graceful exit/unwind with healthy transport; default MaxFans.
- Tray icon: an emoji picked by the hottest sensor — snowflake, steam or fire — with a live
  tooltip showing temperatures, duty and fan speeds.
- Control window: live status, curve editor, EC register prober, service lifecycle buttons.
- CLI for diagnostics and scripted setup.

## Build

Requires the MSVC toolchain. `build.bat` sets up the Visual Studio environment and then calls
cargo — needed because Rust is installed outside rustup here, so `cargo` alone cannot find the
linker.

```
build.bat              # debug
build.bat --release    # release
```

The executable's icon is compiled from `assets/paveconel.ico` by `build.rs` through the SDK's
`rc.exe`. Without `rc.exe` on PATH the build still succeeds, just without the icon.

## Install

paveconel ships its own copy of the WinRing0 kernel driver, so NoteBook FanControl is not
required. Run these from an **elevated** shell:

```
paveconel.exe install-service    # WinRing0 driver + the service (start= auto)
sc start paveconel               # or reboot: it comes up on its own
paveconel.exe install-tray       # start the tray automatically at logon
```

`install-service` registers the bundled driver as the `WinRing0_1_2_0` kernel service,
pointed at the driver sitting next to the executable, and replaces any previous registration
of either the driver or the service — so it is safe to re-run. `uninstall-service` removes
both.

The tray autostarts at logon rather than with the service because a Windows service runs in
session 0 and cannot own a notification-area icon. `uninstall-tray` undoes that.

> WinRing0 is on Microsoft's vulnerable-driver blocklist, so on a system where that list is
> enforced the driver will not load. It works on the target machine.

### Scoop

```
scoop bucket add hrkcz001 https://github.com/hrkcz001/scoop_bucket
scoop install hrkcz001/paveconel
sudo paveconel install-service   # driver + service, needs admin once
paveconel install-tray
```

Scoop installs the files per-user; the kernel driver and the service need one elevated run.

## Usage

```
paveconel                      # tray client (default)
paveconel --hidden             # tray client, window hidden (what autostart runs)
paveconel status               # one-shot status from the running service

paveconel dump                 # dump all 256 EC bytes      [service stopped]
paveconel read  0x5A           # read one register          [service stopped]
paveconel write 0x2C 64        # fan register only; hex 64=100% [service stopped]
paveconel raw   0x66           # raw I/O-port read          [service stopped]
paveconel restore              # hand fan control to the EC [service stopped]
paveconel maxfans              # latch manual, fans to 100% [service stopped]
paveconel monitor [CONFIG]     # headless control loop      [service stopped]
```

The bare CLI commands drive the EC directly and are meant for diagnostics while the service is
stopped. A global named mutex, held for the entire driver lifetime, refuses a second
cooperating paveconel owner (service or any direct command, including raw reads).
Access-denied opening that mutex also fails closed. This does not coordinate with older
paveconel binaries, NBFC, OmenMon, Windows ACPI, firmware or other port-access tools.
Stop other controllers before use; never run direct dumps alongside the service.
Use `status` over the pipe while the service runs.

## Configuration

Copy `config.example.toml` to `paveconel.toml` next to the executable and edit it. The service
resolves the file relative to the binary, not the working directory.

```toml
[fan]
unify = true             # default: both follow the hotter sensor through the CPU curve

[fan.cpu]
function = "(T - 45) * (T - 45) / 12"
min_duty = 25
max_duty = 100

[on_exit]
policy = "max_fans"      # auto | max_fans | keep
```

## Safety

- Manual mode is latched for as long as the loop drives the fans. An `ExitGuard` applies the
  configured exit policy on graceful return/panic unwind only while transport is healthy.
  The service STOP command preserves that policy; it no longer forces EC auto.
- `on_exit.policy`:
  - `auto` — hand the fans back to the EC's own control.
  - `max_fans` — leave both fans at 100 %. The default, because this board's automatic control
    is not dependable.
  - `keep` — leave whatever the loop last wrote.
- `critical_temp` and `max_safe_temp` force 100 %.
- I/O errors, malformed WinRing0 read lengths and any handshake timeout latch a transport
  fault. The loop stops immediately: no blind retry, further transactions or exit-policy
  writes. A partially sent EC command cannot safely be recovered by another write recipe.
  Cooling remains at its last successfully applied state, which may be insufficient.
  Do not blindly restart after a fault; investigate/recover the EC and cooling safely first.
- Implausible telemetry forces manual maximum cooling, never a lower curve duty. CPU
  bounds are 10–115 C, GPU 10–110 C (zero permitted for sleeping dGPU), duty 0–100,
  RPM <=10,000 and mode auto/manual. These broad board-specific checks cannot detect
  every plausible-but-wrong byte. Firmware/Windows ACPI access can still interleave with
  our multi-byte protocol; the mutex is NOT an ACPI lock or a fix for that race.
- Named-pipe raw register writes are disabled (the old prober write action is rejected).
  Direct CLI writes allow only fan duty registers 0x2C/0x2D (0–100 or 0xFF) and mode
  0x62 (0 or 6). The existing permissive pipe still allows fan settings and service stop;
  this change is not full local-client authorization hardening.
- Hard kills/power loss cannot run the exit guard. Touchpad/power-loss incidents reported
  alongside garbage EC temperatures have not been root-caused; no hardware cure is claimed.
- A fan commanded at 30 % or more whose tacho reads 0 RPM is reported as stalled.

## License

Apache License 2.0 — see [LICENSE](LICENSE). Copyright 2026 hrkcz001.
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
- **Independent or unified fans.** By default the CPU fan follows the CPU sensor and the GPU
  fan the GPU sensor; a checkbox drives both from whichever is hotter instead.
- Curve mode and EC auto mode, switchable while running.
- An exit policy applied on every exit path, so the EC is never left in an unintended state.
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

```
paveconel.exe install-service    # register the service (start= auto)
sc start paveconel               # or reboot: it comes up on its own
paveconel.exe install-tray       # start the tray automatically at logon
```

The tray autostarts at logon rather than with the service because a Windows service runs in
session 0 and cannot own a notification-area icon. `uninstall-service` and `uninstall-tray`
undo both.

## Usage

```
paveconel                      # tray client (default)
paveconel --hidden             # tray client, window hidden (what autostart runs)
paveconel status               # one-shot status from the running service

paveconel dump                 # dump all 256 EC bytes      [service stopped]
paveconel read  0x5A           # read one register          [service stopped]
paveconel write 0x2C 100       # write one register         [service stopped]
paveconel raw   0x66           # raw I/O-port read          [service stopped]
paveconel restore              # hand fan control to the EC [service stopped]
paveconel maxfans              # latch manual, fans to 100% [service stopped]
paveconel monitor [CONFIG]     # headless control loop      [service stopped]
```

The bare CLI commands drive the EC directly and are meant for diagnostics while the service is
stopped — two processes must never drive the EC at once.

## Configuration

Copy `config.example.toml` to `paveconel.toml` next to the executable and edit it. The service
resolves the file relative to the binary, not the working directory.

```toml
[fan]
unify = false            # false: each fan follows its own sensor

[fan.cpu]
function = "(T - 45) * (T - 45) / 12"
min_duty = 25
max_duty = 100

[on_exit]
policy = "max_fans"      # auto | max_fans | keep
```

## Safety

- Manual mode is latched for as long as the loop drives the fans. An `ExitGuard` applies the
  configured exit policy on every exit path, panic unwind included.
- `on_exit.policy`:
  - `auto` — hand the fans back to the EC's own control.
  - `max_fans` — leave both fans at 100 %. The default, because this board's automatic control
    is not dependable.
  - `keep` — leave whatever the loop last wrote.
- `critical_temp` and `max_safe_temp` force 100 %.
- A fan commanded at 30 % or more whose tacho reads 0 RPM is reported as stalled.

## License

Apache License 2.0 — see [LICENSE](LICENSE). Copyright 2026 hrkcz001.
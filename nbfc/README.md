# NoteBook FanControl — fallback configs

paveconel is the primary fan controller. These NoteBook FanControl profiles exist only so
NBFC has something **valid and safe** to load, and they are kept here because reinstalling
or updating NBFC overwrites its `Configs` directory.

Copy them to `C:\Program Files (x86)\NoteBook FanControl\Configs\` if needed.

## What makes a profile here safe

A profile that can leave the fans dead is worse than none. Every profile here carries, for
each fan:

- `MinSpeedValue=0` / `MaxSpeedValue=100` — the write register accepts 0-100 (the applied
  duty saturates around 73-74 regardless);
- `IndependentReadMinMaxValues=true` with a read range of 0-100;
- `ResetRequired=true` / `FanSpeedResetValue=255` — stopping the service hands the fans
  back to the EC instead of leaving them latched;
- a `RegisterWriteConfiguration` that enables EC manual control on init (`0x62 = 6`) and
  restores automatic (`0x00`) on stop;
- no display-only pseudo-fans (`WriteRegister=0` is gone — it made NBFC write to EC
  register 0).

## Register model (verified on the 15-ec1xxx, Quanta G35D, BIOS F.35)

| Register | Meaning |
|---|---|
| `0x2C` / `0x2D` | fan duty write, CPU-side / GPU-side, 0-100; 255 = EC automatic |
| `0x2E` / `0x2F` | fan duty read-back (saturates ~73-74) |
| `0xB0:B1` / `0xB2:B3` | fan tacho, 16-bit little-endian |
| `0x62` | manual fan control: 6 = on, 0 = automatic |
| `0x5A` | true CPU temperature |
| `0xB7` | GPU temperature (reads 0 while the dGPU sleeps) |

No register on this board raises the fans above ~3650 RPM; `0xEC`, `0xF4` and `0x34/0x35`
are all inert.

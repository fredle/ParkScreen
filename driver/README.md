# ParkScreen virtual display driver

A user-mode indirect display driver (IddCx, UMDF 2). While a car is connected in **Extend** mode
the host agent asks it to add one monitor, in exactly the car's resolution. Windows treats that as
an extra screen. When the car leaves, or the agent exits or crashes, the monitor is removed.
**Duplicate** mode does not need this driver.

## Status

Written but **not yet built or run**. The machine it was written on has no Visual Studio or WDK.
Expect compile errors on the first build (IddCx structure names are the likeliest). The Rust side
(`host/src/idd.rs`) is built and tested, including a test that the IOCTL codes and GUID match
`ParkScreenIdd/Ioctl.h`.

## Layout

| File | Purpose |
|---|---|
| `ParkScreenIdd/Driver.cpp` | Device setup, IddCx callbacks, IOCTLs, monitor plug and unplug |
| `ParkScreenIdd/Edid.cpp` | EDID for the requested mode, and the signal-info helper |
| `ParkScreenIdd/SwapChain.cpp` | Thread that drains the swap chain (frames are captured by the host) |
| `ParkScreenIdd/Ioctl.h` | Control interface shared with the host agent |
| `install.ps1` | Install or remove the driver (administrator) |

## Control interface

The host opens the device interface `GUID_DEVINTERFACE_PARKSCREEN` and sends:

- `IOCTL_PARKSCREEN_PLUG` with `{width, height, refresh_hz}`: add the monitor, or switch its mode
  (the driver removes and re-adds it, because the mode comes from the EDID).
- `IOCTL_PARKSCREEN_UNPLUG`: remove it.
- `IOCTL_PARKSCREEN_STATUS`: read the current state.

The device allows interactive users, so the agent needs no administrator rights. Closing the
handle that plugged the monitor removes it.

## Build

Needs Visual Studio 2022 (C++ desktop workload) with the WDK and its extension:

    msbuild driver\ParkScreenIdd\ParkScreenIdd.vcxproj /p:Configuration=Release /p:Platform=x64

CI does the same (`.github/workflows/driver.yml`) and uploads an unsigned package.

## Try it on a development PC

1. `bcdedit /set testsigning on` (administrator), reboot.
2. Sign `ParkScreenIdd.dll` and the `.cat` with a self-signed certificate and trust it in
   `LocalMachine\Root` and `LocalMachine\TrustedPublisher`.
3. `driver\install.ps1 -Package <folder with the .inf, .dll and .cat>`
4. Run the agent, choose **Extend** in the tray menu, connect a car.
   `parkscreen-host list` shows the "ParkScreen" monitor while it is plugged.

## Not done

- **Production signing.** Plan §4.4: test Azure Trusted Signing on this driver (the W4 spike). If
  Windows rejects it, use SignPath Foundation or an EV certificate with attestation signing.
- **Installer.** The MSI/Store installer does not include the driver yet. Until then Extend is
  shown as unavailable in the tray menu, and the agent falls back to Duplicate.
- Stage B (frames handed to the host through a shared texture), HDR, and more than one monitor.

// Control interface between the ParkScreen host agent and the display driver.
// Shared by the driver (C++) and read by a test in host/src/idd.rs, so keep the formats stable.
#pragma once

#include <windows.h>
#include <winioctl.h>
#include <initguid.h>

// Device interface the host opens. {A3C2E1B4-6F5D-4B7A-9E28-1C4D7F0B8A65}
DEFINE_GUID(GUID_DEVINTERFACE_PARKSCREEN, 0xa3c2e1b4, 0x6f5d, 0x4b7a, 0x9e, 0x28, 0x1c, 0x4d, 0x7f, 0x0b, 0x8a, 0x65);

#define PARKSCREEN_FUNC_PLUG   0x800
#define PARKSCREEN_FUNC_UNPLUG 0x801
#define PARKSCREEN_FUNC_STATUS 0x802

// Add the monitor, or change its mode if it is already present. Input: ParkScreenMode.
#define IOCTL_PARKSCREEN_PLUG   CTL_CODE(FILE_DEVICE_UNKNOWN, PARKSCREEN_FUNC_PLUG, METHOD_BUFFERED, FILE_ANY_ACCESS)
// Remove the monitor. No data.
#define IOCTL_PARKSCREEN_UNPLUG CTL_CODE(FILE_DEVICE_UNKNOWN, PARKSCREEN_FUNC_UNPLUG, METHOD_BUFFERED, FILE_ANY_ACCESS)
// Output: ParkScreenStatus.
#define IOCTL_PARKSCREEN_STATUS CTL_CODE(FILE_DEVICE_UNKNOWN, PARKSCREEN_FUNC_STATUS, METHOD_BUFFERED, FILE_ANY_ACCESS)

// Limits: an EDID detailed timing stores each size in 12 bits.
#define PARKSCREEN_MIN_SIZE 320
#define PARKSCREEN_MAX_SIZE 4095
#define PARKSCREEN_MIN_HZ   24
#define PARKSCREEN_MAX_HZ   120

typedef struct _ParkScreenMode {
    UINT32 Width;
    UINT32 Height;
    UINT32 RefreshHz;
} ParkScreenMode;

typedef struct _ParkScreenStatus {
    UINT32 Plugged;  // 0 or 1
    UINT32 Width;
    UINT32 Height;
    UINT32 RefreshHz;
} ParkScreenStatus;

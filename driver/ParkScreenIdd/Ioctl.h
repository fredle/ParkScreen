// Control interface between the ParkScreen host agent and the display driver.
// Shared by the driver (C++) and read by a test in host/src/idd.rs, so keep the formats stable.
#pragma once

#include <windows.h>
#include <initguid.h>

// Device interface the host opens. {A3C2E1B4-6F5D-4B7A-9E28-1C4D7F0B8A65}
DEFINE_GUID(GUID_DEVINTERFACE_PARKSCREEN, 0xa3c2e1b4, 0x6f5d, 0x4b7a, 0x9e, 0x28, 0x1c, 0x4d, 0x7f, 0x0b, 0x8a, 0x65);

// Named pipe the driver serves (message mode). One request per message: u32 function, then the
// input; the reply is an i32 NTSTATUS, then the output. Custom IOCTLs cannot be used: this is a
// display adapter and Windows does not deliver them to the driver.
#define PARKSCREEN_PIPE_NAME L"\\\\.\\pipe\\ParkScreenIdd"

// Add the monitor, or change its mode if it is already present. Input: ParkScreenMode.
// The monitor is removed when the connection that plugged it closes.
#define PARKSCREEN_FUNC_PLUG   0x800
// Remove the monitor. No data.
#define PARKSCREEN_FUNC_UNPLUG 0x801
// Output: ParkScreenStatus.
#define PARKSCREEN_FUNC_STATUS 0x802

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

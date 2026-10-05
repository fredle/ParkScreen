// ParkScreen indirect display driver (IddCx, UMDF 2).
//
// Presents one virtual monitor to Windows while the host agent has asked for it, in exactly the
// mode the car needs. The monitor is added and removed through IOCTLs (Ioctl.h). Windows treats
// it as an extra display, so the car works as an extended screen. The host agent captures the
// monitor with Desktop Duplication; the swap chain here only has to be drained.

#include "Driver.h"

#include <sddl.h>
#include <new>
#include <cstdarg>
#include <cstdio>

namespace {

// Diagnostics: appends a line to ParkScreenIdd.log in the driver host's temp folder
// (C:\Windows\ServiceProfiles\LocalService\AppData\Local\Temp, the host runs as LocalService). Used for failures and adapter state only, so it stays small.
void Log(const char* fmt, ...)
{
    char line[256];
    SYSTEMTIME t;
    GetLocalTime(&t);
    int n = sprintf_s(line, "%02d:%02d:%02d ", t.wHour, t.wMinute, t.wSecond);
    va_list ap;
    va_start(ap, fmt);
    n += vsnprintf_s(line + n, sizeof(line) - n - 2, _TRUNCATE, fmt, ap);
    va_end(ap);
    line[n++] = '\r';
    line[n++] = '\n';
    wchar_t path[MAX_PATH + 32];
    DWORD len = GetTempPathW(MAX_PATH, path);
    if (len == 0 || len > MAX_PATH) return;
    wcscat_s(path, L"ParkScreenIdd.log");
    HANDLE f = CreateFileW(path, FILE_APPEND_DATA, FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr,
                           OPEN_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (f == INVALID_HANDLE_VALUE) return;
    DWORD written;
    WriteFile(f, line, n, &written, nullptr);
    CloseHandle(f);
}

struct State {
    SRWLOCK plugLock = SRWLOCK_INIT;   // serialises plug / unplug (IOCTLs and handle cleanup)
    SRWLOCK procLock = SRWLOCK_INIT;   // guards `processor`; taken by IddCx callbacks
    SRWLOCK modeLock = SRWLOCK_INIT;   // guards `mode` only; never held across an IddCx call (the IddCx
                                       // callbacks read the mode while a plug is still in progress)
    IDDCX_ADAPTER adapter = nullptr;
    NTSTATUS adapterInit = STATUS_PENDING;  // AdapterInitStatus from EvtAdapterInitFinished
    IDDCX_MONITOR monitor = nullptr;
    bool plugged = false;
    ParkScreenMode mode = { 1920, 1080, 60 };
    void* owner = nullptr;             // the pipe connection that plugged the monitor; closing it unplugs
    SwapChainProcessor* processor = nullptr;
    HANDLE departed = nullptr;         // set when IddCx has released the swap chain
    GUID container = {};
} g;

ParkScreenMode CurrentMode()
{
    AcquireSRWLockShared(&g.modeLock);
    ParkScreenMode m = g.mode;
    ReleaseSRWLockShared(&g.modeLock);
    return m;
}

bool ValidMode(const ParkScreenMode& m)
{
    return m.Width >= PARKSCREEN_MIN_SIZE && m.Width <= PARKSCREEN_MAX_SIZE &&
           m.Height >= PARKSCREEN_MIN_SIZE && m.Height <= PARKSCREEN_MAX_SIZE &&
           m.RefreshHz >= PARKSCREEN_MIN_HZ && m.RefreshHz <= PARKSCREEN_MAX_HZ;
}

// Caller holds plugLock.
NTSTATUS UnplugLocked()
{
    if (!g.plugged) return STATUS_SUCCESS;
    ResetEvent(g.departed);
    NTSTATUS st = IddCxMonitorDeparture(g.monitor);
    g.monitor = nullptr;
    g.plugged = false;
    g.owner = nullptr;
    // A new monitor on the same connector must wait until the old swap chain is released.
    WaitForSingleObject(g.departed, 3000);
    return st;
}

// Caller holds plugLock.
NTSTATUS PlugLocked(const ParkScreenMode& mode, void* owner)
{
    if (!g.adapter) return STATUS_DEVICE_NOT_READY;
    if (g.plugged && g.mode.Width == mode.Width && g.mode.Height == mode.Height && g.mode.RefreshHz == mode.RefreshHz) {
        g.owner = owner;
        return STATUS_SUCCESS;
    }
    NTSTATUS st = UnplugLocked();
    if (!NT_SUCCESS(st)) return st;

    AcquireSRWLockExclusive(&g.modeLock);
    g.mode = mode;
    ReleaseSRWLockExclusive(&g.modeLock);
    BYTE edid[128];
    BuildEdid(mode, edid);

    IDDCX_MONITOR_INFO info = {};
    info.Size = sizeof(info);
    info.MonitorType = DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HDMI;
    info.ConnectorIndex = 0;
    info.MonitorDescription.Size = sizeof(info.MonitorDescription);
    info.MonitorDescription.Type = IDDCX_MONITOR_DESCRIPTION_TYPE_EDID;
    info.MonitorDescription.DataSize = sizeof(edid);
    info.MonitorDescription.pData = edid;
    info.MonitorContainerId = g.container;

    WDF_OBJECT_ATTRIBUTES attr;
    WDF_OBJECT_ATTRIBUTES_INIT(&attr);
    IDARG_IN_MONITORCREATE in = {};
    in.ObjectAttributes = &attr;
    in.pMonitorInfo = &info;
    IDARG_OUT_MONITORCREATE out = {};
    st = IddCxMonitorCreate(g.adapter, &in, &out);
    Log("IddCxMonitorCreate 0x%08x", st);
    if (!NT_SUCCESS(st)) { Log("IddCxMonitorCreate failed 0x%08x (adapter init 0x%08x)", st, g.adapterInit); return st; }

    IDARG_OUT_MONITORARRIVAL arrival = {};
    st = IddCxMonitorArrival(out.MonitorObject, &arrival);
    if (!NT_SUCCESS(st)) { Log("IddCxMonitorArrival failed 0x%08x (adapter init 0x%08x)", st, g.adapterInit); return st; }

    g.monitor = out.MonitorObject;
    g.plugged = true;
    g.owner = owner;
    return STATUS_SUCCESS;
}

NTSTATUS Plug(const ParkScreenMode& mode, void* owner)
{
    AcquireSRWLockExclusive(&g.plugLock);
    NTSTATUS st = PlugLocked(mode, owner);
    ReleaseSRWLockExclusive(&g.plugLock);
    return st;
}

NTSTATUS Unplug()
{
    AcquireSRWLockExclusive(&g.plugLock);
    NTSTATUS st = UnplugLocked();
    ReleaseSRWLockExclusive(&g.plugLock);
    return st;
}

// The host agent controls the driver over a named pipe, not IOCTLs: this device is a display
// adapter (IndirectKmd), and Windows does not deliver custom IOCTLs sent to such a device to the
// UMDF driver (they fail with ERROR_NOT_SUPPORTED). Messages: request = u32 function (Ioctl.h)
// followed by its input; response = i32 NTSTATUS followed by its output. Closing the connection
// that plugged the monitor unplugs it, so a crashed agent never leaves a monitor behind.
NTSTATUS HandleRequest(void* conn, const BYTE* in, DWORD inLen, BYTE* out, DWORD* outLen)
{
    *outLen = 0;
    if (inLen < sizeof(UINT32)) return STATUS_INVALID_PARAMETER;
    UINT32 func;
    memcpy(&func, in, sizeof(func));
    in += sizeof(func);
    inLen -= sizeof(func);

    switch (func) {
    case PARKSCREEN_FUNC_PLUG: {
        if (inLen < sizeof(ParkScreenMode)) return STATUS_INVALID_PARAMETER;
        ParkScreenMode mode;
        memcpy(&mode, in, sizeof(mode));
        if (!ValidMode(mode)) return STATUS_INVALID_PARAMETER;
        return Plug(mode, conn);
    }
    case PARKSCREEN_FUNC_UNPLUG:
        return Unplug();
    case PARKSCREEN_FUNC_STATUS: {
        ParkScreenStatus status;
        AcquireSRWLockShared(&g.plugLock);
        status.Plugged = g.plugged ? 1 : 0;
        ParkScreenMode m = CurrentMode();
        status.Width = m.Width;
        status.Height = m.Height;
        status.RefreshHz = m.RefreshHz;
        ReleaseSRWLockShared(&g.plugLock);
        memcpy(out, &status, sizeof(status));
        *outLen = sizeof(status);
        return STATUS_SUCCESS;
    }
    }
    return STATUS_INVALID_DEVICE_REQUEST;
}

DWORD WINAPI ConnectionThread(LPVOID param)
{
    HANDLE pipe = static_cast<HANDLE>(param);
    BYTE in[64];
    BYTE out[sizeof(INT32) + sizeof(ParkScreenStatus)];
    for (;;) {
        DWORD n = 0;
        if (!ReadFile(pipe, in, sizeof(in), &n, nullptr)) break;
        DWORD payload = 0;
        NTSTATUS st = HandleRequest(pipe, in, n, out + sizeof(INT32), &payload);
        if (!NT_SUCCESS(st)) Log("request failed 0x%08x", st);
        INT32 code = static_cast<INT32>(st);
        memcpy(out, &code, sizeof(code));
        DWORD written = 0;
        if (!WriteFile(pipe, out, sizeof(INT32) + payload, &written, nullptr)) break;
    }
    AcquireSRWLockExclusive(&g.plugLock);
    if (g.plugged && g.owner == pipe) UnplugLocked();
    ReleaseSRWLockExclusive(&g.plugLock);
    DisconnectNamedPipe(pipe);
    CloseHandle(pipe);
    return 0;
}

DWORD WINAPI PipeServerThread(LPVOID)
{
    // System, administrators and LocalService (the driver host itself, which must be able to create
    // further pipe instances): everything; interactive users: read and write (the agent runs
    // without administrator rights).
    PSECURITY_DESCRIPTOR sd = nullptr;
    if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(L"D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;LS)(A;;GRGW;;;IU)", SDDL_REVISION_1, &sd, nullptr)) {
        Log("security descriptor failed %lu", GetLastError());
        return 1;
    }
    SECURITY_ATTRIBUTES sa = { sizeof(sa), sd, FALSE };
    for (;;) {
        HANDLE pipe = CreateNamedPipeW(PARKSCREEN_PIPE_NAME, PIPE_ACCESS_DUPLEX,
                                       PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                                       PIPE_UNLIMITED_INSTANCES, 256, 256, 0, &sa);
        if (pipe == INVALID_HANDLE_VALUE) {
            Log("CreateNamedPipe failed %lu", GetLastError());
            Sleep(1000);
            continue;
        }
        if (ConnectNamedPipe(pipe, nullptr) || GetLastError() == ERROR_PIPE_CONNECTED) {
            HANDLE t = CreateThread(nullptr, 0, ConnectionThread, pipe, 0, nullptr);
            if (t) { CloseHandle(t); continue; }
        }
        CloseHandle(pipe);
    }
}

void StartPipeServer()
{
    static bool started = false;
    if (started) return;
    started = true;
    HANDLE t = CreateThread(nullptr, 0, PipeServerThread, nullptr, 0, nullptr);
    if (t) CloseHandle(t); else Log("could not start the pipe server %lu", GetLastError());
}

}  // namespace

EVT_WDF_DRIVER_DEVICE_ADD EvtDeviceAdd;
EVT_WDF_DEVICE_D0_ENTRY EvtDeviceD0Entry;
EVT_IDD_CX_ADAPTER_INIT_FINISHED EvtAdapterInitFinished;
EVT_IDD_CX_ADAPTER_COMMIT_MODES EvtAdapterCommitModes;
EVT_IDD_CX_PARSE_MONITOR_DESCRIPTION EvtParseMonitorDescription;
EVT_IDD_CX_MONITOR_GET_DEFAULT_DESCRIPTION_MODES EvtGetDefaultDescriptionModes;
EVT_IDD_CX_MONITOR_QUERY_TARGET_MODES EvtQueryTargetModes;
EVT_IDD_CX_MONITOR_ASSIGN_SWAPCHAIN EvtAssignSwapChain;
EVT_IDD_CX_MONITOR_UNASSIGN_SWAPCHAIN EvtUnassignSwapChain;

extern "C" DRIVER_INITIALIZE DriverEntry;

extern "C" NTSTATUS DriverEntry(PDRIVER_OBJECT driverObject, PUNICODE_STRING registryPath)
{
    WDF_DRIVER_CONFIG config;
    WDF_DRIVER_CONFIG_INIT(&config, EvtDeviceAdd);
    WDF_OBJECT_ATTRIBUTES attributes;
    WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
    return WdfDriverCreate(driverObject, registryPath, &attributes, &config, WDF_NO_HANDLE);
}

NTSTATUS EvtDeviceAdd(WDFDRIVER, PWDFDEVICE_INIT deviceInit)
{
    if (!g.departed) {
        g.departed = CreateEvent(nullptr, TRUE, TRUE, nullptr);
        CoCreateGuid(&g.container);
    }

    WDF_PNPPOWER_EVENT_CALLBACKS pnp;
    WDF_PNPPOWER_EVENT_CALLBACKS_INIT(&pnp);
    pnp.EvtDeviceD0Entry = EvtDeviceD0Entry;
    WdfDeviceInitSetPnpPowerEventCallbacks(deviceInit, &pnp);

    IDD_CX_CLIENT_CONFIG client;
    IDD_CX_CLIENT_CONFIG_INIT(&client);
    client.EvtIddCxAdapterInitFinished = EvtAdapterInitFinished;
    client.EvtIddCxAdapterCommitModes = EvtAdapterCommitModes;
    client.EvtIddCxParseMonitorDescription = EvtParseMonitorDescription;
    client.EvtIddCxMonitorGetDefaultDescriptionModes = EvtGetDefaultDescriptionModes;
    client.EvtIddCxMonitorQueryTargetModes = EvtQueryTargetModes;
    client.EvtIddCxMonitorAssignSwapChain = EvtAssignSwapChain;
    client.EvtIddCxMonitorUnassignSwapChain = EvtUnassignSwapChain;
    NTSTATUS st = IddCxDeviceInitConfig(deviceInit, &client);
    if (!NT_SUCCESS(st)) return st;

    WDFDEVICE device;
    WDF_OBJECT_ATTRIBUTES attributes;
    WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
    st = WdfDeviceCreate(&deviceInit, &attributes, &device);
    if (!NT_SUCCESS(st)) return st;

    st = IddCxDeviceInitialize(device);
    if (!NT_SUCCESS(st)) return st;

    // The interface only tells the host that the driver is running (IOCTLs sent to it do not reach
    // this driver, see the pipe server above); control goes over the named pipe.
    st = WdfDeviceCreateDeviceInterface(device, &GUID_DEVINTERFACE_PARKSCREEN, nullptr);
    if (!NT_SUCCESS(st)) return st;
    StartPipeServer();
    return STATUS_SUCCESS;
}

NTSTATUS EvtDeviceD0Entry(WDFDEVICE device, WDF_POWER_DEVICE_STATE)
{
    IDDCX_ADAPTER_CAPS caps = {};
    caps.Size = sizeof(caps);
    caps.MaxMonitorsSupported = 1;
    caps.EndPointDiagnostics.Size = sizeof(caps.EndPointDiagnostics);
    caps.EndPointDiagnostics.GammaSupport = IDDCX_FEATURE_IMPLEMENTATION_NONE;
    caps.EndPointDiagnostics.TransmissionType = IDDCX_TRANSMISSION_TYPE_WIRED_OTHER;
    caps.EndPointDiagnostics.pEndPointFriendlyName = L"ParkScreen";
    caps.EndPointDiagnostics.pEndPointManufacturerName = L"ParkScreen";
    caps.EndPointDiagnostics.pEndPointModelName = L"ParkScreen Display";
    IDDCX_ENDPOINT_VERSION version = {};
    version.Size = sizeof(version);
    version.MajorVer = 1;
    caps.EndPointDiagnostics.pFirmwareVersion = &version;
    caps.EndPointDiagnostics.pHardwareVersion = &version;

    WDF_OBJECT_ATTRIBUTES attr;
    WDF_OBJECT_ATTRIBUTES_INIT(&attr);
    IDARG_IN_ADAPTER_INIT in = {};
    in.WdfDevice = device;
    in.pCaps = &caps;
    in.ObjectAttributes = &attr;
    IDARG_OUT_ADAPTER_INIT out = {};
    NTSTATUS st = IddCxAdapterInitAsync(&in, &out);
    if (NT_SUCCESS(st)) g.adapter = out.AdapterObject;
    Log("IddCxAdapterInitAsync 0x%08x", st);
    return st;
}

NTSTATUS EvtAdapterInitFinished(IDDCX_ADAPTER adapter, const IDARG_IN_ADAPTER_INIT_FINISHED* args)
{
    g.adapterInit = args->AdapterInitStatus;
    Log("EvtAdapterInitFinished 0x%08x", args->AdapterInitStatus);
    if (NT_SUCCESS(args->AdapterInitStatus)) g.adapter = adapter;
    return STATUS_SUCCESS;
}

NTSTATUS EvtAdapterCommitModes(IDDCX_ADAPTER, const IDARG_IN_COMMITMODES*)
{
    Log("CommitModes");
    return STATUS_SUCCESS;
}

NTSTATUS EvtParseMonitorDescription(const IDARG_IN_PARSEMONITORDESCRIPTION* in, IDARG_OUT_PARSEMONITORDESCRIPTION* out)
{
    // The EDID is ours, and describes the one mode the host asked for.
    Log("ParseMonitorDescription in=%u", in->MonitorModeBufferInputCount);
    out->MonitorModeBufferOutputCount = 1;
    if (in->MonitorModeBufferInputCount < 1) {
        return in->MonitorModeBufferInputCount > 0 ? STATUS_BUFFER_TOO_SMALL : STATUS_SUCCESS;
    }
    const ParkScreenMode m = CurrentMode();
    IDDCX_MONITOR_MODE& mode = in->pMonitorModes[0];
    mode = {};
    mode.Size = sizeof(mode);
    mode.Origin = IDDCX_MONITOR_MODE_ORIGIN_MONITORDESCRIPTOR;
    FillSignalInfo(mode.MonitorVideoSignalInfo, m.Width, m.Height, m.RefreshHz);
    out->PreferredMonitorModeIdx = 0;
    return STATUS_SUCCESS;
}

NTSTATUS EvtGetDefaultDescriptionModes(IDDCX_MONITOR, const IDARG_IN_GETDEFAULTDESCRIPTIONMODES* in,
                                       IDARG_OUT_GETDEFAULTDESCRIPTIONMODES* out)
{
    Log("GetDefaultDescriptionModes in=%u", in->DefaultMonitorModeBufferInputCount);
    out->DefaultMonitorModeBufferOutputCount = 1;
    if (in->DefaultMonitorModeBufferInputCount < 1) {
        return in->DefaultMonitorModeBufferInputCount > 0 ? STATUS_BUFFER_TOO_SMALL : STATUS_SUCCESS;
    }
    const ParkScreenMode m = CurrentMode();
    IDDCX_MONITOR_MODE& mode = in->pDefaultMonitorModes[0];
    mode = {};
    mode.Size = sizeof(mode);
    mode.Origin = IDDCX_MONITOR_MODE_ORIGIN_DRIVER;
    FillSignalInfo(mode.MonitorVideoSignalInfo, m.Width, m.Height, m.RefreshHz);
    out->PreferredMonitorModeIdx = 0;
    return STATUS_SUCCESS;
}

NTSTATUS EvtQueryTargetModes(IDDCX_MONITOR, const IDARG_IN_QUERYTARGETMODES* in, IDARG_OUT_QUERYTARGETMODES* out)
{
    Log("QueryTargetModes in=%u", in->TargetModeBufferInputCount);
    out->TargetModeBufferOutputCount = 1;
    if (in->TargetModeBufferInputCount < 1) {
        return in->TargetModeBufferInputCount > 0 ? STATUS_BUFFER_TOO_SMALL : STATUS_SUCCESS;
    }
    const ParkScreenMode m = CurrentMode();
    IDDCX_TARGET_MODE& target = in->pTargetModes[0];
    target = {};
    target.Size = sizeof(target);
    FillSignalInfo(target.TargetVideoSignalInfo.targetVideoSignalInfo, m.Width, m.Height, m.RefreshHz);
    return STATUS_SUCCESS;
}

NTSTATUS EvtAssignSwapChain(IDDCX_MONITOR, const IDARG_IN_SETSWAPCHAIN* in)
{
    Log("AssignSwapChain");
    AcquireSRWLockExclusive(&g.procLock);
    delete g.processor;
    g.processor = new (std::nothrow) SwapChainProcessor(in->hSwapChain, in->RenderAdapterLuid, in->hNextSurfaceAvailable);
    ReleaseSRWLockExclusive(&g.procLock);
    return STATUS_SUCCESS;
}

NTSTATUS EvtUnassignSwapChain(IDDCX_MONITOR)
{
    AcquireSRWLockExclusive(&g.procLock);
    delete g.processor;
    g.processor = nullptr;
    ReleaseSRWLockExclusive(&g.procLock);
    SetEvent(g.departed);
    return STATUS_SUCCESS;
}

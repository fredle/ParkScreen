#pragma once

#define NOMINMAX
#include <windows.h>
#include <bugcodes.h>
#include <wudfwdm.h>
#include <wdf.h>
#include <iddcx.h>
#include <dxgi1_5.h>
#include <d3d11_2.h>
#include <wrl/client.h>
#include "Ioctl.h"

// Builds the 128-byte EDID for a monitor named "ParkScreen" whose preferred mode is `mode`.
void BuildEdid(const ParkScreenMode& mode, BYTE (&edid)[128]);

// Fills a DISPLAYCONFIG_VIDEO_SIGNAL_INFO for width x height at hz. `monitorMode` is true for the
// modes of the monitor (description and default modes: vSyncFreqDivider 0) and false for target
// modes (divider 1); IddCx rejects the monitor with STATUS_INVALID_PARAMETER otherwise.
void FillSignalInfo(DISPLAYCONFIG_VIDEO_SIGNAL_INFO& info, UINT32 width, UINT32 height, UINT32 hz, bool monitorMode);

// Pulls frames from the swap chain so Windows keeps presenting to the monitor. The pixels are
// not used here: the host captures the monitor with Desktop Duplication.
class SwapChainProcessor {
public:
    SwapChainProcessor(IDDCX_SWAPCHAIN swapChain, LUID renderAdapter, HANDLE newFrameEvent);
    ~SwapChainProcessor();

private:
    static DWORD CALLBACK Thread(LPVOID self);
    void Run();
    void RunCore();

    IDDCX_SWAPCHAIN m_swapChain;
    LUID m_adapter;
    HANDLE m_newFrame;
    HANDLE m_thread = nullptr;
    HANDLE m_stop = nullptr;
};

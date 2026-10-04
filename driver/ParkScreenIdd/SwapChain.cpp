#include "Driver.h"

using Microsoft::WRL::ComPtr;

SwapChainProcessor::SwapChainProcessor(IDDCX_SWAPCHAIN swapChain, LUID renderAdapter, HANDLE newFrameEvent)
    : m_swapChain(swapChain), m_adapter(renderAdapter), m_newFrame(newFrameEvent)
{
    m_stop = CreateEvent(nullptr, TRUE, FALSE, nullptr);
    m_thread = CreateThread(nullptr, 0, Thread, this, 0, nullptr);
}

SwapChainProcessor::~SwapChainProcessor()
{
    if (m_stop) SetEvent(m_stop);
    if (m_thread) {
        WaitForSingleObject(m_thread, INFINITE);
        CloseHandle(m_thread);
    }
    if (m_stop) CloseHandle(m_stop);
}

DWORD CALLBACK SwapChainProcessor::Thread(LPVOID self)
{
    static_cast<SwapChainProcessor*>(self)->Run();
    return 0;
}

void SwapChainProcessor::Run()
{
    RunCore();
    // Tell IddCx this swap chain is finished with; it deletes the object.
    WdfObjectDelete(reinterpret_cast<WDFOBJECT>(m_swapChain));
    m_swapChain = nullptr;
}

void SwapChainProcessor::RunCore()
{
    ComPtr<IDXGIFactory5> factory;
    if (FAILED(CreateDXGIFactory2(0, IID_PPV_ARGS(&factory)))) return;
    ComPtr<IDXGIAdapter1> adapter;
    if (FAILED(factory->EnumAdapterByLuid(m_adapter, IID_PPV_ARGS(&adapter)))) return;

    ComPtr<ID3D11Device> device;
    ComPtr<ID3D11DeviceContext> context;
    if (FAILED(D3D11CreateDevice(adapter.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                                 nullptr, 0, D3D11_SDK_VERSION, &device, nullptr, &context))) return;
    ComPtr<IDXGIDevice> dxgiDevice;
    if (FAILED(device.As(&dxgiDevice))) return;

    IDARG_IN_SWAPCHAINSETDEVICE setDevice = {};
    setDevice.pDevice = dxgiDevice.Get();
    if (FAILED(IddCxSwapChainSetDevice(m_swapChain, &setDevice))) return;

    const HANDLE waits[] = { m_newFrame, m_stop };
    for (;;) {
        IDARG_OUT_RELEASEANDACQUIREBUFFER buffer = {};
        HRESULT hr = IddCxSwapChainReleaseAndAcquireBuffer(m_swapChain, &buffer);
        if (hr == E_PENDING) {
            DWORD w = WaitForMultipleObjects(2, waits, FALSE, 16);
            if (w == WAIT_OBJECT_0 + 1) return;           // stop requested
            if (w == WAIT_OBJECT_0 || w == WAIT_TIMEOUT) continue;
            return;
        }
        if (FAILED(hr)) return;                           // swap chain gone (monitor removed, mode change)
        // A frame arrived. Nothing to do with it; Desktop Duplication reads the monitor itself.
        hr = IddCxSwapChainFinishedProcessingFrame(m_swapChain);
        if (FAILED(hr)) return;
    }
}

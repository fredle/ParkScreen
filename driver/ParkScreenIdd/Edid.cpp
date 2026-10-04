#include "Driver.h"

#include <algorithm>
#include <cstring>

void FillSignalInfo(DISPLAYCONFIG_VIDEO_SIGNAL_INFO& info, UINT32 width, UINT32 height, UINT32 hz)
{
    info = {};
    info.totalSize.cx = info.activeSize.cx = width;
    info.totalSize.cy = info.activeSize.cy = height;
    info.AdditionalSignalInfo.vSyncFreqDivider = 1;
    info.AdditionalSignalInfo.videoStandard = 255;
    info.vSyncFreq.Numerator = hz;
    info.vSyncFreq.Denominator = 1;
    info.hSyncFreq.Numerator = hz * height;
    info.hSyncFreq.Denominator = 1;
    info.scanLineOrdering = DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE;
    info.pixelRate = static_cast<UINT64>(hz) * width * height;
}

// A fixed EDID 1.4 base block: only the detailed timing and physical size change with the mode.
// Windows takes the real mode list from EvtIddCxParseMonitorDescription; the EDID identifies the
// monitor (name "ParkScreen", vendor "PKS") and must have a valid checksum.
void BuildEdid(const ParkScreenMode& mode, BYTE (&edid)[128])
{
    std::memset(edid, 0, sizeof(edid));
    const BYTE header[8] = { 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00 };
    std::memcpy(edid, header, 8);

    edid[8] = 0x41; edid[9] = 0x73;            // manufacturer "PKS"
    edid[10] = 0x01; edid[11] = 0x00;          // product code
    edid[12] = 0x01;                           // serial number: constant, so Windows remembers the position
    edid[16] = 1;  edid[17] = 36;              // week 1, year 2026
    edid[18] = 1;  edid[19] = 4;               // EDID 1.4
    edid[20] = 0xA2;                           // digital input, 8 bits per colour, HDMI-a

    const UINT32 wmm = std::clamp<UINT32>(static_cast<UINT32>(mode.Width * 25.4 / 96.0 + 0.5), 10, 4095);
    const UINT32 hmm = std::clamp<UINT32>(static_cast<UINT32>(mode.Height * 25.4 / 96.0 + 0.5), 10, 4095);
    edid[21] = static_cast<BYTE>(std::clamp<UINT32>(wmm / 10, 1, 255));  // size in cm
    edid[22] = static_cast<BYTE>(std::clamp<UINT32>(hmm / 10, 1, 255));
    edid[23] = 120;                            // gamma 2.2
    edid[24] = 0x06;                           // sRGB, preferred timing is the native mode

    const BYTE chroma[10] = { 0xEE, 0x91, 0xA3, 0x54, 0x4C, 0x99, 0x26, 0x0F, 0x50, 0x54 };  // sRGB
    std::memcpy(edid + 25, chroma, 10);
    for (int i = 0; i < 8; i++) { edid[38 + 2 * i] = 0x01; edid[39 + 2 * i] = 0x01; }  // no standard timings

    // Detailed timing descriptor (reduced blanking style).
    const UINT32 hblank = 160, hfront = 48, hsync = 32;
    const UINT32 vblank = 30, vfront = 3, vsync = 5;
    UINT64 clock = (static_cast<UINT64>(mode.Width) + hblank) * (mode.Height + vblank) * mode.RefreshHz / 10000;
    clock = std::min<UINT64>(clock, 65535);  // cosmetic for very large modes
    BYTE* d = edid + 54;
    d[0] = clock & 0xFF;               d[1] = static_cast<BYTE>(clock >> 8);
    d[2] = mode.Width & 0xFF;          d[3] = hblank & 0xFF;
    d[4] = static_cast<BYTE>(((mode.Width >> 8) << 4) | (hblank >> 8));
    d[5] = mode.Height & 0xFF;         d[6] = vblank & 0xFF;
    d[7] = static_cast<BYTE>(((mode.Height >> 8) << 4) | (vblank >> 8));
    d[8] = hfront & 0xFF;              d[9] = hsync & 0xFF;
    d[10] = static_cast<BYTE>(((vfront & 0xF) << 4) | (vsync & 0xF));
    d[11] = 0;
    d[12] = wmm & 0xFF;                d[13] = hmm & 0xFF;
    d[14] = static_cast<BYTE>(((wmm >> 8) << 4) | (hmm >> 8));
    d[17] = 0x1E;                      // digital separate sync, positive polarity

    // Monitor range limits.
    BYTE* r = edid + 72;
    r[3] = 0xFD; r[5] = 24; r[6] = 120; r[7] = 30; r[8] = 250; r[9] = 255; r[10] = 0x00; r[11] = 0x0A;
    std::memset(r + 12, 0x20, 6);

    // Monitor name.
    BYTE* n = edid + 90;
    n[3] = 0xFC;
    const char name[] = "ParkScreen\n  ";
    std::memcpy(n + 5, name, 13);

    // Serial number string.
    BYTE* s = edid + 108;
    s[3] = 0xFF;
    const char serial[] = "PKS0001\n     ";
    std::memcpy(s + 5, serial, 13);

    BYTE sum = 0;
    for (int i = 0; i < 127; i++) sum = static_cast<BYTE>(sum + edid[i]);
    edid[127] = static_cast<BYTE>(256 - sum);
}

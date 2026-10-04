<#
Installs (or removes) the ParkScreen virtual display driver. Run from an elevated PowerShell.

  install.ps1 -Package <folder with ParkScreenIdd.inf, .dll and .cat>
  install.ps1 -Remove

Development: the package must be signed with a certificate the PC trusts. For a self-signed test
certificate run `bcdedit /set testsigning on`, reboot, and import the certificate into
LocalMachine\Root and LocalMachine\TrustedPublisher.
#>
param(
    [string]$Package = (Join-Path $PSScriptRoot 'package'),
    [switch]$Remove
)
$ErrorActionPreference = 'Stop'

if (-not ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole('Administrators')) {
    throw 'Run this script from an elevated (administrator) PowerShell.'
}

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class ParkScreenSetup {
    [StructLayout(LayoutKind.Sequential)] struct SP_DEVINFO_DATA { public int cbSize; public Guid ClassGuid; public int DevInst; public IntPtr Reserved; }
    [DllImport("setupapi.dll", SetLastError=true)] static extern IntPtr SetupDiCreateDeviceInfoList(ref Guid g, IntPtr h);
    [DllImport("setupapi.dll", SetLastError=true, CharSet=CharSet.Unicode)] static extern bool SetupDiCreateDeviceInfoW(IntPtr s, string name, ref Guid g, string desc, IntPtr h, int flags, ref SP_DEVINFO_DATA d);
    [DllImport("setupapi.dll", SetLastError=true, CharSet=CharSet.Unicode)] static extern bool SetupDiSetDeviceRegistryPropertyW(IntPtr s, ref SP_DEVINFO_DATA d, int prop, byte[] buf, int size);
    [DllImport("setupapi.dll", SetLastError=true)] static extern bool SetupDiCallClassInstaller(int fn, IntPtr s, ref SP_DEVINFO_DATA d);
    [DllImport("setupapi.dll")] static extern bool SetupDiDestroyDeviceInfoList(IntPtr s);
    [DllImport("newdev.dll", SetLastError=true, CharSet=CharSet.Unicode)] static extern bool UpdateDriverForPlugAndPlayDevicesW(IntPtr h, string hwid, string inf, int flags, out bool reboot);

    // Creates the root-enumerated device node (like `devcon install`) and binds the INF to it.
    public static bool Install(string inf, string hwid) {
        Guid cls = new Guid("4D36E968-E325-11CE-BFC1-08002BE10318");
        IntPtr set = SetupDiCreateDeviceInfoList(ref cls, IntPtr.Zero);
        if (set == new IntPtr(-1)) throw new System.ComponentModel.Win32Exception();
        try {
            SP_DEVINFO_DATA d = new SP_DEVINFO_DATA(); d.cbSize = Marshal.SizeOf(d);
            if (!SetupDiCreateDeviceInfoW(set, "Display", ref cls, null, IntPtr.Zero, 1, ref d)) throw new System.ComponentModel.Win32Exception();
            byte[] id = System.Text.Encoding.Unicode.GetBytes(hwid + "\0\0");
            if (!SetupDiSetDeviceRegistryPropertyW(set, ref d, 1 /*SPDRP_HARDWAREID*/, id, id.Length)) throw new System.ComponentModel.Win32Exception();
            if (!SetupDiCallClassInstaller(0x19 /*DIF_REGISTERDEVICE*/, set, ref d)) throw new System.ComponentModel.Win32Exception();
        } finally { SetupDiDestroyDeviceInfoList(set); }
        bool reboot;
        if (!UpdateDriverForPlugAndPlayDevicesW(IntPtr.Zero, hwid, inf, 1 /*INSTALLFLAG_FORCE*/, out reboot)) throw new System.ComponentModel.Win32Exception();
        return reboot;
    }
}
'@

if ($Remove) {
    $dev = Get-PnpDevice -FriendlyName 'ParkScreen Virtual Display' -ErrorAction SilentlyContinue
    foreach ($d in $dev) { pnputil /remove-device $d.InstanceId | Out-Host }
    Get-WindowsDriver -Online | Where-Object OriginalFileName -like '*parkscreenidd.inf' |
        ForEach-Object { pnputil /delete-driver $_.Driver /uninstall /force | Out-Host }
    return
}

$inf = Join-Path (Resolve-Path $Package) 'ParkScreenIdd.inf'
if (-not (Test-Path $inf)) { throw "ParkScreenIdd.inf not found in $Package" }
pnputil /add-driver $inf | Out-Host
if ($LASTEXITCODE -ne 0) { throw 'pnputil could not add the driver (is the package signed and trusted?)' }
if (Get-PnpDevice -FriendlyName 'ParkScreen Virtual Display' -ErrorAction SilentlyContinue) {
    Write-Host 'The ParkScreen display device already exists; the driver package was updated.'
} else {
    $reboot = [ParkScreenSetup]::Install($inf, 'Root\ParkScreenIdd')
    if ($reboot) { Write-Host 'Restart Windows to finish installing the display driver.' }
}
Write-Host 'Done. Choose "Extend" in the ParkScreen tray menu.'

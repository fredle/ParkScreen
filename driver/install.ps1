<#
Installs (or removes) the ParkScreen virtual display driver. It is shipped in the driver zip and run
by the host app through an elevated PowerShell, or by hand from an administrator PowerShell.

  install.ps1 [-Package <folder>] [-ResultFile <path>]
  install.ps1 -Remove [-ResultFile <path>]

-Package     folder with ParkScreenIdd.inf, .dll, .cat and ParkScreenTest.cer (default: this folder)
-ResultFile  a one-line JSON result is written here:
               {"ok":true,"version":"0.1.5.0","reboot":false}
               {"ok":false,"code":3,"message":"..."}

Exit codes: 0 ok, 1 error, 2 not administrator, 3 Windows test-signing is off.

The driver in the rolling "driver" release is signed with a test certificate. Windows only loads it
when test-signing is on (`bcdedit /set testsigning on`, Secure Boot off, then restart). This script
never turns that on for you. It does trust the test certificate (LocalMachine\Root and
TrustedPublisher), which is how `pnputil` accepts the package.
#>
param(
    [string]$Package = $PSScriptRoot,
    [switch]$Remove,
    [string]$ResultFile
)
$ErrorActionPreference = 'Stop'

function Write-Result([hashtable]$r) {
    if ($ResultFile) {
        try { ($r | ConvertTo-Json -Compress) | Set-Content -LiteralPath $ResultFile -Encoding ASCII } catch { }
    }
}

function Fail([int]$code, [string]$message) {
    Write-Host $message
    Write-Result @{ ok = $false; code = $code; message = $message }
    exit $code
}

if (-not ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole('Administrators')) {
    Fail 2 'Run this script from an elevated (administrator) PowerShell.'
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

function Test-SigningOn {
    $out = (& bcdedit /enum '{current}' 2>&1) -join "`n"
    return ($out -match '(?im)^\s*testsigning\s+Yes')
}

try {
    if ($Remove) {
        $dev = Get-PnpDevice -FriendlyName 'ParkScreen Virtual Display' -ErrorAction SilentlyContinue
        foreach ($d in $dev) { pnputil /remove-device $d.InstanceId | Out-Host }
        Get-WindowsDriver -Online | Where-Object OriginalFileName -like '*parkscreenidd.inf' |
            ForEach-Object { pnputil /delete-driver $_.Driver /uninstall /force | Out-Host }
        Write-Host 'The ParkScreen display driver was removed.'
        Write-Result @{ ok = $true; removed = $true; reboot = $false }
        exit 0
    }

    $pkg = (Resolve-Path -LiteralPath $Package).Path
    $inf = Join-Path $pkg 'ParkScreenIdd.inf'
    if (-not (Test-Path -LiteralPath $inf)) { Fail 1 "ParkScreenIdd.inf not found in $pkg" }

    # Trust the test certificate (idempotent).
    $cerFile = Join-Path $pkg 'ParkScreenTest.cer'
    $testSigned = $false
    if (Test-Path -LiteralPath $cerFile) {
        $cer = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2 $cerFile
        $cat = Join-Path $pkg 'ParkScreenIdd.cat'
        if (Test-Path -LiteralPath $cat) {
            $sig = Get-AuthenticodeSignature -LiteralPath $cat
            if ($sig.SignerCertificate -and $sig.SignerCertificate.Thumbprint -eq $cer.Thumbprint) { $testSigned = $true }
        }
        if ($testSigned) {
            foreach ($store in 'Root', 'TrustedPublisher') {
                if (-not (Get-ChildItem "Cert:\LocalMachine\$store" | Where-Object Thumbprint -eq $cer.Thumbprint)) {
                    Import-Certificate -FilePath $cerFile -CertStoreLocation "Cert:\LocalMachine\$store" | Out-Null
                }
            }
        }
    }

    # A test-signed driver only loads with test-signing on. Never switch it on automatically: it
    # needs Secure Boot off and a restart, which is the user's call.
    if ($testSigned -and -not (Test-SigningOn)) {
        Fail 3 ('Windows test-signing is off, so Windows will not load this test-signed display driver. ' +
                'To try it: turn Secure Boot off in the PC firmware, run "bcdedit /set testsigning on" from an ' +
                'administrator command prompt, restart, then install the driver again.')
    }

    $existing = Get-PnpDevice -FriendlyName 'ParkScreen Virtual Display' -ErrorAction SilentlyContinue
    # With the device present, /install also rebinds it to the new package (a plain /add-driver
    # leaves it on the old one).
    if ($existing) { pnputil /add-driver $inf /install | Out-Host } else { pnputil /add-driver $inf | Out-Host }
    # 3010 = success, restart needed.
    if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne 3010) { Fail 1 "pnputil could not add the driver (exit code $LASTEXITCODE). Is the package signed and trusted?" }

    $reboot = $false
    if ($existing) {
        Write-Host 'The ParkScreen display device already exists; the driver was updated.'
        # Swapping the driver under a running device can leave it failed until it is restarted.
        Start-Sleep -Seconds 3
        $dev = Get-PnpDevice -FriendlyName 'ParkScreen Virtual Display' -ErrorAction SilentlyContinue
        if ($dev -and $dev.Status -ne 'OK') { pnputil /restart-device $dev.InstanceId | Out-Host }
        if ($LASTEXITCODE -eq 3010) { $reboot = $true }
    } else {
        $reboot = [ParkScreenSetup]::Install($inf, 'Root\ParkScreenIdd')
        if ($reboot) { Write-Host 'Restart Windows to finish installing the display driver.' }
    }

    $ver = ''
    $line = Select-String -LiteralPath $inf -Pattern '^\s*DriverVer\s*=\s*([^;]+)' | Select-Object -First 1
    if ($line) { $ver = ($line.Matches[0].Groups[1].Value.Trim() -split ',')[-1].Trim() }
    Write-Host 'Done. Choose "Extend" in the ParkScreen tray menu.'
    Write-Result @{ ok = $true; version = $ver; reboot = [bool]$reboot }
    exit 0
}
catch {
    Fail 1 $_.Exception.Message
}

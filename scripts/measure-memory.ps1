#Requires -Version 7.0
<#
.SYNOPSIS
    リリースビルドのブラウザで URL を開き、ピーク時のメモリ使用量を測る (Windows 専用)。

.DESCRIPTION
    主な指標は「ピーク時のプライベートメモリ」(このプロセスだけが確保したメモリ)。
    ワーキングセットは OS の DLL など他のプロセスと共有するページも含むので参考値として扱う。
    通常は `mise run mem -- <URL>` から呼び出す (先にリリースビルドが走る)。

.EXAMPLE
    mise run mem -- https://example.org/
#>
param(
    [Parameter(Mandatory)][string]$Url,
    [int]$Runs = 3
)

$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class MemoryInfo {
    [StructLayout(LayoutKind.Sequential)]
    public struct PROCESS_MEMORY_COUNTERS {
        public uint cb; public uint PageFaultCount;
        public UIntPtr PeakWorkingSetSize; public UIntPtr WorkingSetSize;
        public UIntPtr QuotaPeakPagedPoolUsage; public UIntPtr QuotaPagedPoolUsage;
        public UIntPtr QuotaPeakNonPagedPoolUsage; public UIntPtr QuotaNonPagedPoolUsage;
        public UIntPtr PagefileUsage; public UIntPtr PeakPagefileUsage;
    }
    [DllImport("psapi.dll", SetLastError = true)]
    public static extern bool GetProcessMemoryInfo(IntPtr process, out PROCESS_MEMORY_COUNTERS counters, uint size);
}
'@

$exe = Join-Path $PSScriptRoot '..\target\release\my-browser.exe' | Resolve-Path

function Measure-Once {
    $psi = [System.Diagnostics.ProcessStartInfo]::new($exe)
    $psi.ArgumentList.Add($Url)
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.UseShellExecute = $false
    $process = [System.Diagnostics.Process]::Start($psi)
    # 終了後もメモリのカウンタを読めるよう、先にハンドルを取得しておく
    $handle = $process.Handle
    $stderr = $process.StandardError.ReadToEndAsync()
    $null = $process.StandardOutput.ReadToEnd()
    $process.WaitForExit()

    $counters = New-Object MemoryInfo+PROCESS_MEMORY_COUNTERS
    $counters.cb = [Runtime.InteropServices.Marshal]::SizeOf($counters)
    if (-not [MemoryInfo]::GetProcessMemoryInfo($handle, [ref]$counters, $counters.cb)) {
        throw "GetProcessMemoryInfo に失敗しました"
    }
    if ($process.ExitCode -ne 0) {
        throw "ブラウザが終了コード $($process.ExitCode) で終了しました:`n$($stderr.Result)"
    }
    [pscustomobject]@{
        PeakPrivateKB    = [math]::Round($counters.PeakPagefileUsage.ToUInt64() / 1KB)
        PeakWorkingSetKB = [math]::Round($counters.PeakWorkingSetSize.ToUInt64() / 1KB)
    }
}

$results = 1..$Runs | ForEach-Object { Measure-Once }
$results | Format-Table -AutoSize
$min = $results | Measure-Object -Property PeakPrivateKB -Minimum
"{0}: ピーク時のプライベートメモリ {1:N0} KB (最小値 / {2} 回)" -f $Url, $min.Minimum, $Runs

# Samples memory and CPU use of one embdbg run.
#
#   powershell -NoProfile -File tools\measure.ps1 -AppArgs "--sim" -Label streaming
#
# The window is opened with -AppArgs, left alone for -SettleSeconds, then watched for
# -SampleSeconds and closed. CPU is the average share of one core during the sample window.
param(
    [string]$Exe = "target\release\embdbg.exe",
    [string]$AppArgs = "",
    [string]$Label = "run",
    [int]$SettleSeconds = 5,
    [int]$SampleSeconds = 3
)

$resolved = (Resolve-Path $Exe).Path
$p = if ($AppArgs) {
    Start-Process -FilePath $resolved -ArgumentList $AppArgs -PassThru
} else {
    Start-Process -FilePath $resolved -PassThru
}
try {
    # One Process object, refreshed before each reading: separate Get-Process calls hand
    # back cached snapshots, which makes every difference come out as zero.
    Start-Sleep -Seconds $SettleSeconds
    $p.Refresh()
    if ($p.HasExited) { throw "${Label}: the app exited early with code $($p.ExitCode)" }
    $cpu0 = $p.TotalProcessorTime.TotalSeconds
    $cores = [Environment]::ProcessorCount
    Start-Sleep -Seconds $SampleSeconds
    $p.Refresh()

    $cpu = ($p.TotalProcessorTime.TotalSeconds - $cpu0) / $SampleSeconds * 100
    "{0}: working set {1:N1} MB (peak {2:N1} MB), private {3:N1} MB, threads {4}, cpu {5:N2}% of one core" -f `
        $Label, ($p.WorkingSet64 / 1MB), ($p.PeakWorkingSet64 / 1MB), ($p.PrivateMemorySize64 / 1MB), $p.Threads.Count, $cpu
}
finally {
    if (!$p.HasExited) { Stop-Process -Id $p.Id -Force }
}

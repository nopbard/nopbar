# Average/max DWM 3D-engine GPU utilization over N seconds (1 s samples).
# Usage: measure-dwm-gpu.ps1 [-Seconds 30]
param([int]$Seconds = 30)
$dwm = (Get-Process dwm | Select-Object -First 1).Id
$path = "\GPU Engine(pid_" + $dwm + "_*engtype_3D)\Utilization Percentage"
$samples = Get-Counter -Counter $path -SampleInterval 1 -MaxSamples $Seconds
$vals = $samples | ForEach-Object { ($_.CounterSamples | Measure-Object -Property CookedValue -Sum).Sum }
$avg = ($vals | Measure-Object -Average).Average
$max = ($vals | Measure-Object -Maximum).Maximum
"dwm 3D util: avg {0:N2}%  max {1:N2}%  ({2} samples)" -f $avg, $max, $vals.Count

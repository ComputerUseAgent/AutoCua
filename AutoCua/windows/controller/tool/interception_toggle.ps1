# Licensed under the MIT License. See the LICENSE file in the project root
# for the full license text.

<#
    Binds the Interception input driver to the BUILT-IN keyboard/touchpad only.

    Interception used to be installed as an UpperFilter on the whole keyboard
    CLASS, so every keyboard passed through it. It has 10 keyboard slots that are
    never freed, so each reconnect of a wireless keyboard burned one; once they ran
    out a reconnecting keyboard enumerated fine but delivered no input until
    reboot. That repeatedly killed a user's keyboard - see
    INTERCEPTION_DRIVER.md.

    Binding to the built-in devices instead fixes it for good: they are
    non-removable, so they take exactly one slot each at boot and never another,
    and no external keyboard is ever filtered. AutoCua injects through that
    binding.

    The driver only binds its slots when it loads AT BOOT - attaching it
    mid-session binds nothing and leaves the physical keyboard dead. So this is a
    one-time setup step that takes effect on the next reboot, never a runtime toggle.

    Run elevated.  -Action bind | unbind | status
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('bind', 'unbind', 'status')]
    [string]$Action
)

$ErrorActionPreference = 'Continue'

$KB_CLASS = 'HKLM:\SYSTEM\CurrentControlSet\Control\Class\{4d36e96b-e325-11ce-bfc1-08002be10318}'
$MS_CLASS = 'HKLM:\SYSTEM\CurrentControlSet\Control\Class\{4d36e96f-e325-11ce-bfc1-08002be10318}'

# Built-in devices never reconnect, so they can never exhaust the driver's slots.
function Get-BuiltInInputDevices {
    $out = @()
    foreach ($spec in @(@{Class = 'Keyboard'; Filter = 'keyboard'}, @{Class = 'Mouse'; Filter = 'mouse'})) {
        $best = $null
        foreach ($d in (Get-PnpDevice -PresentOnly -Class $spec.Class -ErrorAction SilentlyContinue)) {
            $rp = (Get-PnpDeviceProperty -InstanceId $d.InstanceId -KeyName 'DEVPKEY_Device_RemovalPolicy' -ErrorAction SilentlyContinue).Data
            if ($rp -ne 1) { continue }                        # 1 = not removable
            if ($d.InstanceId -like '*VID_*') { continue }     # skip anything on a pluggable bus
            # Prefer the firmware-enumerated device (ACPI/PS-2) - the most stable.
            if ((-not $best) -or ($d.InstanceId -like 'ACPI\*')) { $best = $d }
        }
        if ($best) {
            $out += [pscustomobject]@{
                Class = $spec.Class; Filter = $spec.Filter
                InstanceId = $best.InstanceId; Name = $best.FriendlyName
                Key = "HKLM:\SYSTEM\CurrentControlSet\Enum\$($best.InstanceId)"
            }
        }
    }
    return $out
}

function Get-Filters($key) {
    $v = (Get-ItemProperty -Path $key -Name UpperFilters -ErrorAction SilentlyContinue).UpperFilters
    if ($null -eq $v) { return @() }
    return @($v)
}

function Set-Filters($key, [string[]]$values) {
    if ($values.Count -eq 0) { Remove-ItemProperty -Path $key -Name UpperFilters -ErrorAction SilentlyContinue }
    else { Set-ItemProperty -Path $key -Name UpperFilters -Value ([string[]]$values) -Type MultiString }
}

function Clear-ClassFilters($details) {
    foreach ($c in @(@{k = $KB_CLASS; n = 'keyboard'}, @{k = $MS_CLASS; n = 'mouse'})) {
        $cur = Get-Filters $c.k
        if ($cur -contains $c.n) {
            Set-Filters $c.k @($cur | Where-Object { $_ -ne $c.n })
            $details.Add("removed class-wide '$($c.n)' filter - a class-wide filter is what starved external keyboards") | Out-Null
        }
    }
}

$details = New-Object System.Collections.ArrayList
$result = [ordered]@{ action = $Action; ok = $false; boundDevices = @(); classFiltersClear = $false; details = @() }
$devices = Get-BuiltInInputDevices

if ($devices.Count -eq 0 -and $Action -ne 'status') {
    $details.Add("ERROR: no non-removable built-in input device found to bind to") | Out-Null
}

switch ($Action) {

    'bind' {
        foreach ($d in $devices) {
            $cur = Get-Filters $d.Key
            if ($cur -notcontains $d.Filter) {
                Set-Filters $d.Key (@($d.Filter) + $cur)
                $details.Add("$($d.Class): bound '$($d.Filter)' to built-in device '$($d.Name)'") | Out-Null
            } else {
                $details.Add("$($d.Class): already bound to '$($d.Name)'") | Out-Null
            }
        }
        Clear-ClassFilters $details
        $details.Add("takes effect on next reboot - the driver only binds its slots when it loads at boot") | Out-Null
    }

    'unbind' {
        foreach ($d in $devices) {
            $cur = Get-Filters $d.Key
            if ($cur -contains $d.Filter) {
                Set-Filters $d.Key @($cur | Where-Object { $_ -ne $d.Filter })
                $details.Add("$($d.Class): unbound from '$($d.Name)'") | Out-Null
            }
        }
        Clear-ClassFilters $details
    }

    'status' { }
}

$boundCount = 0
foreach ($d in $devices) {
    $bound = (Get-Filters $d.Key) -contains $d.Filter
    if ($bound) { $boundCount++ }
    $stack = (Get-PnpDeviceProperty -InstanceId $d.InstanceId -KeyName DEVPKEY_Device_Stack -ErrorAction SilentlyContinue).Data
    $live = [bool]($stack -contains "\Driver\$($d.Filter)")
    $result.boundDevices += "$($d.Class) '$($d.Name)': registered=$bound liveNow=$live"
}
$result.classFiltersClear = (-not ((Get-Filters $KB_CLASS) -contains 'keyboard')) -and (-not ((Get-Filters $MS_CLASS) -contains 'mouse'))
$details.Add("keyboard class UpperFilters: $((Get-Filters $KB_CLASS) -join ', ')") | Out-Null
$details.Add("mouse class UpperFilters: $((Get-Filters $MS_CLASS) -join ', ')") | Out-Null
$result.details = @($details)

switch ($Action) {
    'bind'   { $result.ok = $result.classFiltersClear -and ($boundCount -gt 0) }
    'unbind' { $result.ok = ($boundCount -eq 0) -and $result.classFiltersClear }
    'status' { $result.ok = $true }
}

$result | ConvertTo-Json -Compress -Depth 3
if (-not $result.ok) { exit 1 }
exit 0

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$PluginsRoot,
    [string]$PythonExecutable = $env:CYRENE_PYTHON
)

$ErrorActionPreference = 'Stop'
$platformRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$dotnetProject = Join-Path $PSScriptRoot 'dotnet\CapabilityExecutionServiceTck.csproj'
$serviceExecutable = Join-Path $platformRoot 'target\debug\cyrene-capability-execution-service.exe'
$pluginsRoot = (Resolve-Path $PluginsRoot).Path
$expectedPluginsSha = 'c8e1be76fabe6184247e16dbda411a6cb9a917dd'

if (-not $PythonExecutable) {
    $PythonExecutable = if ($IsWindows) { 'python' } else { 'python3' }
}

function Assert-PluginsSnapshot {
    $head = (& git -C $pluginsRoot rev-parse HEAD).Trim()
    if ($head -ne $expectedPluginsSha) {
        throw "PluginsRoot must be the immutable c8e1be76fabe6184247e16dbda411a6cb9a917dd snapshot; found $head"
    }
    if ((& git -C $pluginsRoot status --porcelain)) {
        throw "PluginsRoot is dirty; the E2E must not consume mutable plugin changes"
    }
    & git -C $pluginsRoot cat-file -e "$expectedPluginsSha`^{commit}"
}

function Get-FreeLoopbackPort {
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    try {
        $listener.Start()
        return $listener.LocalEndpoint.Port
    }
    finally {
        $listener.Stop()
    }
}

function Wait-LoopbackPort([int]$port) {
    for ($attempt = 0; $attempt -lt 100; $attempt++) {
        $tcpClient = [System.Net.Sockets.TcpClient]::new()
        try {
            $connect = $tcpClient.ConnectAsync('127.0.0.1', $port)
            if ($connect.Wait(100) -and $tcpClient.Connected) {
                return
            }
        }
        finally {
            $tcpClient.Dispose()
        }
        Start-Sleep -Milliseconds 100
    }
    throw "CapabilityExecutionService did not listen on 127.0.0.1:$port"
}

function Start-CapabilityService(
    [string]$manifest,
    [string]$workingDirectory,
    [int]$eventBufferCapacity,
    [int]$defaultInvokeTimeoutMilliseconds = 30000
) {
    $port = Get-FreeLoopbackPort
    $runId = [Guid]::NewGuid().ToString('N')
    $stdout = Join-Path ([System.IO.Path]::GetTempPath()) "cyrene-capability-service-$runId.out.log"
    $stderr = Join-Path ([System.IO.Path]::GetTempPath()) "cyrene-capability-service-$runId.err.log"
    $arguments = @(
        '--bind', "127.0.0.1:$port",
        '--manifest', $manifest,
        '--working-dir', $workingDirectory,
        '--python-path', (Join-Path $platformRoot 'sdk\python'),
        '--python-executable', $PythonExecutable,
        '--event-buffer-capacity', $eventBufferCapacity,
        '--default-invoke-timeout-ms', $defaultInvokeTimeoutMilliseconds
    )
    $process = Start-Process -FilePath $serviceExecutable `
        -ArgumentList $arguments `
        -WorkingDirectory $platformRoot `
        -WindowStyle Hidden `
        -RedirectStandardOutput $stdout `
        -RedirectStandardError $stderr `
        -PassThru
    try {
        Wait-LoopbackPort $port
    }
    catch {
        if (-not $process.HasExited) {
            Stop-Process -Id $process.Id -ErrorAction SilentlyContinue
        }
        throw "$_`n$([System.IO.File]::ReadAllText($stderr))"
    }
    return [pscustomobject]@{
        Port = $port
        Process = $process
        StandardError = $stderr
    }
}

function Stop-CapabilityService($service) {
    if ($null -eq $service) {
        return
    }
    $process = Get-Process -Id $service.Process.Id -ErrorAction SilentlyContinue
    if ($null -ne $process) {
        Stop-Process -Id $process.Id
        $process.WaitForExit(5000) | Out-Null
    }
    if (Get-Process -Id $service.Process.Id -ErrorAction SilentlyContinue) {
        throw "orphan capability execution service process $($service.Process.Id)"
    }
}

function Invoke-GeneratedDotnetClient([string[]]$arguments) {
    $output = @(& dotnet run --no-build --no-restore --project $dotnetProject -- $arguments 2>&1)
    return [pscustomobject]@{
        ExitCode = $LASTEXITCODE
        Output = $output
    }
}

function Assert-OutputContains($result, [string]$needle) {
    if (-not (($result.Output | Out-String) -like "*$needle*")) {
        throw "expected generated client output to contain '$needle'; output was: $($result.Output -join [Environment]::NewLine)"
    }
}

Assert-PluginsSnapshot
if (-not (Test-Path -LiteralPath $serviceExecutable)) {
    throw "service binary not found at $serviceExecutable; run cargo build -p cy-capability-execution-service --bin cyrene-capability-execution-service"
}

$mediaManifest = Join-Path $pluginsRoot 'plugins\tools\media\plugin.manifest.json'
$mediaWorkingDirectory = Join-Path $pluginsRoot 'plugins\tools\media'
$mediaFixtures = Join-Path $PSScriptRoot 'fixtures'
$genericManifest = Join-Path $mediaFixtures 'generic-worker.manifest.json'
$genericWorkingDirectory = Join-Path $platformRoot 'framework\crates\cy-platform-api\tests'

$mediaService = $null
try {
    $mediaService = Start-CapabilityService $mediaManifest $mediaWorkingDirectory 32
    $endpoint = "http://127.0.0.1:$($mediaService.Port)"

    $inspect = Invoke-GeneratedDotnetClient @(
        'invoke', $endpoint, 'media.processor.v1', '1', 'inspect_image',
        (Join-Path $mediaFixtures 'media-inspect.json'), '10000'
    )
    if ($inspect.ExitCode -ne 0) { throw "generated .NET media inspect failed: $($inspect.Output -join ' ')" }
    $inspection = ($inspect.Output | Select-Object -Last 1) | ConvertFrom-Json
    if ($inspection.format -ne 'png' -or $inspection.width -ne 1 -or $inspection.height -ne 1) {
        throw "unexpected media inspect result: $($inspection | ConvertTo-Json -Compress)"
    }

    $transform = Invoke-GeneratedDotnetClient @(
        'invoke', $endpoint, 'media.processor.v1', '1', 'transform_image',
        (Join-Path $mediaFixtures 'media-transform.json'), '10000'
    )
    if ($transform.ExitCode -ne 0) { throw "generated .NET media transform failed: $($transform.Output -join ' ')" }
    $transformed = ($transform.Output | Select-Object -Last 1) | ConvertFrom-Json
    if ($transformed.format -ne 'jpeg' -or $transformed.width -ne 2 -or $transformed.height -ne 2) {
        throw "unexpected media transform result: $($transformed | ConvertTo-Json -Compress)"
    }

    $invalid = Invoke-GeneratedDotnetClient @(
        'invoke', $endpoint, 'media.processor.v1', '1', 'inspect_image',
        (Join-Path $mediaFixtures 'media-invalid.json'), '10000'
    )
    if ($invalid.ExitCode -ne 5) { throw "invalid media request did not return generic execution error: $($invalid.Output -join ' ')" }
    Assert-OutputContains $invalid 'execution:InvalidRequest:'

    $cancelled = Invoke-GeneratedDotnetClient @(
        'invoke', $endpoint, 'media.processor.v1', '1', 'transform_image',
        (Join-Path $mediaFixtures 'media-timeout-transform.json'), '10000', '10'
    )
    if ($cancelled.ExitCode -eq 0) { throw 'Product CancellationToken unexpectedly completed media invocation' }
    Assert-OutputContains $cancelled 'grpc:'
}
finally {
    Stop-CapabilityService $mediaService
}

$timeoutService = $null
try {
    $timeoutService = Start-CapabilityService $mediaManifest $mediaWorkingDirectory 32 1
    $timeout = Invoke-GeneratedDotnetClient @(
        'invoke', "http://127.0.0.1:$($timeoutService.Port)", 'media.processor.v1', '1', 'transform_image',
        (Join-Path $mediaFixtures 'media-timeout-transform.json')
    )
    if ($timeout.ExitCode -ne 5) { throw "media worker timeout did not return generic execution error: $($timeout.Output -join ' ')" }
    Assert-OutputContains $timeout 'execution:Timeout:'
}
finally {
    Stop-CapabilityService $timeoutService
}

$eventService = $null
try {
    $eventService = Start-CapabilityService $genericManifest $genericWorkingDirectory 32
    $events = Invoke-GeneratedDotnetClient @(
        'events', "http://127.0.0.1:$($eventService.Port)", 'test.application-events.v1', '1',
        (Join-Path $mediaFixtures 'generic-event-ordered.json'), '10000'
    )
    if ($events.ExitCode -ne 0) { throw "generated .NET generic event client failed: $($events.Output -join ' ')" }
    $eventLines = @($events.Output | Where-Object { $_ -like 'EVENT|*' })
    if ($eventLines.Count -ne 3) { throw "expected three ordered events; received $($eventLines -join ' ')" }
    $eventFields = @($eventLines | ForEach-Object { ,($_ -split '\|') })
    $subscription = $eventFields[0][1]
    if ([string]::IsNullOrWhiteSpace($subscription) -or ($eventFields | Where-Object { $_[1] -ne $subscription })) {
        throw 'generated event client observed inconsistent subscription identity'
    }
    $sequences = @($eventFields | ForEach-Object { [int]$_.Item(2) })
    if ($sequences.Count -ne 3 -or $sequences[0] -ne 1 -or $sequences[1] -ne 2 -or $sequences[2] -ne 3) {
        throw "event sequence was not ordered: $($eventLines -join ' ')"
    }
    Assert-OutputContains $events 'END|'
    Assert-OutputContains $events '|NormalCompletion|'
}
finally {
    Stop-CapabilityService $eventService
}

$backpressureService = $null
try {
    $backpressureService = Start-CapabilityService $genericManifest $genericWorkingDirectory 2
    $backpressure = Invoke-GeneratedDotnetClient @(
        'events', "http://127.0.0.1:$($backpressureService.Port)", 'test.application-events.v1', '1',
        (Join-Path $mediaFixtures 'generic-event-burst.json'), '10000'
    )
    if ($backpressure.ExitCode -ne 7) { throw "bounded event stream did not terminate with backpressure: $($backpressure.Output -join ' ')" }
    Assert-OutputContains $backpressure '|Backpressure|'
}
finally {
    Stop-CapabilityService $backpressureService
}

$crashService = $null
try {
    $crashService = Start-CapabilityService $genericManifest $genericWorkingDirectory 32
    $crash = Invoke-GeneratedDotnetClient @(
        'invoke', "http://127.0.0.1:$($crashService.Port)", 'test.capability.v1', '1', 'crash', '-', '10000'
    )
    if ($crash.ExitCode -ne 5) { throw "generic worker crash did not return generic execution error: $($crash.Output -join ' ')" }
    Assert-OutputContains $crash 'execution:WorkerCrashed:'
}
finally {
    Stop-CapabilityService $crashService
}

Write-Output 'Capability Execution Service generated .NET media and generic event E2E: PASS'

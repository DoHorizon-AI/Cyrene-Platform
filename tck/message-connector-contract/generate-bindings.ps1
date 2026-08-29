param(
    [string]$ProtocPath,
    [string]$WellKnownTypesPath
)

$ErrorActionPreference = 'Stop'
$repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
$protoRoot = Join-Path $repositoryRoot 'contracts\proto'
$connectorProto = Join-Path $protoRoot 'cyrene\message\connector\v1\message_connector.proto'

if ([string]::IsNullOrWhiteSpace($ProtocPath)) {
    $assetsPath = Join-Path $PSScriptRoot 'dotnet\obj\project.assets.json'
    if (-not (Test-Path -LiteralPath $assetsPath -PathType Leaf)) {
        throw "The .NET TCK assets file is missing; run dotnet restore before generating bindings."
    }
    $assets = Get-Content -LiteralPath $assetsPath -Raw | ConvertFrom-Json
    $packageRoots = @($assets.packageFolders.PSObject.Properties.Name)
    if ($packageRoots.Count -eq 0) {
        throw "The .NET TCK assets file declares no NuGet package folder."
    }
    $grpcToolsRoot = Join-Path $packageRoots[0] 'grpc.tools\2.67.0'
    $ProtocPath = Join-Path $grpcToolsRoot 'tools\windows_x64\protoc.exe'
    $WellKnownTypesPath = Join-Path $grpcToolsRoot 'build\native\include'
}

if (-not (Test-Path -LiteralPath $ProtocPath -PathType Leaf)) {
    throw "protoc was not found at '$ProtocPath'; restore the .NET TCK project or pass -ProtocPath."
}

if ([string]::IsNullOrWhiteSpace($WellKnownTypesPath)) {
    $protocDirectory = Split-Path -Parent ([System.IO.Path]::GetFullPath($ProtocPath))
    $WellKnownTypesPath = [System.IO.Path]::GetFullPath(
        (Join-Path $protocDirectory '..\..\build\native\include'))
}
if (-not (Test-Path -LiteralPath $WellKnownTypesPath -PathType Container)) {
    throw "The protobuf well-known-type include path was not found at '$WellKnownTypesPath'; pass -WellKnownTypesPath."
}

$tempRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
$generationRoot = Join-Path $tempRoot ("cyrene-message-connector-" + [Guid]::NewGuid().ToString('N'))
$outputs = @{
    CSharp = Join-Path $generationRoot 'csharp'
    Java = Join-Path $generationRoot 'java'
    Kotlin = Join-Path $generationRoot 'kotlin'
    Python = Join-Path $generationRoot 'python'
}

try {
    foreach ($output in $outputs.Values) {
        New-Item -ItemType Directory -Path $output | Out-Null
    }

    $arguments = @(
        "--proto_path=$protoRoot"
        "--proto_path=$WellKnownTypesPath"
        "--csharp_out=$($outputs.CSharp)"
        "--java_out=$($outputs.Java)"
        "--kotlin_out=$($outputs.Kotlin)"
        "--python_out=$($outputs.Python)"
        $connectorProto
    )
    & $ProtocPath @arguments
    if ($LASTEXITCODE -ne 0) {
        throw "protoc generation failed with exit code $LASTEXITCODE."
    }

    $checks = @(
        @{ Name = 'C#'; Root = $outputs.CSharp; Filter = '*.cs' }
        @{ Name = 'Java'; Root = $outputs.Java; Filter = '*.java' }
        @{ Name = 'Kotlin'; Root = $outputs.Kotlin; Filter = '*.kt' }
        @{ Name = 'Python'; Root = $outputs.Python; Filter = '*.py' }
    )
    foreach ($check in $checks) {
        $count = @(Get-ChildItem -LiteralPath $check.Root -Recurse -File -Filter $check.Filter).Count
        if ($count -eq 0) {
            throw "$($check.Name) generation produced no $($check.Filter) files."
        }
        Write-Output "$($check.Name) generated files: $count"
    }

    Write-Output 'message.connector.v1 C#/Java/Kotlin/Python generation: PASS'
}
finally {
    $resolvedGenerationRoot = [System.IO.Path]::GetFullPath($generationRoot)
    $leaf = Split-Path -Leaf $resolvedGenerationRoot
    $insideTempRoot = $resolvedGenerationRoot.StartsWith(
        $tempRoot,
        [StringComparison]::OrdinalIgnoreCase)
    $hasExpectedPrefix = $leaf.StartsWith(
        'cyrene-message-connector-',
        [StringComparison]::Ordinal)
    if ($insideTempRoot -and $hasExpectedPrefix) {
        if (Test-Path -LiteralPath $resolvedGenerationRoot) {
            Remove-Item -LiteralPath $resolvedGenerationRoot -Recurse -Force
        }
    }
    else {
        throw "Refusing to clean unverified generation path '$resolvedGenerationRoot'."
    }
}

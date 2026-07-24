param(
    [string]$MongoUri = 'mongodb://127.0.0.1:27017/?retryWrites=false',
    [ValidatePattern('^[A-Za-z0-9_-]{1,64}$')]
    [string]$Database = 'faultkeep',
    [ValidateRange(1, 10000)]
    [int]$Sample = 1000,
    [ValidateRange(1, 1000000)]
    [int]$AcceptedRps = 1158,
    [ValidateRange(1, 3650)]
    [int]$RetentionDays = 30,
    [ValidateRange(1, 9)]
    [int]$ReplicationFactor = 1,
    [string]$Output = 'capacity/reports/phase22-local.json'
)

$ErrorActionPreference = 'Stop'
$outputPath = [System.IO.Path]::GetFullPath((Join-Path (Get-Location) $Output))
$workspace = [System.IO.Path]::GetFullPath((Get-Location).Path)
if (-not $outputPath.StartsWith($workspace, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw 'capacity output must remain inside the workspace'
}
$parent = Split-Path -Parent $outputPath
if (-not (Test-Path -LiteralPath $parent)) {
    New-Item -ItemType Directory -Path $parent | Out-Null
}

$previous = @{
    Database = $env:FAULTKEEP_CAPACITY_DATABASE
    Sample = $env:FAULTKEEP_CAPACITY_SAMPLE
    Rps = $env:FAULTKEEP_CAPACITY_RPS
    Retention = $env:FAULTKEEP_CAPACITY_RETENTION_DAYS
    Replication = $env:FAULTKEEP_CAPACITY_REPLICATION
    Commit = $env:FAULTKEEP_CAPACITY_COMMIT
    Rust = $env:FAULTKEEP_CAPACITY_RUST
    Hardware = $env:FAULTKEEP_CAPACITY_HARDWARE
}
$cpu = (Get-CimInstance Win32_Processor | Select-Object -First 1 -ExpandProperty Name).Trim()
$ram = [Math]::Round((Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory / 1GB, 1)
$env:FAULTKEEP_CAPACITY_DATABASE = $Database
$env:FAULTKEEP_CAPACITY_SAMPLE = [string]$Sample
$env:FAULTKEEP_CAPACITY_RPS = [string]$AcceptedRps
$env:FAULTKEEP_CAPACITY_RETENTION_DAYS = [string]$RetentionDays
$env:FAULTKEEP_CAPACITY_REPLICATION = [string]$ReplicationFactor
$env:FAULTKEEP_CAPACITY_COMMIT = (git rev-parse HEAD).Trim()
$env:FAULTKEEP_CAPACITY_RUST = (rustc --version).Trim()
$env:FAULTKEEP_CAPACITY_HARDWARE = "$cpu; $ram GiB RAM; Windows"

try {
    $json = & mongosh $MongoUri --quiet --file scripts/capacity-report.js
    if ($LASTEXITCODE -ne 0) { throw 'capacity report query failed' }
    $parsed = $json | ConvertFrom-Json
    if ($parsed.schema_version -ne 1) { throw 'capacity report schema is invalid' }
    [System.IO.File]::WriteAllText($outputPath, (($parsed | ConvertTo-Json -Depth 8) + "`n"))
    Write-Output $Output
}
finally {
    $env:FAULTKEEP_CAPACITY_DATABASE = $previous.Database
    $env:FAULTKEEP_CAPACITY_SAMPLE = $previous.Sample
    $env:FAULTKEEP_CAPACITY_RPS = $previous.Rps
    $env:FAULTKEEP_CAPACITY_RETENTION_DAYS = $previous.Retention
    $env:FAULTKEEP_CAPACITY_REPLICATION = $previous.Replication
    $env:FAULTKEEP_CAPACITY_COMMIT = $previous.Commit
    $env:FAULTKEEP_CAPACITY_RUST = $previous.Rust
    $env:FAULTKEEP_CAPACITY_HARDWARE = $previous.Hardware
}

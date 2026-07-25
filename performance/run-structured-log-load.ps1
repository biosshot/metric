param(
    [ValidateRange(1, 20000)]
    [int]$LogRps = 1000,
    [ValidateRange(1, 5000)]
    [int]$ErrorRps = 250,
    [ValidatePattern('^\d+[smh]$')]
    [string]$Duration = '10s',
    [string]$MongoUri = 'mongodb://127.0.0.1:27017/?retryWrites=false',
    [ValidatePattern('^faultkeep_phase24_[a-z0-9_]+$')]
    [string]$Database = "faultkeep_phase24_$([DateTimeOffset]::UtcNow.ToUnixTimeSeconds())",
    [string]$Result = "performance/results/phase24-logs-$LogRps-errors-$ErrorRps.json",
    [switch]$KeepDatabase
)

$ErrorActionPreference = 'Stop'
$server = $null
$stdout = $null
$stderr = $null
$previous = @{
    Uri = $env:FAULTKEEP_BENCH_MONGODB_URI
    Database = $env:FAULTKEEP_BENCH_DATABASE
    Address = $env:FAULTKEEP_BENCH_ADDRESS
    Target = $env:FAULTKEEP_TARGET
    LogRps = $env:FAULTKEEP_LOG_RPS
    ErrorRps = $env:FAULTKEEP_ERROR_RPS
    Duration = $env:FAULTKEEP_DURATION
    Result = $env:FAULTKEEP_RESULT
    RunId = $env:FAULTKEEP_RUN_ID
    Commit = $env:FAULTKEEP_COMMIT
    Rust = $env:FAULTKEEP_RUST
    K6 = $env:FAULTKEEP_K6
    Hardware = $env:FAULTKEEP_HARDWARE
    Mongo = $env:FAULTKEEP_MONGO
}

try {
    cargo build --locked --release --bin durable-ingest-bench
    if ($LASTEXITCODE -ne 0) { throw 'structured Log benchmark build failed' }
    & mongosh $MongoUri --quiet --eval "db.getSiblingDB('$Database').dropDatabase()" | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'fresh Phase 24 database preparation failed' }

    $env:FAULTKEEP_BENCH_MONGODB_URI = $MongoUri
    $env:FAULTKEEP_BENCH_DATABASE = $Database
    $env:FAULTKEEP_BENCH_ADDRESS = '127.0.0.1:3124'
    $stdout = Join-Path $env:TEMP "faultkeep-phase24-server-$PID.out"
    $stderr = Join-Path $env:TEMP "faultkeep-phase24-server-$PID.err"
    $server = Start-Process -FilePath 'target/release/durable-ingest-bench.exe' `
        -PassThru -WindowStyle Hidden -RedirectStandardOutput $stdout `
        -RedirectStandardError $stderr

    $ready = $false
    for ($attempt = 0; $attempt -lt 60; $attempt++) {
        try {
            $probe = Invoke-WebRequest -UseBasicParsing 'http://127.0.0.1:3124/ready' -TimeoutSec 1
            if ($probe.StatusCode -eq 200) { $ready = $true; break }
        } catch {}
        Start-Sleep -Milliseconds 500
    }
    if (-not $ready) {
        throw "Phase 24 benchmark server did not become ready: $(Get-Content $stderr -Raw)"
    }

    $commit = (git rev-parse HEAD).Trim()
    git diff --quiet
    if ($LASTEXITCODE -ne 0) { $commit = "$commit-dirty" }
    $rust = (rustc --version).Trim()
    $k6Version = (& k6 version | Select-Object -First 1).Trim()
    $cpu = (Get-CimInstance Win32_Processor | Select-Object -First 1 -ExpandProperty Name).Trim()
    $ram = [Math]::Round((Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory / 1GB, 1)
    $runIdText = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds().ToString()
    $runId = $runIdText.Substring($runIdText.Length - 8, 8)
    $env:FAULTKEEP_LOG_RPS = [string]$LogRps
    $env:FAULTKEEP_ERROR_RPS = [string]$ErrorRps
    $env:FAULTKEEP_TARGET = 'http://127.0.0.1:3124'
    $env:FAULTKEEP_DURATION = $Duration
    $env:FAULTKEEP_RESULT = $Result
    $env:FAULTKEEP_RUN_ID = $runId
    $env:FAULTKEEP_COMMIT = $commit
    $env:FAULTKEEP_RUST = $rust
    $env:FAULTKEEP_K6 = $k6Version
    $env:FAULTKEEP_HARDWARE = "$cpu; $ram GiB RAM; Windows"
    $env:FAULTKEEP_MONGO = 'MongoDB local standalone; direct connection'

    & k6 run performance/k6/structured-logs.js
    $k6Exit = $LASTEXITCODE
    if (-not (Test-Path -LiteralPath $Result)) {
        throw 'k6 did not write the Phase 24 result artifact'
    }
    $artifact = Get-Content -LiteralPath $Result -Raw | ConvertFrom-Json
    $acceptedLogs = [uint64]$artifact.metrics.log_status_200
    $acceptedErrors = [uint64]$artifact.metrics.error_status_200
    $logCountText = & mongosh $MongoUri --quiet --eval `
        "db.getSiblingDB('$Database').logs.countDocuments({})"
    if ($LASTEXITCODE -ne 0) { throw 'durable Log count verification failed' }
    $errorCountText = & mongosh $MongoUri --quiet --eval `
        "db.getSiblingDB('$Database').error_events.countDocuments({})"
    if ($LASTEXITCODE -ne 0) { throw 'durable Error count verification failed' }
    $durableLogs = [uint64]($logCountText | Select-Object -Last 1)
    $durableErrors = [uint64]($errorCountText | Select-Object -Last 1)
    if ($durableLogs -ne $acceptedLogs) {
        throw "acknowledged/durable Log mismatch: status_200=$acceptedLogs durable=$durableLogs"
    }
    if ($durableErrors -ne $acceptedErrors) {
        throw "acknowledged/durable Error mismatch: status_200=$acceptedErrors durable=$durableErrors"
    }
    Write-Output "verified durable Logs: $durableLogs; Errors: $durableErrors; acknowledged loss: 0"
    if ($k6Exit -ne 0) {
        throw "Phase 24 k6 thresholds failed with exit code $k6Exit; artifact retained at $Result"
    }
}
finally {
    if ($null -ne $server -and -not $server.HasExited) {
        Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
        $server.WaitForExit(10000) | Out-Null
    }
    if (-not $KeepDatabase -and $Database -match '^faultkeep_phase24_[a-z0-9_]+$') {
        & mongosh $MongoUri --quiet --eval "db.getSiblingDB('$Database').dropDatabase()" | Out-Null
    }
    foreach ($path in @($stdout, $stderr)) {
        if ($path -and (Test-Path -LiteralPath $path)) {
            Remove-Item -LiteralPath $path -Force
        }
    }
    $env:FAULTKEEP_BENCH_MONGODB_URI = $previous.Uri
    $env:FAULTKEEP_BENCH_DATABASE = $previous.Database
    $env:FAULTKEEP_BENCH_ADDRESS = $previous.Address
    $env:FAULTKEEP_TARGET = $previous.Target
    $env:FAULTKEEP_LOG_RPS = $previous.LogRps
    $env:FAULTKEEP_ERROR_RPS = $previous.ErrorRps
    $env:FAULTKEEP_DURATION = $previous.Duration
    $env:FAULTKEEP_RESULT = $previous.Result
    $env:FAULTKEEP_RUN_ID = $previous.RunId
    $env:FAULTKEEP_COMMIT = $previous.Commit
    $env:FAULTKEEP_RUST = $previous.Rust
    $env:FAULTKEEP_K6 = $previous.K6
    $env:FAULTKEEP_HARDWARE = $previous.Hardware
    $env:FAULTKEEP_MONGO = $previous.Mongo
}

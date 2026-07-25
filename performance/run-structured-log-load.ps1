param(
    [ValidateRange(1, 20000)]
    [int]$LogRps = 1000,
    [ValidateRange(1, 5000)]
    [int]$ErrorRps = 250,
    [ValidatePattern('^\d+[smh]$')]
    [string]$Duration = '10s',
    [string]$MongoUri = 'mongodb://127.0.0.1:27017/?retryWrites=false',
    [ValidatePattern('^metric_phase24_[a-z0-9_]+$')]
    [string]$Database = "metric_phase24_$([DateTimeOffset]::UtcNow.ToUnixTimeSeconds())",
    [string]$Result = "performance/results/phase24-logs-$LogRps-errors-$ErrorRps.json",
    [switch]$KeepDatabase
)

$ErrorActionPreference = 'Stop'
$server = $null
$stdout = $null
$stderr = $null
$previous = @{
    Uri = $env:METRIC_BENCH_MONGODB_URI
    Database = $env:METRIC_BENCH_DATABASE
    Address = $env:METRIC_BENCH_ADDRESS
    Target = $env:METRIC_TARGET
    LogRps = $env:METRIC_LOG_RPS
    ErrorRps = $env:METRIC_ERROR_RPS
    Duration = $env:METRIC_DURATION
    Result = $env:METRIC_RESULT
    RunId = $env:METRIC_RUN_ID
    Commit = $env:METRIC_COMMIT
    Rust = $env:METRIC_RUST
    K6 = $env:METRIC_K6
    Hardware = $env:METRIC_HARDWARE
    Mongo = $env:METRIC_MONGO
}

try {
    cargo build --locked --release --bin durable-ingest-bench
    if ($LASTEXITCODE -ne 0) { throw 'structured Log benchmark build failed' }
    & mongosh $MongoUri --quiet --eval "db.getSiblingDB('$Database').dropDatabase()" | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'fresh Phase 24 database preparation failed' }

    $env:METRIC_BENCH_MONGODB_URI = $MongoUri
    $env:METRIC_BENCH_DATABASE = $Database
    $env:METRIC_BENCH_ADDRESS = '127.0.0.1:3124'
    $stdout = Join-Path $env:TEMP "metric-phase24-server-$PID.out"
    $stderr = Join-Path $env:TEMP "metric-phase24-server-$PID.err"
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
    $env:METRIC_LOG_RPS = [string]$LogRps
    $env:METRIC_ERROR_RPS = [string]$ErrorRps
    $env:METRIC_TARGET = 'http://127.0.0.1:3124'
    $env:METRIC_DURATION = $Duration
    $env:METRIC_RESULT = $Result
    $env:METRIC_RUN_ID = $runId
    $env:METRIC_COMMIT = $commit
    $env:METRIC_RUST = $rust
    $env:METRIC_K6 = $k6Version
    $env:METRIC_HARDWARE = "$cpu; $ram GiB RAM; Windows"
    $env:METRIC_MONGO = 'MongoDB local standalone; direct connection'

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
    if (-not $KeepDatabase -and $Database -match '^metric_phase24_[a-z0-9_]+$') {
        & mongosh $MongoUri --quiet --eval "db.getSiblingDB('$Database').dropDatabase()" | Out-Null
    }
    foreach ($path in @($stdout, $stderr)) {
        if ($path -and (Test-Path -LiteralPath $path)) {
            Remove-Item -LiteralPath $path -Force
        }
    }
    $env:METRIC_BENCH_MONGODB_URI = $previous.Uri
    $env:METRIC_BENCH_DATABASE = $previous.Database
    $env:METRIC_BENCH_ADDRESS = $previous.Address
    $env:METRIC_TARGET = $previous.Target
    $env:METRIC_LOG_RPS = $previous.LogRps
    $env:METRIC_ERROR_RPS = $previous.ErrorRps
    $env:METRIC_DURATION = $previous.Duration
    $env:METRIC_RESULT = $previous.Result
    $env:METRIC_RUN_ID = $previous.RunId
    $env:METRIC_COMMIT = $previous.Commit
    $env:METRIC_RUST = $previous.Rust
    $env:METRIC_K6 = $previous.K6
    $env:METRIC_HARDWARE = $previous.Hardware
    $env:METRIC_MONGO = $previous.Mongo
}

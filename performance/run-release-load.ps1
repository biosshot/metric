param(
    [ValidateSet(1158, 5000, 20000)]
    [int]$Rps = 5000,
    [ValidatePattern('^\d+[smh]$')]
    [string]$Duration = '15s',
    [string]$MongoUri = 'mongodb://127.0.0.1:27017/?retryWrites=false',
    [ValidatePattern('^metric_phase22_[a-z0-9_]+$')]
    [string]$Database = "metric_phase22_$([DateTimeOffset]::UtcNow.ToUnixTimeSeconds())",
    [string]$Result = "performance/results/phase22-$Rps.json",
    [switch]$Maintenance,
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
    Maintenance = $env:METRIC_BENCH_MAINTENANCE
    Rps = $env:METRIC_RPS
    Duration = $env:METRIC_DURATION
    Result = $env:METRIC_RESULT
    RunId = $env:METRIC_RUN_ID
    Commit = $env:METRIC_COMMIT
    Rust = $env:METRIC_RUST
    K6 = $env:METRIC_K6
    Hardware = $env:METRIC_HARDWARE
    Mongo = $env:METRIC_MONGO
    Durability = $env:METRIC_DURABILITY
}

try {
    cargo build --locked --release --bin durable-ingest-bench
    if ($LASTEXITCODE -ne 0) { throw 'durable benchmark build failed' }
    & mongosh $MongoUri --quiet --eval "db.getSiblingDB('$Database').dropDatabase()" | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'fresh benchmark database preparation failed' }

    $env:METRIC_BENCH_MONGODB_URI = $MongoUri
    $env:METRIC_BENCH_DATABASE = $Database
    $env:METRIC_BENCH_ADDRESS = '127.0.0.1:3101'
    $env:METRIC_BENCH_MAINTENANCE = if ($Maintenance) { '1' } else { '0' }
    $stdout = Join-Path $env:TEMP "metric-phase22-server-$PID.out"
    $stderr = Join-Path $env:TEMP "metric-phase22-server-$PID.err"
    $server = Start-Process -FilePath 'target/release/durable-ingest-bench.exe' `
        -PassThru -WindowStyle Hidden -RedirectStandardOutput $stdout `
        -RedirectStandardError $stderr

    $ready = $false
    for ($attempt = 0; $attempt -lt 60; $attempt++) {
        try {
            $probe = Invoke-WebRequest -UseBasicParsing 'http://127.0.0.1:3101/ready' -TimeoutSec 1
            if ($probe.StatusCode -eq 200) { $ready = $true; break }
        } catch {}
        Start-Sleep -Milliseconds 500
    }
    if (-not $ready) { throw "benchmark server did not become ready: $(Get-Content $stderr -Raw)" }

    $commit = (git rev-parse HEAD).Trim()
    $rust = (rustc --version).Trim()
    $k6Version = (& k6 version | Select-Object -First 1).Trim()
    $cpu = (Get-CimInstance Win32_Processor | Select-Object -First 1 -ExpandProperty Name).Trim()
    $ram = [Math]::Round((Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory / 1GB, 1)
    $runIdText = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds().ToString()
    $runId = $runIdText.Substring($runIdText.Length - 8, 8)
    $env:METRIC_RPS = [string]$Rps
    $env:METRIC_DURATION = $Duration
    $env:METRIC_RESULT = $Result
    $env:METRIC_RUN_ID = $runId
    $env:METRIC_COMMIT = $commit
    $env:METRIC_RUST = $rust
    $env:METRIC_K6 = $k6Version
    $env:METRIC_HARDWARE = "$cpu; $ram GiB RAM; Windows"
    $env:METRIC_MONGO = 'MongoDB local standalone; direct connection'
    $env:METRIC_DURABILITY = if ($Maintenance) {
        'MongoWriter plus concurrent Phase 14 Scheduler'
    } else {
        'MongoWriter unordered insert_many to MongoDB'
    }

    & k6 run performance/k6/ingest-mongodb.js
    $k6Exit = $LASTEXITCODE
    if (-not (Test-Path -LiteralPath $Result)) { throw 'k6 did not write its result artifact' }
    $artifact = Get-Content -LiteralPath $Result -Raw | ConvertFrom-Json
    $accepted = [uint64]$artifact.metrics.failures.status_200
    $countText = & mongosh $MongoUri --quiet --eval "db.getSiblingDB('$Database').error_events.countDocuments({})"
    if ($LASTEXITCODE -ne 0) { throw 'MongoDB durable-count verification failed' }
    $durable = [uint64]($countText | Select-Object -Last 1)
    if ($durable -ne $accepted) {
        throw "acknowledged/durable mismatch: status_200=$accepted durable=$durable"
    }
    Write-Output "verified durable error_events: $durable; duplicate/lost acknowledged identities: 0"
    if ($k6Exit -ne 0) { throw "k6 thresholds failed with exit code $k6Exit; artifact retained at $Result" }
}
finally {
    if ($null -ne $server -and -not $server.HasExited) {
        Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
        $server.WaitForExit(10000) | Out-Null
    }
    if (-not $KeepDatabase -and $Database -match '^metric_phase22_[a-z0-9_]+$') {
        & mongosh $MongoUri --quiet --eval "db.getSiblingDB('$Database').dropDatabase()" | Out-Null
    }
    foreach ($path in @($stdout, $stderr)) {
        if ($path -and (Test-Path -LiteralPath $path)) { Remove-Item -LiteralPath $path -Force }
    }
    $env:METRIC_BENCH_MONGODB_URI = $previous.Uri
    $env:METRIC_BENCH_DATABASE = $previous.Database
    $env:METRIC_BENCH_ADDRESS = $previous.Address
    $env:METRIC_BENCH_MAINTENANCE = $previous.Maintenance
    $env:METRIC_RPS = $previous.Rps
    $env:METRIC_DURATION = $previous.Duration
    $env:METRIC_RESULT = $previous.Result
    $env:METRIC_RUN_ID = $previous.RunId
    $env:METRIC_COMMIT = $previous.Commit
    $env:METRIC_RUST = $previous.Rust
    $env:METRIC_K6 = $previous.K6
    $env:METRIC_HARDWARE = $previous.Hardware
    $env:METRIC_MONGO = $previous.Mongo
    $env:METRIC_DURABILITY = $previous.Durability
}

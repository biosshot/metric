$ErrorActionPreference = 'Stop'

$version = if ($env:METRIC_VERSION) { $env:METRIC_VERSION } else { '0.1.0' }
$installDir = if ($env:METRIC_INSTALL_DIR) { $env:METRIC_INSTALL_DIR } else { 'metric' }
$downloadBase = if ($env:METRIC_DOWNLOAD_BASE) {
    $env:METRIC_DOWNLOAD_BASE
} else {
    "https://raw.githubusercontent.com/biosshot/metric/v$version/deploy"
}

if (-not (Get-Command docker -ErrorAction SilentlyContinue)) {
    throw 'Docker is required: https://docs.docker.com/get-docker/'
}

docker compose version | Out-Null
if ($LASTEXITCODE -ne 0) {
    throw 'Docker Compose is required.'
}
New-Item -ItemType Directory -Force -Path $installDir | Out-Null

foreach ($file in @('compose.yml', 'metric.toml')) {
    $target = Join-Path $installDir $file
    if (-not (Test-Path -LiteralPath $target)) {
        $temporaryTarget = "$target.tmp"
        Remove-Item -LiteralPath $temporaryTarget -Force -ErrorAction SilentlyContinue
        Invoke-WebRequest -Uri "$downloadBase/$file" -OutFile $temporaryTarget
        Move-Item -LiteralPath $temporaryTarget -Destination $target
    }
}

function New-RandomHex([int]$ByteCount) {
    $bytes = New-Object byte[] $ByteCount
    $random = [Security.Cryptography.RandomNumberGenerator]::Create()
    try {
        $random.GetBytes($bytes)
    } finally {
        $random.Dispose()
    }
    return [BitConverter]::ToString($bytes).Replace('-', '').ToLowerInvariant()
}

$envFile = Join-Path $installDir '.env'
if (-not (Test-Path -LiteralPath $envFile)) {
    $content = @(
        "METRIC_MONGO_PASSWORD=$(New-RandomHex 24)"
        "METRIC_SCRUB_HMAC_KEY=$(New-RandomHex 32)"
        'METRIC_HTTP_PORT=4001'
        "METRIC_IMAGE=ghcr.io/biosshot/metric:$version"
        ''
    ) -join [Environment]::NewLine
    [IO.File]::WriteAllText(
        (Join-Path (Get-Location) $envFile),
        $content,
        [Text.UTF8Encoding]::new($false)
    )
}

Push-Location $installDir
try {
    docker compose pull
    if ($LASTEXITCODE -ne 0) {
        throw 'Failed to pull the container images.'
    }
    docker compose up -d --wait --wait-timeout 120
    if ($LASTEXITCODE -ne 0) {
        throw 'Failed to start Metric.'
    }
    docker compose ps
    if ($LASTEXITCODE -ne 0) {
        throw 'Failed to read the container status.'
    }
} finally {
    Pop-Location
}

Write-Host ''
Write-Host 'Metric is ready at http://localhost:4001'
Write-Host "First setup token: cd $installDir; docker compose logs metric"

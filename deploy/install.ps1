$ErrorActionPreference = 'Stop'

$version = if ($env:METRIC_VERSION) { $env:METRIC_VERSION } else { '0.1.0' }
$installDir = if ($env:METRIC_INSTALL_DIR) { $env:METRIC_INSTALL_DIR } else { 'metric' }
$profile = if ($env:METRIC_PROFILE) { $env:METRIC_PROFILE } else { 'medium' }
$downloadBase = if ($env:METRIC_DOWNLOAD_BASE) {
    $env:METRIC_DOWNLOAD_BASE
} else {
    "https://raw.githubusercontent.com/biosshot/metric/v$version/deploy"
}

if ($profile -notin @('min', 'low', 'medium', 'high')) {
    throw 'METRIC_PROFILE must be min, low, medium or high.'
}

if (-not (Get-Command docker -ErrorAction SilentlyContinue)) {
    throw 'Docker is required: https://docs.docker.com/get-docker/'
}

docker compose version | Out-Null
if ($LASTEXITCODE -ne 0) {
    throw 'Docker Compose is required.'
}
New-Item -ItemType Directory -Force -Path $installDir | Out-Null

foreach ($file in @('compose.yml', 'symbolicator.yml')) {
    $target = Join-Path $installDir $file
    if (-not (Test-Path -LiteralPath $target)) {
        $temporaryTarget = "$target.tmp"
        Remove-Item -LiteralPath $temporaryTarget -Force -ErrorAction SilentlyContinue
        Invoke-WebRequest -Uri "$downloadBase/$file" -OutFile $temporaryTarget
        Move-Item -LiteralPath $temporaryTarget -Destination $target
    }
}

$metricConfig = Join-Path $installDir 'metric.toml'
if (-not (Test-Path -LiteralPath $metricConfig)) {
    $temporaryTarget = "$metricConfig.tmp"
    Remove-Item -LiteralPath $temporaryTarget -Force -ErrorAction SilentlyContinue
    Invoke-WebRequest -Uri "$downloadBase/profiles/$profile.toml" -OutFile $temporaryTarget
    Move-Item -LiteralPath $temporaryTarget -Destination $metricConfig
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
    $temporaryTarget = "$envFile.tmp"
    Remove-Item -LiteralPath $temporaryTarget -Force -ErrorAction SilentlyContinue
    Invoke-WebRequest `
        -Uri "$downloadBase/profiles/$profile.env.example" `
        -OutFile $temporaryTarget
    $content = [IO.File]::ReadAllText($temporaryTarget)
    $content = $content.Replace(
        'replace-with-a-long-url-safe-random-password',
        (New-RandomHex 24)
    ).Replace(
        'replace-with-64-lowercase-hex-characters',
        (New-RandomHex 32)
    ).Replace(
        'ghcr.io/biosshot/metric:0.1.0',
        "ghcr.io/biosshot/metric:$version"
    )
    [IO.File]::WriteAllText(
        [IO.Path]::GetFullPath($temporaryTarget),
        $content,
        [Text.UTF8Encoding]::new($false)
    )
    Move-Item -LiteralPath $temporaryTarget -Destination $envFile
}

$activeProfile = (
    Get-Content -LiteralPath $envFile |
        Where-Object { $_ -like 'METRIC_PROFILE=*' } |
        Select-Object -First 1
)
if ($activeProfile) {
    $activeProfile = $activeProfile.Substring('METRIC_PROFILE='.Length)
} else {
    $activeProfile = 'unknown'
}
if ($activeProfile -ne 'unknown' -and $activeProfile -ne $profile) {
    Write-Warning (
        "Existing installation keeps profile $activeProfile; " +
        "requested $profile was not applied."
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
Write-Host "Profile: $activeProfile"
Write-Host 'Metric is ready at http://localhost:4001'
Write-Host "First setup token: cd $installDir; docker compose logs metric"

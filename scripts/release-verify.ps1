param(
    [ValidateSet('contracts', 'compatibility', 'web', 'full')]
    [string]$Profile = 'contracts'
)

$ErrorActionPreference = 'Stop'

function Invoke-Checked([string]$Name, [scriptblock]$Command) {
    Write-Output "==> $Name"
    & $Command
    if ($LASTEXITCODE -ne 0) { throw "$Name failed with exit code $LASTEXITCODE" }
}

function Get-DescendantProcessIds([int]$RootPid) {
    $processes = Get-CimInstance Win32_Process
    $parents = @($RootPid)
    $descendants = @()
    do {
        $children = @($processes | Where-Object { $parents -contains [int]$_.ParentProcessId })
        $parents = @($children | ForEach-Object { [int]$_.ProcessId })
        $descendants += $parents
    } while ($parents.Count -gt 0)
    return $descendants
}

function Invoke-Contracts {
    Invoke-Checked 'compatibility manifest' { python scripts/validate-compatibility.py }
    Invoke-Checked 'Rust format' { cargo fmt --all -- --check }
    Invoke-Checked 'dependency direction' { cargo dep-graph --locked }
    Invoke-Checked 'strict Rust lint' {
        cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
    }
    Invoke-Checked 'workspace tests' {
        cargo test --workspace --all-targets --all-features --locked
    }
}

function Invoke-Compatibility {
    Push-Location sdk-tests/node
    try {
        Invoke-Checked 'Node SDK install' { npm ci }
        Invoke-Checked 'Node SDK format' { npm run format:check }
        Invoke-Checked 'Node SDK lint' { npm run lint }
    } finally {
        Pop-Location
    }
    Invoke-Checked 'real Node SDK Error Event' {
        cargo test -p faultkeep-server --test sdk_compatibility_e2e `
            real_node_sdk_sends_an_error_event_without_blob -- --ignored --exact --nocapture
    }
    Invoke-Checked 'real Node SDK attachment Event' {
        cargo test -p faultkeep-server --test sdk_compatibility_e2e `
            real_node_sdk_sends_an_attachment_event -- --ignored --exact --nocapture
    }

    Push-Location sdk-tests/browser
    try {
        Invoke-Checked 'Browser SDK install' { npm ci }
        Invoke-Checked 'Browser SDK format' { npm run format:check }
        Invoke-Checked 'Browser SDK lint' { npm run lint }
        Invoke-Checked 'Browser SDK bundle' { npm run build }
        Invoke-Checked 'Playwright Chromium install' { npx playwright install chromium }
    } finally {
        Pop-Location
    }
    Invoke-Checked 'real Browser SDK Error Event' {
        cargo test -p faultkeep-server --test sdk_compatibility_e2e `
            real_browser_sdk_sends_an_error_event -- --ignored --exact --nocapture
    }

    Push-Location sdk-tests/sentry-cli
    try {
        Invoke-Checked 'Sentry CLI install' { npm ci }
        Invoke-Checked 'Sentry CLI pinned versions' { npm run versions }
    } finally {
        Pop-Location
    }
}

function Invoke-Web {
    Push-Location web
    try {
        Invoke-Checked 'Web install' { npm ci }
        Invoke-Checked 'Web format' { npm run format:check }
        Invoke-Checked 'Web lint' { npm run lint }
        Invoke-Checked 'Web unit tests' { npm test }
        Invoke-Checked 'Web production build' { npm run build }
    } finally {
        Pop-Location
    }
}

try {
    switch ($Profile) {
        'contracts' { Invoke-Contracts }
        'compatibility' { Invoke-Compatibility }
        'web' { Invoke-Web }
        'full' {
            Invoke-Checked 'complete SDK family release gate' {
                python scripts/validate-compatibility.py --require-all
            }
            if (-not $env:FAULTKEEP_TEST_MONGODB_URI) {
                throw 'FAULTKEEP_TEST_MONGODB_URI is required for full verification'
            }
            Invoke-Contracts
            Invoke-Compatibility
            Invoke-Web
            Invoke-Checked 'real infrastructure suites' {
                cargo test --workspace --all-targets --all-features --locked `
                    infrastructure_ -- --ignored --nocapture
            }
        }
    }
}
finally {
    $descendants = Get-DescendantProcessIds -RootPid $PID
    foreach ($processId in $descendants) {
        Stop-Process -Id $processId -Force -ErrorAction SilentlyContinue
    }
}

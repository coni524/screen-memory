# Release build plus installer generation (requires Inno Setup 6).
# Usage: powershell -ExecutionPolicy Bypass -File packaging\build-installer.ps1
# Output: packaging\dist\screen-memory-setup-<version>.exe
$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path

# Dependency crates bake their own source paths into panic messages and log
# events, which would ship the build machine's profile directory
# (C:\Users\<name>\.cargo\registry\...) inside the exe. Rewrite that prefix to a
# plain "~" so the distributed exe carries no user name.
$env:RUSTFLAGS = "--remap-path-prefix=$env:USERPROFILE=~ $env:RUSTFLAGS".Trim()

# --remap-path-prefix only reaches rustc. aws-lc-sys and ring compile their C
# through cc-rs, and cl.exe bakes each __FILE__ into the assert and error
# strings it emits, which put the profile directory back into the exe by a
# second route. /d1trimfile: strips the same prefix off __FILE__; it is
# undocumented but has been in cl.exe since VS 2019. cc-rs splits these
# variables on whitespace, so a profile path containing a space would not
# survive being passed through.
$env:CFLAGS = "/d1trimfile:$env:USERPROFILE\ $env:CFLAGS".Trim()
$env:CXXFLAGS = "/d1trimfile:$env:USERPROFILE\ $env:CXXFLAGS".Trim()

cargo build --release --manifest-path (Join-Path $root "Cargo.toml")
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# Take the version from [package] in Cargo.toml
$version = (Select-String -Path (Join-Path $root "Cargo.toml") -Pattern '^version\s*=\s*"([^"]+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value

$iscc = @(
    (Join-Path ${env:ProgramFiles(x86)} "Inno Setup 6\ISCC.exe"),
    (Join-Path $env:ProgramFiles "Inno Setup 6\ISCC.exe"),
    (Join-Path $env:LOCALAPPDATA "Programs\Inno Setup 6\ISCC.exe")
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $iscc) {
    Write-Error "ISCC.exe not found. Install Inno Setup 6: winget install JRSoftware.InnoSetup"
}

& $iscc "/DAppVersion=$version" (Join-Path $PSScriptRoot "installer.iss")
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host "Built: $(Join-Path $PSScriptRoot "dist\screen-memory-setup-$version.exe")"

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

# Inno Setup installs into a directory named after its major version
# ("Inno Setup 6", "Inno Setup 7", ...), so match any of them and take the
# newest instead of pinning one version. Sorting the paths in reverse puts the
# highest major first. installer.iss stays compatible with 6 as well: it sets
# ArchitecturesAllowed and ArchitecturesInstallIn64BitMode itself and does not
# use the SetupArchitecture directive that only 7 understands.
$iscc = @($env:ProgramFiles, ${env:ProgramFiles(x86)}, (Join-Path $env:LOCALAPPDATA "Programs")) |
    Where-Object { $_ } |
    ForEach-Object { Get-ChildItem -Path (Join-Path $_ "Inno Setup *\ISCC.exe") -ErrorAction SilentlyContinue } |
    Sort-Object FullName -Descending |
    Select-Object -First 1 -ExpandProperty FullName
if (-not $iscc) {
    Write-Error "ISCC.exe not found. Install Inno Setup: winget install JRSoftware.InnoSetup"
}
Write-Host "Using $iscc"

& $iscc "/DAppVersion=$version" (Join-Path $PSScriptRoot "installer.iss")
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host "Built: $(Join-Path $PSScriptRoot "dist\screen-memory-setup-$version.exe")"

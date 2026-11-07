# Build the native segment builder (ffi/torque-build) for the client SDKs and copy it where the
# .NET client packs it. Portable CPU baseline (x86-64-v3: AVX2, BMI2, FMA) instead of the
# repository's target-cpu=native: the library runs on the machines that build segments.
#   pwsh ffi/torque-build/build_native.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
# Where cargo puts it: CARGO_TARGET_DIR, a target-dir in a cargo config, or target/.
$target = (cargo metadata --format-version 1 --no-deps --manifest-path (Join-Path $root 'Cargo.toml') | ConvertFrom-Json).target_directory
# Encoded flags (0x1F-separated): paths may contain spaces.
$env:CARGO_ENCODED_RUSTFLAGS = @(
    '-C', 'target-cpu=x86-64-v3',
    '--remap-path-prefix', "$root=src",
    '--remap-path-prefix', "$env:USERPROFILE\.cargo\registry\src\=dep:",
    '--remap-path-prefix', "$env:USERPROFILE\.rustup\toolchains\=toolchain:"
) -join [char]0x1F
Push-Location $root
try {
    cargo build -p torque-build --profile ffi
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
} finally {
    Pop-Location
}
$dest = Join-Path $root 'clients\dotnet\src\Torque.Client\runtimes\win-x64\native'
New-Item -ItemType Directory -Force $dest | Out-Null
Copy-Item (Join-Path $target 'ffi\torque_build.dll') $dest -Force
Write-Host "torque_build.dll -> $dest"

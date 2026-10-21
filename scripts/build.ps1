param(
    [switch]$Debug
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
if (-not (Test-Path $vswhere)) {
    throw 'vswhere not found. Install Visual Studio Build Tools with the C++ workload.'
}
$vsPath = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vsPath) {
    throw 'MSVC C++ tools not found.'
}
$vcvars = Join-Path $vsPath 'VC\Auxiliary\Build\vcvars64.bat'
if (-not (Test-Path $vcvars)) {
    throw "vcvars64.bat not found at $vcvars"
}
$link = Get-ChildItem (Join-Path $vsPath 'VC\Tools\MSVC') -Recurse -Filter link.exe |
    Where-Object { $_.FullName -match 'Hostx64\\x64\\link.exe$' } |
    Select-Object -First 1 -ExpandProperty FullName
if (-not $link) {
    throw 'MSVC link.exe not found.'
}
$env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER = $link
$nasm = Join-Path $root 'tools\nasm-2.16.03'
if (-not (Test-Path (Join-Path $nasm 'nasm.exe'))) {
    $tools = Join-Path $root 'tools'
    New-Item -ItemType Directory -Force -Path $tools | Out-Null
    $zip = Join-Path $tools 'nasm.zip'
    try {
        if (-not (Test-Path $zip)) {
            Invoke-WebRequest -UseBasicParsing -Uri 'https://www.nasm.us/pub/nasm/releasebuilds/2.16.03/win64/nasm-2.16.03-win64.zip' -OutFile $zip
        }
        Expand-Archive -Path $zip -DestinationPath $tools -Force
    } catch {
        Write-Warning "NASM download failed: $($_.Exception.Message). Install NASM and add it to PATH."
    }
}
if (-not (Test-Path (Join-Path $nasm 'nasm.exe'))) {
    $nasm = ''
}
$profileFlag = if ($Debug) { '' } else { '--release' }
$nasmPath = if ($nasm) { "set `"PATH=$nasm;%PATH%`" && " } else { '' }
$command = "`"$vcvars`" >nul && $nasmPath npx napi build --platform $profileFlag --no-js --dts native.d.ts --manifest-path crates/fastpkg-napi/Cargo.toml --output-dir packages/image"
Push-Location $root
try {
    & cmd /c $command
    if ($LASTEXITCODE -ne 0) {
        throw "native build failed with exit code $LASTEXITCODE"
    }
} finally {
    Pop-Location
}

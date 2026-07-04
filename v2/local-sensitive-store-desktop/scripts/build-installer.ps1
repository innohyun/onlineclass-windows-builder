$ErrorActionPreference = "Stop"

$projectRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"

if (Test-Path $cargoBin -PathType Container) {
  if ($env:Path -notlike "*$cargoBin*") {
    $env:Path = "$cargoBin;$env:Path"
  }
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
  throw "Rust toolchain not found in PATH. Install rustup first."
}

$vcvarsCandidates = @(
  "C:\BuildTools\VC\Auxiliary\Build\vcvars64.bat",
  "C:\Program Files\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat",
  "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
)
$vcvars = $vcvarsCandidates | Where-Object { Test-Path $_ -PathType Leaf } | Select-Object -First 1

Push-Location $projectRoot
try {
  if ($vcvars) {
    Write-Host "[build-installer] Using vcvars: $vcvars"
    cmd /c "`"$vcvars`" >nul && npx tauri build --bundles nsis"
    exit $LASTEXITCODE
  }

  npx tauri build --bundles nsis
  exit $LASTEXITCODE
}
finally {
  Pop-Location
}

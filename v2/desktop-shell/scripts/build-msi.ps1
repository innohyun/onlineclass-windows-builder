$ErrorActionPreference = 'Stop'

Write-Host '[desktop-shell] build installer (nsis + zip) 시작' -ForegroundColor Cyan

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot '..')
Set-Location $repoRoot

npm install
npm run build:installer

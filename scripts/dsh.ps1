#!/usr/bin/env pwsh

$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path

if ($args.Count -gt 0 -and $args[0] -eq "web") {
  & pnpm --dir $projectRoot dsh @args
  exit $LASTEXITCODE
}

$existingDsh = Join-Path $env:APPDATA "npm\dsh.ps1"
if (Test-Path $existingDsh -and (Resolve-Path $existingDsh).Path -ne $MyInvocation.MyCommand.Path) {
  & $existingDsh @args
  exit $LASTEXITCODE
}

Write-Error ("No existing dsh command was found. Use pnpm --dir {0} dsh web." -f $projectRoot)
exit 1

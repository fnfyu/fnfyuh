#!/usr/bin/env pwsh

$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
& pnpm --dir $projectRoot fnfyuh @args
exit $LASTEXITCODE

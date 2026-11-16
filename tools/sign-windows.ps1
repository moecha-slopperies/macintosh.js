param(
  [Parameter(Mandatory = $true)]
  [string]$Path
)

$ErrorActionPreference = "Stop"

$missing = @(
  "SIGNTOOL_PATH",
  "AZURE_CODE_SIGNING_DLIB",
  "AZURE_TENANT_ID",
  "AZURE_CLIENT_ID",
  "AZURE_CLIENT_SECRET",
  "AZURE_CODE_SIGNING_ACCOUNT_NAME",
  "AZURE_CODE_SIGNING_CERTIFICATE_PROFILE_NAME"
) | Where-Object { -not [Environment]::GetEnvironmentVariable($_) }

if ($missing.Count -gt 0) {
  Write-Warning "Windows signing is not configured; leaving $Path unsigned."
  exit 0
}

$metadataPath = Join-Path $env:TEMP "macintosh-js-trusted-signing-$PID.json"
$endpoint = if ($env:AZURE_CODE_SIGNING_ENDPOINT) {
  $env:AZURE_CODE_SIGNING_ENDPOINT
} else {
  "https://wcus.codesigning.azure.net"
}

@{
  Endpoint = $endpoint
  CodeSigningAccountName = $env:AZURE_CODE_SIGNING_ACCOUNT_NAME
  CertificateProfileName = $env:AZURE_CODE_SIGNING_CERTIFICATE_PROFILE_NAME
} | ConvertTo-Json | Set-Content -Path $metadataPath -Encoding utf8

try {
  & $env:SIGNTOOL_PATH sign `
    /v `
    /fd SHA256 `
    /td SHA256 `
    /tr http://timestamp.acs.microsoft.com `
    /dlib $env:AZURE_CODE_SIGNING_DLIB `
    /dmdf $metadataPath `
    $Path
  if ($LASTEXITCODE -ne 0) {
    throw "signtool failed with exit code $LASTEXITCODE"
  }
} finally {
  Remove-Item $metadataPath -Force -ErrorAction SilentlyContinue
}

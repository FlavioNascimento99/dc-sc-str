# Remove a instalação do usuário atual, o atalho e o início automático.
# Uso: powershell -File installer\uninstall.ps1

$ErrorActionPreference = "Stop"
$dir = Join-Path $env:LOCALAPPDATA "DiscordScreenCompanion"
$shortcut = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Discord Screen Companion.lnk"
if (Test-Path -LiteralPath $shortcut) { Remove-Item -LiteralPath $shortcut -Force }
if (Test-Path -LiteralPath $dir) { Remove-Item -LiteralPath $dir -Recurse -Force }

$run = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
if (Get-ItemProperty -Path $run -Name "DiscordScreenCompanion" -ErrorAction SilentlyContinue) {
    Remove-ItemProperty -Path $run -Name "DiscordScreenCompanion"
}
Write-Output "Discord Screen Companion removido."

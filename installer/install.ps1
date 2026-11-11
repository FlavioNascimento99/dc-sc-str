# Instala o Discord Screen Companion para o usuário atual.
# Não pede administrador. A assinatura só roda se SIGN_THUMBPRINT estiver definido
# e o signtool.exe estiver no PATH.
#
# Uso, a partir de dc-sc-str, depois do build de release:
#   powershell -File installer\install.ps1 -Signal http://IP:8787
#   powershell -File installer\uninstall.ps1

param(
    [string]$Exe,
    [Parameter(Mandatory = $true)][string]$Signal
)

$ErrorActionPreference = "Stop"
if (-not $Exe) {
    $Exe = Join-Path $PSScriptRoot "..\poc\target\release\poc.exe"
}
if (-not (Test-Path -LiteralPath $Exe)) {
    throw "Não achei o executável em $Exe. Rode cargo build --release em poc\."
}

$dir = Join-Path $env:LOCALAPPDATA "DiscordScreenCompanion"
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$dest = Join-Path $dir "poc.exe"
Copy-Item -LiteralPath $Exe -Destination $dest -Force

if ($env:SIGN_THUMBPRINT) {
    $signtool = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if (-not $signtool) {
        Write-Warning "SIGN_THUMBPRINT definido, mas signtool.exe não está no PATH. O executável ficou sem assinatura."
    } else {
        & signtool.exe sign /sha1 $env:SIGN_THUMBPRINT /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 $dest
        if ($LASTEXITCODE -ne 0) { throw "signtool falhou com $LASTEXITCODE" }
    }
}

$shortcutPath = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Discord Screen Companion.lnk"
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($shortcutPath)
$shortcut.TargetPath = $dest
$shortcut.Arguments = "tray --signal $Signal"
$shortcut.WorkingDirectory = $dir
$shortcut.Description = "Compartilhamento de tela para chamadas do Discord"
$shortcut.Save()

Write-Output "Instalado em $dest"
Write-Output "Atalho: $shortcutPath"
Write-Output "Abra pelo menu Iniciar. O início com o Windows fica no menu do ícone."

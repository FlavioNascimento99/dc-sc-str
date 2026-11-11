use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::version::VERSION;

#[derive(Deserialize)]
struct Manifest {
    version: String,
    url: String,
}

pub fn prompt(base: &str) {
    if let Err(err) = offer(base) {
        eprintln!("update: {err:#}");
    }
}

fn offer(base: &str) -> Result<()> {
    let body = get(&format!("{}/update", base.trim_end_matches('/')))?;
    if body.is_empty() {
        return Ok(());
    }
    let manifest: Manifest = serde_json::from_str(&body).context("manifesto")?;
    if !newer(&manifest.version, VERSION) {
        eprintln!("update: {VERSION} já é a versão atual");
        return Ok(());
    }
    if !manifest.url.starts_with("https://") && !manifest.url.starts_with("http://") {
        anyhow::bail!("url de atualização recusada");
    }
    if !confirm(&manifest.version)? {
        return Ok(());
    }
    let current = std::env::current_exe().context("executável")?;
    let download = current.with_extension("new.exe");
    download_file(&manifest.url, &download)?;
    stage_replace(&download, &current)?;
    std::process::exit(0);
}

fn confirm(version: &str) -> Result<bool> {
    use windows::core::w;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, IDYES, MB_YESNO};
    let text: Vec<u16> = format!("Há uma versão {version}. Instalar agora?")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let answer = unsafe {
        MessageBoxW(
            None,
            windows::core::PCWSTR(text.as_ptr()),
            w!("discord-companion"),
            MB_YESNO,
        )
    };
    Ok(answer == IDYES)
}

fn get(url: &str) -> Result<String> {
    let (status, body) = exchange("GET", url)?;
    if status == 204 {
        return Ok(String::new());
    }
    if status != 200 {
        anyhow::bail!("update HTTP {status}");
    }
    Ok(body)
}

fn exchange(method: &str, url: &str) -> Result<(u16, String)> {
    let rest = url.strip_prefix("http://").context("update só consulta http:// do servidor")?;
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let addr = if host.contains(':') { host.to_string() } else { format!("{host}:80") };
    let mut stream = TcpStream::connect(&addr)?;
    let request = format!("{method} /{path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes())?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let (head, body) = response.split_once("\r\n\r\n").context("resposta")?;
    let code = head
        .split_whitespace()
        .nth(1)
        .unwrap_or("0")
        .parse::<u16>()
        .unwrap_or(0);
    Ok((code, body.trim().to_string()))
}

fn download_file(url: &str, dest: &PathBuf) -> Result<()> {
    let dest = dest.display().to_string().replace('\'', "''");
    let url = url.replace('\'', "''");
    let status = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!("Invoke-WebRequest -UseBasicParsing -Uri '{url}' -OutFile '{dest}'"),
        ])
        .status()
        .context("powershell")?;
    if !status.success() {
        anyhow::bail!("download da atualização falhou");
    }
    Ok(())
}

fn stage_replace(download: &PathBuf, current: &PathBuf) -> Result<()> {
    let script = std::env::temp_dir().join("discord-companion-update.ps1");
    let args = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let body = format!(
        "$src = '{src}'\n$dst = '{dst}'\nfor ($i = 0; $i -lt 30; $i++) {{\n  try {{ Move-Item -Force -LiteralPath $src -Destination $dst; break }} catch {{ Start-Sleep -Seconds 1 }}\n}}\nStart-Process -FilePath $dst -ArgumentList '{args}'\n",
        src = download.display().to_string().replace('\'', "''"),
        dst = current.display().to_string().replace('\'', "''"),
        args = args.replace('\'', "''"),
    );
    std::fs::write(&script, body)?;
    std::process::Command::new("powershell")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-File", &script.display().to_string()])
        .spawn()
        .context("aplicar atualização")?;
    Ok(())
}

fn newer(remote: &str, local: &str) -> bool {
    parts(remote) > parts(local)
}

fn parts(value: &str) -> Vec<u32> {
    let mut numbers = value
        .trim()
        .trim_start_matches('v')
        .split('.')
        .map(|part| part.parse::<u32>().unwrap_or(0))
        .collect::<Vec<_>>();
    numbers.resize(3, 0);
    numbers
}

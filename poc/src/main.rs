mod host_room;
mod meter;
mod capture;
mod color;
mod decode;
mod encode;
mod present;
mod rtc;
mod session;
mod signal;
mod tray;
mod update;
mod version;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

use capture::Capture;
use decode::Decoder;
use encode::Encoder;
use rtc::{Inbound, Peer};
use signal::{Sig, Signal};

fn main() {
    if let Err(err) = run() {
        eprintln!("erro: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() {
        eprintln!(
            "uso:\n  signaling 8787\n  poc login --signal http://IP:8787\n  poc tray --signal http://IP:8787
  poc update --signal http://IP:8787\n  poc whoami\n  poc logout --signal http://IP:8787\n  poc host --signal http://IP:8787 [--allow-software]\n  poc view --signal http://IP:8787 --code CODIGO\n\nModo local, sem servidor e sem login:\n  poc host [--port 9876] [--allow-software]\n  poc view [--port 9876]"
        );
        std::process::exit(2);
    }
    let cmd = args.remove(0);
    let port = take_flag_value(&mut args, "--port")
        .unwrap_or_else(|| "9876".into())
        .parse::<u16>()
        .context("--port")?;
    let signal = take_flag_value(&mut args, "--signal");
    let code = take_flag_value(&mut args, "--code");
    match cmd.as_str() {
        "login" => login(signal.context("--signal")?),
        "logout" => logout(signal),
        "whoami" => whoami(),
        "tray" => tray::run(signal.context("--signal")?),
        "update" => {
            update::prompt(&signal.context("--signal")?);
            Ok(())
        }
        "host" => host(port, args.iter().any(|a| a == "--allow-software"), signal, &AtomicBool::new(false), None),
        "view" => view(port, signal, code),
        _ => anyhow::bail!("comando desconhecido: {cmd}"),
    }
}

fn take_flag_value(args: &mut Vec<String>, flag: &str) -> Option<String> {
    let pos = args.iter().position(|a| a == flag)?;
    args.remove(pos);
    if pos < args.len() {
        Some(args.remove(pos))
    } else {
        None
    }
}

fn prime_encoder(encoder: &mut Encoder, blank: &[u8]) -> Result<()> {
    for _ in 0..8 {
        if let Some(au) = encoder.encode(blank)? {
            eprintln!("encoder: AU inicial {} bytes", au.len());
            return Ok(());
        }
    }
    eprintln!("encoder: sem AU nos primeiros quadros");
    Ok(())
}

pub fn host(
    port: u16,
    allow_software: bool,
    signal_base: Option<String>,
    stop: &AtomicBool,
    code_out: Option<Arc<Mutex<Option<String>>>>,
) -> Result<()> {
    init_win()?;
    let room_mode = signal_base.is_some();
    let mut signal = if let Some(base) = signal_base {
        let (token, user) = session::load()?;
        eprintln!("conta: {} ({})", user.global_name.as_deref().unwrap_or(&user.username), user.username);
        let (signal, code) = Signal::open_room(&base, &token)?;
        if let Some(slot) = &code_out {
            *slot.lock().unwrap_or_else(|err| err.into_inner()) = Some(code.clone());
        }
        eprintln!("sala: {code}");
        signal
    } else {
        Signal::listen(port)?
    };
    let mut encoder = Encoder::new(allow_software)?;
    let mut blank = vec![16u8; color::nv12_len()];
    blank[(color::WIDTH * color::HEIGHT) as usize..].fill(128);
    if let Err(err) = prime_encoder(&mut encoder, &blank) {
        if !encoder.hardware() {
            return Err(err);
        }
        eprintln!("encoder: hardware não entregou bitstream ({err:#}). Usando encoder de software.");
        encoder = Encoder::software()?;
        prime_encoder(&mut encoder, &blank)?;
    }
    let capture = Capture::start()?;
    if room_mode {
        return host_room::pump(signal, encoder, capture, stop);
    }
    let mut peer = Peer::new()?;
    let offer = peer.create_offer()?;
    signal.send(&Sig::Offer { body: offer })?;
    eprintln!("sinalização: offer enviado");
    loop {
        if let Some(msg) = signal.try_recv()? {
            match msg {
                Sig::Answer { body, .. } => {
                    peer.accept_answer(&body)?;
                    eprintln!("sinalização: answer aceito");
                    break;
                }
                Sig::Offer { .. } | Sig::ViewerJoined { .. } | Sig::ViewerLeft { .. } => {}
            }
        }
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let _ = peer.poll()?;
        thread::sleep(Duration::from_millis(5));
    }

    let started = Instant::now();
    let mut sent = 0u64;
    let mut last_log = Instant::now();
    let mut encode_us = 0u128;
    let mut last_idr = Instant::now();
    loop {
        for _ in 0..8 {
            let _ = peer.poll()?;
            if let Some(msg) = signal.try_recv()? {
                if matches!(msg, Sig::Answer { .. }) {
                    eprintln!("sinalização: answer extra ignorado");
                }
            }
        }
        if peer.take_keyframe_request() || last_idr.elapsed() >= Duration::from_secs(2) {
            encoder.force_keyframe();
            last_idr = Instant::now();
        }
        if peer.connected() {
            if let Some(nv12) = capture.take() {
                let t0 = Instant::now();
                if let Some(au) = encoder.encode(&nv12)? {
                    encode_us += t0.elapsed().as_micros();
                    peer.send_h264(&au)?;
                    sent += 1;
                }
            }
        }
        if last_log.elapsed() >= Duration::from_secs(1) {
            let avg = if sent == 0 { 0 } else { encode_us / sent as u128 };
            eprintln!(
                "host: capturados {} enviados {sent} encode médio {avg}us ice {} há {:.0}s encoder {} {}",
                capture.frames(),
                if peer.connected() { "ok" } else { "aguardando" },
                started.elapsed().as_secs_f32(),
                encoder.name(),
                if encoder.hardware() { "hw" } else { "sw" },
            );
            last_log = Instant::now();
        }
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(2));
    }
}

fn view(port: u16, signal_base: Option<String>, code: Option<String>) -> Result<()> {
    init_win()?;
    let latest = Arc::new(Mutex::new(None));
    let received = Arc::new(Mutex::new(0u64));
    let frames = latest.clone();
    let count = received.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let net_stop = stop.clone();
    thread::spawn(move || {
        if let Err(err) = view_net(port, signal_base, code, frames, count, &net_stop) {
            eprintln!("view: {err:#}");
        }
    });
    present::run(latest, received)?;
    stop.store(true, Ordering::Relaxed);
    Ok(())
}

pub fn view_net(
    port: u16,
    signal_base: Option<String>,
    code: Option<String>,
    latest: Arc<Mutex<Option<Vec<u8>>>>,
    received: Arc<Mutex<u64>>,
    stop: &AtomicBool,
) -> Result<()> {
    if signal_base.is_some() {
        while !stop.load(Ordering::Relaxed) {
            match view_session(port, signal_base.clone(), code.clone(), latest.clone(), received.clone(), stop) {
                Ok(()) => return Ok(()),
                Err(err) => {
                    eprintln!("view: {err:#}; nova tentativa em 2s");
                    thread::sleep(Duration::from_secs(2));
                }
            }
        }
        return Ok(());
    }
    view_session(port, signal_base, code, latest, received, stop)
}

fn view_session(
    port: u16,
    signal_base: Option<String>,
    code: Option<String>,
    latest: Arc<Mutex<Option<Vec<u8>>>>,
    received: Arc<Mutex<u64>>,
    stop: &AtomicBool,
) -> Result<()> {
    let mut signal = if let Some(base) = signal_base {
        let code = code.context("--code é obrigatório com --signal")?;
        let (token, _) = session::load()?;
        Signal::join_room(&base, &code, &token)?
    } else {
        Signal::connect(port)?
    };
    let mut peer = Peer::new()?;
    let offer = loop {
        if let Some(Sig::Offer { body }) = signal.try_recv()? {
            break body;
        }
        thread::sleep(Duration::from_millis(5));
    };
    let answer = peer.accept_offer(&offer)?;
    signal.send(&Sig::Answer { body: answer, from: None })?;
    eprintln!("view: answer enviado");
    let mut decoder = Decoder::new()?;
    let mut decoded = 0u64;
    let mut last_log = Instant::now();
    let mut lost: Option<Instant> = None;
    loop {
        if let Some(msg) = signal.try_recv()? {
            if let Sig::Offer { body } = msg {
                peer = Peer::new()?;
                let answer = peer.accept_offer(&body)?;
                signal.send(&Sig::Answer { body: answer, from: None })?;
                eprintln!("view: novo offer aceito");
                lost = None;
            }
        }
        if peer.connected() {
            lost = None;
        } else if decoded > 0 {
            if lost.get_or_insert_with(Instant::now).elapsed() >= Duration::from_secs(8) {
                anyhow::bail!("ice caiu");
            }
        }
        let inbound = peer.poll()?;
        for item in inbound {
            let Inbound::Video(au) = item;
            *received.lock().unwrap_or_else(|e| e.into_inner()) = peer.received();
            if let Some(bgra) = decoder.decode(&au)? {
                *latest.lock().unwrap_or_else(|e| e.into_inner()) = Some(bgra);
                decoded += 1;
            }
        }
        if last_log.elapsed() >= Duration::from_secs(1) {
            eprintln!(
                "view: recebidos {} decodificados {decoded} ice {}",
                peer.received(),
                if peer.connected() { "ok" } else { "aguardando" }
            );
            last_log = Instant::now();
        }
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(2));
    }
}

fn login(base: String) -> Result<()> {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind(("127.0.0.1", 0)).context("porta local do login")?;
    let port = listener.local_addr()?.port();
    let url = format!("{}/auth/discord?local_port={port}", base.trim_end_matches('/'));
    std::process::Command::new("cmd")
        .args(["/C", "start", "", &url])
        .spawn()
        .context("abrir o navegador")?;
    eprintln!("login: autorizando em {url}");
    let (mut stream, _) = listener.accept().context("retorno do navegador")?;
    let mut buf = [0u8; 2048];
    let n = stream.read(&mut buf)?;
    let request = String::from_utf8_lossy(&buf[..n]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .context("pedido local")?;
    let ticket = path
        .split("ticket=")
        .nth(1)
        .and_then(|value| value.split('&').next())
        .context("ticket ausente")?;
    let page = "Login concluído. Pode fechar esta aba.";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
        page.len()
    );
    stream.write_all(response.as_bytes())?;
    let body = signal::http_send(
        "POST",
        &format!("{}/auth/ticket", base.trim_end_matches('/')),
        &format!(r#"{{"ticket":"{ticket}"}}"#),
        None,
    )?;
    let created: LoginResponse = serde_json::from_str(&body).context("resposta do ticket")?;
    session::save(&created.token, created.user.clone())?;
    eprintln!(
        "login: {} ({})",
        created.user.global_name.as_deref().unwrap_or(&created.user.username),
        created.user.username
    );
    Ok(())
}

#[derive(serde::Deserialize)]
struct LoginResponse {
    token: String,
    user: session::User,
}

fn logout(base: Option<String>) -> Result<()> {
    if let Some(base) = base {
        if let Ok((token, _)) = session::load() {
            let _ = signal::http_send(
                "POST",
                &format!("{}/auth/logout", base.trim_end_matches('/')),
                "",
                Some(&token),
            );
        }
    }
    session::clear()?;
    eprintln!("logout: sessão local apagada");
    Ok(())
}

fn whoami() -> Result<()> {
    let (_, user) = session::load()?;
    eprintln!(
        "{} ({}) id {}",
        user.global_name.as_deref().unwrap_or(&user.username),
        user.username,
        user.id
    );
    Ok(())
}

fn init_win() -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
    }
    Ok(())
}

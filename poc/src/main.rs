mod capture;
mod color;
mod decode;
mod encode;
mod present;
mod rtc;
mod signal;

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
            "uso:\n  poc host [--port 9876] [--allow-software]\n  poc view [--port 9876]\n\nSuba o host primeiro. Na mesma máquina, abra outro terminal e rode o view."
        );
        std::process::exit(2);
    }
    let cmd = args.remove(0);
    let port = take_flag_value(&mut args, "--port")
        .unwrap_or_else(|| "9876".into())
        .parse::<u16>()
        .context("--port")?;
    match cmd.as_str() {
        "host" => host(port, args.iter().any(|a| a == "--allow-software")),
        "view" => view(port),
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

fn host(port: u16, allow_software: bool) -> Result<()> {
    init_win()?;
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
    let mut signal = Signal::listen(port)?;
    let mut peer = Peer::new()?;
    let offer = peer.create_offer()?;
    signal.send(&Sig::Offer { body: offer })?;
    eprintln!("sinalização: offer enviado");
    loop {
        if let Some(msg) = signal.try_recv()? {
            match msg {
                Sig::Answer { body } => {
                    peer.accept_answer(&body)?;
                    eprintln!("sinalização: answer aceito");
                    break;
                }
                Sig::Offer { .. } => {}
            }
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
        thread::sleep(Duration::from_millis(2));
    }
}

fn view(port: u16) -> Result<()> {
    init_win()?;
    let latest = Arc::new(Mutex::new(None));
    let received = Arc::new(Mutex::new(0u64));
    let frames = latest.clone();
    let count = received.clone();
    thread::spawn(move || {
        if let Err(err) = view_net(port, frames, count) {
            eprintln!("view: {err:#}");
        }
    });
    present::run(latest, received)
}

fn view_net(port: u16, latest: Arc<Mutex<Option<Vec<u8>>>>, received: Arc<Mutex<u64>>) -> Result<()> {
    let mut signal = Signal::connect(port)?;
    let mut peer = Peer::new()?;
    let offer = loop {
        if let Some(Sig::Offer { body }) = signal.try_recv()? {
            break body;
        }
        thread::sleep(Duration::from_millis(5));
    };
    let answer = peer.accept_offer(&offer)?;
    signal.send(&Sig::Answer { body: answer })?;
    eprintln!("view: answer enviado");
    let mut decoder = Decoder::new()?;
    let mut decoded = 0u64;
    let mut last_log = Instant::now();
    loop {
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
        let _ = signal.try_recv()?;
        thread::sleep(Duration::from_millis(2));
    }
}

fn init_win() -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
    }
    Ok(())
}

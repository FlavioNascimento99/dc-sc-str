use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::capture::Capture;
use crate::encode::Encoder;
use crate::meter::Meter;
use crate::rtc::Peer;
use crate::signal::{Sig, Signal};

const MAX_VIEWERS: usize = 4;
const LEVELS: [(u32, u32); 3] = [(2_500_000, 30), (1_500_000, 24), (800_000, 15)];

struct Slot {
    id: String,
    peer: Peer,
    answered: bool,
    retry_at: Option<Instant>,
}

pub fn pump(mut signal: Signal, mut encoder: Encoder, capture: Capture, stop: &AtomicBool) -> Result<()> {
    let mut slots: Vec<Slot> = Vec::new();
    let mut level = 0usize;
    let mut stable_since = Instant::now();
    let mut next_frame = Instant::now();
    let mut last_idr = Instant::now();
    let mut last_log = Instant::now();
    let mut meter = Meter::new();
    let mut sent = 0u64;

    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        while let Some(msg) = signal.try_recv()? {
            match msg {
                Sig::ViewerJoined { id } => {
                    if id.is_empty() || slots.iter().any(|slot| slot.id == id) || slots.len() >= MAX_VIEWERS {
                        continue;
                    }
                    open_slot(&mut signal, &mut slots, id)?;
                }
                Sig::ViewerLeft { id } => slots.retain(|slot| slot.id != id),
                Sig::Answer { body, from } => {
                    if let Some(id) = from {
                        if let Some(slot) = slots.iter_mut().find(|slot| slot.id == id) {
                            slot.peer.accept_answer(&body)?;
                            slot.answered = true;
                            slot.retry_at = None;
                            eprintln!("sala: answer de {id}");
                        }
                    }
                }
                Sig::Offer { .. } => {}
            }
        }

        let mut refresh = Vec::new();
        for slot in &mut slots {
            let _ = slot.peer.poll()?;
            if slot.peer.connected() {
                slot.retry_at = None;
            } else if slot.answered {
                let at = slot.retry_at.get_or_insert_with(|| Instant::now() + Duration::from_secs(3));
                if Instant::now() >= *at {
                    refresh.push(slot.id.clone());
                    *at = Instant::now() + Duration::from_secs(10);
                }
            }
            if slot.peer.take_keyframe_request() {
                encoder.force_keyframe();
            }
        }
        for id in refresh {
            slots.retain(|slot| slot.id != id);
            open_slot(&mut signal, &mut slots, id)?;
            level = (level + 1).min(LEVELS.len() - 1);
            let (bitrate, fps) = LEVELS[level];
            encoder.retarget(bitrate, fps);
            stable_since = Instant::now();
        }

        let connected = slots.iter().filter(|slot| slot.peer.connected()).count();
        if connected == slots.len() && !slots.is_empty() && stable_since.elapsed() >= Duration::from_secs(15) && level > 0 {
            level -= 1;
            let (bitrate, fps) = LEVELS[level];
            encoder.retarget(bitrate, fps);
            stable_since = Instant::now();
        }

        let frame = Duration::from_millis(1000 / encoder.fps() as u64);
        if connected > 0 && Instant::now() >= next_frame {
            next_frame = Instant::now() + frame;
            if last_idr.elapsed() >= Duration::from_secs(2) {
                encoder.force_keyframe();
                last_idr = Instant::now();
            }
            if let Some(nv12) = capture.take() {
                if let Some(au) = encoder.encode(&nv12)? {
                    for slot in &mut slots {
                        if slot.peer.connected() {
                            slot.peer.send_h264(&au)?;
                            sent += 1;
                        }
                    }
                }
            }
        }

        if last_log.elapsed() >= Duration::from_secs(1) {
            let (cpu, rss) = meter.sample();
            eprintln!(
                "host: espectadores {}/{} conectados {connected} enviados {sent} {} fps cpu {cpu:.0}% rss {} MB",
                slots.len(),
                MAX_VIEWERS,
                encoder.fps(),
                rss / (1024 * 1024)
            );
            last_log = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn open_slot(signal: &mut Signal, slots: &mut Vec<Slot>, id: String) -> Result<()> {
    if slots.len() >= MAX_VIEWERS {
        eprintln!("sala: limite de {MAX_VIEWERS} espectadores");
        return Ok(());
    }
    let mut peer = Peer::new()?;
    let offer = peer.create_offer()?;
    signal.send_to("offer", &offer, &id)?;
    eprintln!("sala: offer para {id}");
    slots.push(Slot {
        id,
        peer,
        answered: false,
        retry_at: None,
    });
    Ok(())
}

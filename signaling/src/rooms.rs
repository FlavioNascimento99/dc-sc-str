use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;

const CODE_LEN: usize = 6;
const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const MAX_VIEWERS: usize = 4;
const ROOM_TTL: Duration = Duration::from_secs(4 * 60 * 60);
const MAX_BODY: usize = 60_000;
const MAX_MISSES: u32 = 20;
const MISS_WINDOW: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerId {
    Host,
    Viewer(u64),
}

#[derive(Debug, PartialEq, Eq)]
pub enum RoomError {
    NotFound,
    Expired,
    HostTaken,
    Full,
    Forbidden,
    BadMessage,
    RateLimited,
}

struct Room {
    expires: Instant,
    host_user_id: String,
    host: Option<UnboundedSender<String>>,
    viewers: HashMap<u64, UnboundedSender<String>>,
    next_viewer: u64,
    offer: Option<String>,
    ice_from_host: VecDeque<String>,
}

pub struct Hub {
    rooms: HashMap<String, Room>,
    misses: HashMap<IpAddr, (u32, Instant)>,
}

#[derive(Deserialize)]
struct Incoming {
    kind: String,
    body: String,
    #[serde(default)]
    to: Option<String>,
}

impl Hub {
    pub fn new() -> Self {
        Self {
            rooms: HashMap::new(),
            misses: HashMap::new(),
        }
    }

    pub fn create(&mut self, now: Instant, host_user_id: String) -> String {
        self.sweep(now);
        loop {
            let code = random_code();
            if self.rooms.contains_key(&code) {
                continue;
            }
            self.rooms.insert(
                code.clone(),
                Room {
                    expires: now + ROOM_TTL,
                    host_user_id,
                    host: None,
                    viewers: HashMap::new(),
                    next_viewer: 1,
                    offer: None,
                    ice_from_host: VecDeque::new(),
                },
            );
            return code;
        }
    }

    pub fn ensure(&mut self, code: &str, now: Instant) -> Result<(), RoomError> {
        self.live_mut(code, now).map(|_| ())
    }

    pub fn join_host(
        &mut self,
        code: &str,
        now: Instant,
        user_id: &str,
        tx: UnboundedSender<String>,
    ) -> Result<(), RoomError> {
        let room = self.live_mut(code, now)?;
        if room.host_user_id != user_id {
            return Err(RoomError::Forbidden);
        }
        if room.host.is_some() {
            return Err(RoomError::HostTaken);
        }
        let already: Vec<u64> = room.viewers.keys().copied().collect();
        let _ = tx.send(joined("host", code));
        for id in already {
            let _ = tx.send(event("viewer_joined", id));
        }
        room.host = Some(tx);
        Ok(())
    }

    pub fn join_viewer(
        &mut self,
        code: &str,
        now: Instant,
        tx: UnboundedSender<String>,
    ) -> Result<u64, RoomError> {
        let room = self.live_mut(code, now)?;
        if room.viewers.len() >= MAX_VIEWERS {
            return Err(RoomError::Full);
        }
        let id = room.next_viewer;
        room.next_viewer += 1;
        let _ = tx.send(joined("viewer", code));
        if let Some(offer) = &room.offer {
            let _ = tx.send(offer.clone());
        }
        for ice in &room.ice_from_host {
            let _ = tx.send(ice.clone());
        }
        room.viewers.insert(id, tx);
        if let Some(host) = &room.host {
            let _ = host.send(event("viewer_joined", id));
        }
        Ok(id)
    }

    pub fn note_miss(&mut self, ip: IpAddr, now: Instant) -> Result<(), RoomError> {
        let entry = self.misses.entry(ip).or_insert((0, now));
        if now.duration_since(entry.1) > MISS_WINDOW {
            *entry = (0, now);
        }
        entry.0 += 1;
        if entry.0 > MAX_MISSES {
            Err(RoomError::RateLimited)
        } else {
            Err(RoomError::NotFound)
        }
    }

    pub fn miss_limited(&self, ip: IpAddr, now: Instant) -> bool {
        match self.misses.get(&ip) {
            Some((count, at)) if now.duration_since(*at) <= MISS_WINDOW && *count > MAX_MISSES => {
                true
            }
            _ => false,
        }
    }

    pub fn on_message(
        &mut self,
        code: &str,
        from: PeerId,
        now: Instant,
        raw: &str,
    ) -> Result<(), RoomError> {
        if raw.len() > MAX_BODY + 256 {
            return Err(RoomError::BadMessage);
        }
        let msg: Incoming = serde_json::from_str(raw).map_err(|_| RoomError::BadMessage)?;
        if msg.body.is_empty() || msg.body.len() > MAX_BODY {
            return Err(RoomError::BadMessage);
        }
        let room = self.live_mut(code, now)?;
        let outbound = serde_json::json!({
            "kind": msg.kind,
            "body": msg.body,
            "from": peer_label(from),
        })
        .to_string();
        match (from, msg.kind.as_str()) {
            (PeerId::Host, "offer") => {
                if send_to(&room.viewers, msg.to.as_deref(), &outbound) {
                    room.offer = Some(outbound);
                }
            }
            (PeerId::Host, "ice") => {
                if msg.to.is_none() {
                    if room.ice_from_host.len() == 32 {
                        room.ice_from_host.pop_front();
                    }
                    room.ice_from_host.push_back(outbound.clone());
                }
                send_to(&room.viewers, msg.to.as_deref(), &outbound);
            }
            (PeerId::Viewer(id), "answer" | "ice") => {
                let tagged = serde_json::json!({
                    "kind": msg.kind,
                    "body": msg.body,
                    "from": id.to_string(),
                })
                .to_string();
                let Some(host) = &room.host else {
                    return Err(RoomError::Forbidden);
                };
                let _ = host.send(tagged);
            }
            _ => return Err(RoomError::Forbidden),
        }
        Ok(())
    }

    pub fn leave(&mut self, code: &str, id: PeerId) {
        let Some(room) = self.rooms.get_mut(code) else {
            return;
        };
        match id {
            PeerId::Host => {
                let viewers = std::mem::take(&mut room.viewers);
                self.rooms.remove(code);
                let ended = r#"{"kind":"ended","message":"transmissão encerrada"}"#;
                for tx in viewers.values() {
                    let _ = tx.send(ended.into());
                }
            }
            PeerId::Viewer(viewer) => {
                room.viewers.remove(&viewer);
                if let Some(host) = &room.host {
                    let _ = host.send(event("viewer_left", viewer));
                }
            }
        }
    }

    fn live_mut(&mut self, code: &str, now: Instant) -> Result<&mut Room, RoomError> {
        self.sweep(now);
        let room = self.rooms.get_mut(code).ok_or(RoomError::NotFound)?;
        if now >= room.expires {
            self.rooms.remove(code);
            return Err(RoomError::Expired);
        }
        Ok(self.rooms.get_mut(code).expect("sala"))
    }

    fn sweep(&mut self, now: Instant) {
        self.rooms.retain(|_, room| now < room.expires);
        self.misses
            .retain(|_, (count, at)| now.duration_since(*at) <= MISS_WINDOW && *count > 0);
    }
}

fn joined(role: &str, code: &str) -> String {
    serde_json::json!({"kind": "joined", "role": role, "code": code}).to_string()
}

fn event(kind: &str, viewer: u64) -> String {
    serde_json::json!({"kind": kind, "from": viewer.to_string()}).to_string()
}

fn peer_label(id: PeerId) -> String {
    match id {
        PeerId::Host => "host".into(),
        PeerId::Viewer(id) => id.to_string(),
    }
}

fn fanout(viewers: &HashMap<u64, UnboundedSender<String>>, message: &str) {
    for tx in viewers.values() {
        let _ = tx.send(message.to_string());
    }
}

fn send_to(viewers: &HashMap<u64, UnboundedSender<String>>, to: Option<&str>, message: &str) -> bool {
    let Some(to) = to else {
        fanout(viewers, message);
        return true;
    };
    if let Ok(id) = to.parse::<u64>() {
        if let Some(tx) = viewers.get(&id) {
            let _ = tx.send(message.to_string());
        }
    }
    false
}

fn random_code() -> String {
    let mut bytes = [0u8; CODE_LEN];
    getrandom::getrandom(&mut bytes).expect("entropia");
    bytes
        .into_iter()
        .map(|byte| ALPHABET[(byte as usize) % ALPHABET.len()] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn pair() -> (mpsc::UnboundedSender<String>, mpsc::UnboundedReceiver<String>) {
        mpsc::unbounded_channel()
    }

    #[test]
    fn one_host_four_viewers_and_replay() {
        let mut hub = Hub::new();
        let now = Instant::now();
        let code = hub.create(now, "host".into());
        let (host_tx, mut host_rx) = pair();
        hub.join_host(&code, now, "host", host_tx).unwrap();
        assert_eq!(hub.join_host(&code, now, "host", pair().0), Err(RoomError::HostTaken));
        let _ = host_rx.try_recv();

        hub.on_message(
            &code,
            PeerId::Host,
            now,
            r#"{"kind":"offer","body":"sdp-offer"}"#,
        )
        .unwrap();

        let mut viewers = Vec::new();
        for _ in 0..4 {
            let (tx, rx) = pair();
            let id = hub.join_viewer(&code, now, tx).unwrap();
            viewers.push((id, rx));
        }
        assert_eq!(
            hub.join_viewer(&code, now, pair().0),
            Err(RoomError::Full)
        );

        let (_, mut first) = viewers.pop().unwrap();
        let joined = first.try_recv().unwrap();
        assert!(joined.contains("viewer"));
        let offer = first.try_recv().unwrap();
        assert!(offer.contains("sdp-offer"));
        assert!(offer.contains(r#""from":"host""#));
    }

    #[test]
    fn answer_reaches_only_the_host() {
        let mut hub = Hub::new();
        let now = Instant::now();
        let code = hub.create(now, "host".into());
        let (host_tx, mut host_rx) = pair();
        hub.join_host(&code, now, "host", host_tx).unwrap();
        let _ = host_rx.try_recv();
        let (tx, mut viewer_rx) = pair();
        let id = hub.join_viewer(&code, now, tx).unwrap();
        let _ = viewer_rx.try_recv();

        hub.on_message(
            &code,
            PeerId::Viewer(id),
            now,
            r#"{"kind":"answer","body":"sdp-answer"}"#,
        )
        .unwrap();
        let answer = host_rx.try_recv().unwrap();
        assert!(answer.contains("sdp-answer"));
        assert!(viewer_rx.try_recv().is_err());
    }

    #[test]
    fn targeted_offer_reaches_one_viewer() {
        let mut hub = Hub::new();
        let now = Instant::now();
        let code = hub.create(now, "host".into());
        let (host_tx, mut host_rx) = pair();
        hub.join_host(&code, now, "host", host_tx).unwrap();
        let _ = host_rx.try_recv();
        let (first_tx, mut first_rx) = pair();
        let (second_tx, mut second_rx) = pair();
        let first = hub.join_viewer(&code, now, first_tx).unwrap();
        let _ = hub.join_viewer(&code, now, second_tx).unwrap();
        while first_rx.try_recv().is_ok() {}
        while second_rx.try_recv().is_ok() {}
        hub.on_message(
            &code,
            PeerId::Host,
            now,
            &format!(r#"{{"kind":"offer","body":"só-o-primeiro","to":"{first}"}}"#),
        )
        .unwrap();
        assert!(first_rx.try_recv().unwrap().contains("só-o-primeiro"));
        assert!(second_rx.try_recv().is_err());
    }

    #[test]
    fn viewer_cannot_offer_and_expiry_drops_the_room() {
        let mut hub = Hub::new();
        let now = Instant::now();
        let code = hub.create(now, "host".into());
        let (tx, _rx) = pair();
        let id = hub.join_viewer(&code, now, tx).unwrap();
        assert_eq!(
            hub.on_message(&code, PeerId::Viewer(id), now, r#"{"kind":"offer","body":"x"}"#),
            Err(RoomError::Forbidden)
        );
        let later = now + ROOM_TTL;
        assert_eq!(
            hub.join_host(&code, later, "host", pair().0),
            Err(RoomError::Expired)
        );
    }

    #[test]
    fn host_leave_ends_the_room() {
        let mut hub = Hub::new();
        let now = Instant::now();
        let code = hub.create(now, "host".into());
        hub.join_host(&code, now, "host", pair().0).unwrap();
        let (tx, mut rx) = pair();
        hub.join_viewer(&code, now, tx).unwrap();
        let _ = rx.try_recv();
        hub.leave(&code, PeerId::Host);
        assert!(rx.try_recv().unwrap().contains("ended"));
        assert_eq!(
            hub.join_viewer(&code, now, pair().0),
            Err(RoomError::NotFound)
        );
    }

    #[test]
    fn guesses_are_limited_per_address() {
        let mut hub = Hub::new();
        let now = Instant::now();
        let ip = "203.0.113.8".parse().unwrap();
        for _ in 0..MAX_MISSES {
            assert_eq!(hub.note_miss(ip, now), Err(RoomError::NotFound));
        }
        assert_eq!(hub.note_miss(ip, now), Err(RoomError::RateLimited));
        assert!(hub.miss_limited(ip, now));
    }
}

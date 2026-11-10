//! WebRTC via str0m.
//!
//! O crate `datachannel` 0.16 expõe track H.264, mas `add_track_ex` não chama
//! `rtcSetH264Packetizer`. Sem isso o `send` não gera RTP. O str0m packetiza
//! Annex-B (RFC 6184) no próprio processo Rust.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::Instant;

use anyhow::{Context, Result};
use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::media::{Direction, MediaKind};
use str0m::net::{Protocol, Receive};
use str0m::media::{MediaTime, Mid};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};

pub struct Peer {
    rtc: Rtc,
    socket: UdpSocket,
    lan: Option<Ipv4Addr>,
    pending: Option<SdpPendingOffer>,
    mid: Option<Mid>,
    started: Instant,
    connected: bool,
    keyframe_requested: bool,
    received: u64,
}

pub enum Inbound {
    Video(Vec<u8>),
}

impl Peer {
    pub fn new() -> Result<Self> {
        let _ = str0m::crypto::from_feature_flags().install_process_default();
        let socket = UdpSocket::bind("0.0.0.0:0").context("bind UDP")?;
        socket.set_nonblocking(true)?;
        let mut rtc = Rtc::new(Instant::now());
        let port = socket.local_addr()?.port();
        let lan = default_route_v4().filter(|ip| *ip != Ipv4Addr::LOCALHOST);
        for addr in advertise(port) {
            let candidate = Candidate::host(addr, "udp").context("candidate host")?;
            if rtc.add_local_candidate(candidate).is_none() {
                anyhow::bail!("ice recusou o candidato {addr}");
            }
            eprintln!("ice: candidato {addr}");
        }
        Ok(Self {
            rtc,
            socket,
            lan,
            pending: None,
            mid: None,
            started: Instant::now(),
            connected: false,
            keyframe_requested: false,
            received: 0,
        })
    }

    pub fn create_offer(&mut self) -> Result<String> {
        let mut changes = self.rtc.sdp_api();
        let mid = changes.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
        self.mid = Some(mid);
        let (offer, pending) = changes.apply().context("offer sem mídia")?;
        self.pending = Some(pending);
        Ok(serde_json::to_string(&offer)?)
    }

    pub fn accept_offer(&mut self, body: &str) -> Result<String> {
        let offer: SdpOffer = serde_json::from_str(body)?;
        let answer = self.rtc.sdp_api().accept_offer(offer)?;
        self.drive()?;
        Ok(serde_json::to_string(&answer)?)
    }

    pub fn accept_answer(&mut self, body: &str) -> Result<()> {
        let answer: SdpAnswer = serde_json::from_str(body)?;
        let pending = self.pending.take().context("answer sem offer pendente")?;
        self.rtc.sdp_api().accept_answer(pending, answer)?;
        self.drive()?;
        Ok(())
    }

    pub fn connected(&self) -> bool {
        self.connected
    }

    pub fn take_keyframe_request(&mut self) -> bool {
        std::mem::take(&mut self.keyframe_requested)
    }

    pub fn received(&self) -> u64 {
        self.received
    }

    pub fn send_h264(&mut self, annex_b: &[u8]) -> Result<()> {
        let Some(mid) = self.mid else {
            return Ok(());
        };
        let Some(writer) = self.rtc.writer(mid) else {
            return Ok(());
        };
        let Some(params) = writer.payload_params().next() else {
            return Ok(());
        };
        let pt = params.pt();
        let ticks = self.started.elapsed().as_micros() as u64 * 90 / 1000;
        writer.write(
            pt,
            Instant::now(),
            MediaTime::from_90khz(ticks),
            annex_b.to_vec(),
        )?;
        self.drive()?;
        Ok(())
    }

    pub fn poll(&mut self) -> Result<Vec<Inbound>> {
        self.drive()
    }

    fn drive(&mut self) -> Result<Vec<Inbound>> {
        let mut inbound = Vec::new();
        let mut buf = [0u8; 2000];
        loop {
            let timeout = match self.rtc.poll_output()? {
                Output::Timeout(at) => at,
                Output::Transmit(send) => {
                    if let Err(err) = self.socket.send_to(&send.contents, send.destination) {
                        eprintln!("udp: envio para {} falhou: {err}", send.destination);
                    }
                    continue;
                }
                Output::Event(event) => {
                    self.on_event(event, &mut inbound);
                    continue;
                }
            };

            let wait = timeout.saturating_duration_since(Instant::now());
            if wait.is_zero() {
                self.rtc.handle_input(Input::Timeout(Instant::now()))?;
                continue;
            }

            self.socket
                .set_read_timeout(Some(wait.min(std::time::Duration::from_millis(5))))?;
            match self.socket.recv_from(&mut buf) {
                Ok((n, source)) => {
                    let destination = self.local_destination(source);
                    let received = Receive::new(Protocol::Udp, source, destination, &buf[..n])?;
                    self.rtc
                        .handle_input(Input::Receive(Instant::now(), received))?;
                }
                Err(err)
                    if err.kind() == std::io::ErrorKind::WouldBlock
                        || err.kind() == std::io::ErrorKind::TimedOut =>
                {
                    break;
                }
                Err(err) => return Err(err.into()),
            }
        }
        Ok(inbound)
    }

    fn local_destination(&self, source: SocketAddr) -> SocketAddr {
        let port = self.socket.local_addr().map(|addr| addr.port()).unwrap_or(0);
        let ip = match source.ip() {
            std::net::IpAddr::V4(ip) if ip.is_loopback() => Ipv4Addr::LOCALHOST,
            std::net::IpAddr::V4(_) => self.lan.unwrap_or(Ipv4Addr::LOCALHOST),
            std::net::IpAddr::V6(_) => Ipv4Addr::LOCALHOST,
        };
        SocketAddr::from((ip, port))
    }

    fn on_event(&mut self, event: Event, inbound: &mut Vec<Inbound>) {
        match event {
            Event::IceConnectionStateChange(state) => {
                eprintln!("ice: {state:?}");
                self.connected = matches!(
                    state,
                    IceConnectionState::Connected | IceConnectionState::Completed
                );
            }
            Event::MediaData(data) => {
                self.received += 1;
                inbound.push(Inbound::Video(data.data.to_vec()));
            }
            Event::KeyframeRequest(_) => {
                self.keyframe_requested = true;
            }
            _ => {}
        }
    }
}

fn advertise(port: u16) -> Vec<SocketAddr> {
    let mut addrs = vec![SocketAddr::from((Ipv4Addr::LOCALHOST, port))];
    if let Some(ip) = default_route_v4() {
        if ip != Ipv4Addr::LOCALHOST {
            addrs.push(SocketAddr::from((ip, port)));
        }
    }
    addrs
}

fn default_route_v4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    match socket.local_addr().ok()? {
        SocketAddr::V4(addr) => Some(*addr.ip()),
        _ => None,
    }
}

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tungstenite::client::IntoClientRequest;
use tungstenite::protocol::Message;
use tungstenite::WebSocket;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Sig {
    #[serde(rename = "offer")]
    Offer { body: String },
    #[serde(rename = "answer")]
    Answer { body: String, from: Option<String> },
    ViewerJoined { id: String },
    ViewerLeft { id: String },
}

pub struct Signal {
    ws: WebSocket<TcpStream>,
}

impl Signal {
    pub fn listen(port: u16) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port))
            .with_context(|| format!("bind 127.0.0.1:{port}"))?;
        eprintln!("sinalização: esperando o view em ws://127.0.0.1:{port}");
        let (stream, addr) = listener.accept()?;
        eprintln!("sinalização: view em {addr}");
        let ws = tungstenite::accept(stream).context("handshake do view")?;
        set_nonblocking(&ws)?;
        Ok(Self { ws })
    }

    pub fn connect(port: u16) -> Result<Self> {
        let stream = TcpStream::connect(("127.0.0.1", port))
            .with_context(|| format!("conexão em 127.0.0.1:{port}. Suba o host antes."))?;
        let url = format!("ws://127.0.0.1:{port}");
        let (ws, _) = tungstenite::client::client(url.as_str(), stream).context("handshake do host")?;
        set_nonblocking(&ws)?;
        Ok(Self { ws })
    }

    pub fn open_room(base: &str, token: &str) -> Result<(Self, String)> {
        let base = base.trim_end_matches('/');
        let body = http_send("POST", &format!("{base}/rooms"), "", Some(token))?;
        let created: RoomCreated = serde_json::from_str(&body).context("resposta de /rooms")?;
        let code = created.code.to_ascii_uppercase();
        let signal = connect_url(&ws_url(base, &code, "host")?, Some(token))?;
        Ok((signal, code))
    }

    pub fn join_room(base: &str, code: &str, token: &str) -> Result<Self> {
        let base = base.trim_end_matches('/');
        let code = code.trim().to_ascii_uppercase();
        connect_url(&ws_url(base, &code, "viewer")?, Some(token))
    }

    pub fn send(&mut self, msg: &Sig) -> Result<()> {
        let text = serde_json::to_string(msg)?;
        self.send_raw(&text)
    }

    pub fn send_to(&mut self, kind: &str, body: &str, to: &str) -> Result<()> {
        let text = serde_json::json!({ "kind": kind, "body": body, "to": to }).to_string();
        self.send_raw(&text)
    }

    fn send_raw(&mut self, text: &str) -> Result<()> {
        self.ws
            .send(Message::Text(text.to_string().into()))
            .context("envio da sinalização")?;
        Ok(())
    }

    pub fn try_recv(&mut self) -> Result<Option<Sig>> {
        match self.ws.read() {
            Ok(Message::Text(text)) => Ok(parse_incoming(text.as_str())?),
            Ok(Message::Close(_)) => anyhow::bail!("sinalização fechou"),
            Ok(_) => Ok(None),
            Err(tungstenite::Error::Io(err))
                if err.kind() == ErrorKind::WouldBlock || err.kind() == ErrorKind::TimedOut =>
            {
                Ok(None)
            }
            Err(err) => Err(err).context("leitura da sinalização"),
        }
    }
}

fn set_nonblocking(ws: &WebSocket<TcpStream>) -> Result<()> {
    ws.get_ref().set_nonblocking(true)?;
    Ok(())
}

#[derive(Deserialize)]
struct RoomCreated {
    code: String,
}

#[derive(Deserialize)]
struct Incoming {
    kind: String,
    body: Option<String>,
    message: Option<String>,
    from: Option<String>,
}

fn parse_incoming(text: &str) -> Result<Option<Sig>> {
    let msg: Incoming = serde_json::from_str(text).with_context(|| format!("sinalização: {text}"))?;
    match msg.kind.as_str() {
        "offer" => Ok(Some(Sig::Offer {
            body: msg.body.context("offer sem body")?,
        })),
        "answer" => Ok(Some(Sig::Answer {
            body: msg.body.context("answer sem body")?,
            from: msg.from,
        })),
        "viewer_joined" => Ok(Some(Sig::ViewerJoined {
            id: msg.from.unwrap_or_default(),
        })),
        "viewer_left" => Ok(Some(Sig::ViewerLeft {
            id: msg.from.unwrap_or_default(),
        })),
        "error" => anyhow::bail!(msg.message.unwrap_or_else(|| "erro de sinalização".into())),
        "ended" => anyhow::bail!(msg.message.unwrap_or_else(|| "sala encerrada".into())),
        _ => Ok(None),
    }
}

fn connect_url(url: &str, token: Option<&str>) -> Result<Signal> {
    let host = url
        .trim_start_matches("ws://")
        .split('/')
        .next()
        .context("url")?;
    let stream = TcpStream::connect(host).with_context(|| format!("conexão em {host}"))?;
    let mut request = url
        .into_client_request()
        .context("pedido da sala")?;
    if let Some(token) = token {
        let value = format!("Bearer {token}")
            .parse()
            .context("Authorization")?;
        request.headers_mut().insert("Authorization", value);
    }
    let (ws, _) = tungstenite::client::client(request, stream).context("handshake da sala")?;
    set_nonblocking(&ws)?;
    Ok(Signal { ws })
}

fn ws_url(base: &str, code: &str, role: &str) -> Result<String> {
    let http = base
        .strip_prefix("http://")
        .context("o servidor de sinalização usa http://host:porta")?;
    Ok(format!("ws://{http}/ws?code={code}&role={role}"))
}

pub fn http_send(method: &str, url: &str, body: &str, token: Option<&str>) -> Result<String> {
    let rest = url
        .strip_prefix("http://")
        .context("o servidor de sinalização usa http://host:porta")?;
    let (host, path) = rest.split_once('/').context("url sem caminho")?;
    let addr = if host.contains(':') {
        host.to_string()
    } else {
        format!("{host}:80")
    };
    let auth = token
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let mut stream = TcpStream::connect(&addr).with_context(|| format!("conexão em {addr}"))?;
    let request = format!(
        "{method} /{path} HTTP/1.1\r\nHost: {host}\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).context("envio HTTP")?;
    let mut response = String::new();
    stream.read_to_string(&mut response).context("resposta HTTP")?;
    let (head, payload) = response.split_once("\r\n\r\n").context("resposta HTTP")?;
    let status = head.lines().next().unwrap_or("");
    if status.contains(" 401 ") {
        anyhow::bail!("sessão recusada. Rode poc login --signal {url}");
    }
    if !status.contains(" 200 ") && !status.contains(" 201 ") && !status.contains(" 204 ") {
        anyhow::bail!("{status}");
    }
    Ok(payload.trim().to_string())
}

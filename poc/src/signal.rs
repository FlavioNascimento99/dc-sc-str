use std::io::ErrorKind;
use std::net::{TcpListener, TcpStream};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tungstenite::protocol::Message;
use tungstenite::WebSocket;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Sig {
    #[serde(rename = "offer")]
    Offer { body: String },
    #[serde(rename = "answer")]
    Answer { body: String },
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

    pub fn send(&mut self, msg: &Sig) -> Result<()> {
        let text = serde_json::to_string(msg)?;
        self.ws
            .send(Message::Text(text.into()))
            .context("envio da sinalização")?;
        Ok(())
    }

    pub fn try_recv(&mut self) -> Result<Option<Sig>> {
        match self.ws.read() {
            Ok(Message::Text(text)) => Ok(Some(serde_json::from_str(text.as_str())?)),
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

mod auth;
mod rooms;
mod server;

use std::net::SocketAddr;

#[tokio::main]
async fn main() {
    let port = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "8787".into())
        .parse::<u16>()
        .expect("porta");
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    eprintln!("sinalização: http://{addr}  ws://{addr}/ws");
    if std::env::var("DISCORD_CLIENT_ID").is_err()
        || std::env::var("DISCORD_CLIENT_SECRET").is_err()
        || std::env::var("PUBLIC_URL").is_err()
    {
        eprintln!("auth: defina DISCORD_CLIENT_ID, DISCORD_CLIENT_SECRET e PUBLIC_URL para o login");
    }
    if std::env::var("UPDATE_VERSION").is_err() || std::env::var("UPDATE_URL").is_err() {
        eprintln!("update: defina UPDATE_VERSION e UPDATE_URL para anunciar uma versão nova");
    }
    server::serve(addr).await.expect("servidor");
}

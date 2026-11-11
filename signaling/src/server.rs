use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{mpsc, Mutex};

use crate::auth::{exchange, Auth, AuthError, Identity};
use crate::rooms::{Hub, PeerId, RoomError};

#[derive(Clone)]
struct App {
    hub: Arc<Mutex<Hub>>,
    auth: Arc<Mutex<Auth>>,
}

#[derive(Deserialize)]
struct JoinQuery {
    code: String,
    role: String,
}

#[derive(Deserialize)]
struct StartQuery {
    local_port: u16,
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct TicketBody {
    ticket: String,
}

pub async fn serve(addr: SocketAddr) -> Result<(), std::io::Error> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        router().into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
}

fn router() -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/update", get(app_update))
        .route("/auth/discord", get(start_login))
        .route("/auth/callback", get(callback))
        .route("/auth/ticket", post(redeem))
        .route("/auth/me", get(me))
        .route("/auth/logout", post(logout))
        .route("/rooms", post(create_room))
        .route("/ws", get(join))
        .with_state(App {
            hub: Arc::new(Mutex::new(Hub::new())),
            auth: Arc::new(Mutex::new(Auth::from_env())),
        })
}

async fn app_update() -> impl IntoResponse {
    let version = std::env::var("UPDATE_VERSION").ok().filter(|value| !value.is_empty());
    let url = std::env::var("UPDATE_URL").ok().filter(|value| !value.is_empty());
    match (version, url) {
        (Some(version), Some(url)) => {
            (StatusCode::OK, Json(json!({ "version": version, "url": url }))).into_response()
        }
        _ => StatusCode::NO_CONTENT.into_response(),
    }
}

async fn start_login(
    State(app): State<App>,
    Query(query): Query<StartQuery>,
) -> Result<Redirect, StatusCode> {
    let url = app
        .auth
        .lock()
        .await
        .begin(query.local_port, Instant::now())
        .map_err(auth_status)?;
    Ok(Redirect::temporary(&url))
}

async fn callback(
    State(app): State<App>,
    Query(query): Query<CallbackQuery>,
) -> Result<Redirect, (StatusCode, String)> {
    if query.error.is_some() {
        return Err((StatusCode::BAD_REQUEST, "autorização recusada".into()));
    }
    let code = query.code.ok_or((StatusCode::BAD_REQUEST, "code ausente".into()))?;
    let state = query.state.ok_or((StatusCode::BAD_REQUEST, "state ausente".into()))?;
    let (config, pending) = app
        .auth
        .lock()
        .await
        .take_pending(&state, Instant::now())
        .map_err(|err| (auth_status(err), "state inválido".into()))?;
    let user = exchange(&config, &pending, &code)
        .await
        .map_err(|err| (auth_status(err), "falha ao falar com o Discord".into()))?;
    let url = app.auth.lock().await.issue(user, pending.local_port, Instant::now());
    Ok(Redirect::temporary(&url))
}

async fn redeem(
    State(app): State<App>,
    Json(body): Json<TicketBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let (token, user) = app
        .auth
        .lock()
        .await
        .redeem(&body.ticket, Instant::now())
        .map_err(auth_status)?;
    Ok(Json(session_json(&token, &user)))
}

async fn me(
    State(app): State<App>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let token = bearer(&headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let user = app
        .auth
        .lock()
        .await
        .session(&token, Instant::now())
        .map_err(auth_status)?;
    Ok(Json(user_json(&user)))
}

async fn logout(State(app): State<App>, headers: HeaderMap) -> Result<StatusCode, StatusCode> {
    let token = bearer(&headers).ok_or(StatusCode::UNAUTHORIZED)?;
    app.auth.lock().await.logout(&token);
    Ok(StatusCode::NO_CONTENT)
}

async fn create_room(
    State(app): State<App>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let user = require_user(&app, &headers).await?;
    let code = app.hub.lock().await.create(Instant::now(), user.id);
    Ok(Json(json!({ "code": code })))
}

async fn join(
    ws: WebSocketUpgrade,
    State(app): State<App>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(query): Query<JoinQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let user = require_user(&app, &headers).await?;
    let code = query.code.to_ascii_uppercase();
    if code.len() != 6 || !matches!(query.role.as_str(), "host" | "viewer") {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut hub = app.hub.lock().await;
    let now = Instant::now();
    if hub.miss_limited(peer.ip(), now) {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    if let Err(err) = hub.ensure(&code, now) {
        let limited = matches!(hub.note_miss(peer.ip(), now), Err(RoomError::RateLimited));
        return Err(if limited {
            StatusCode::TOO_MANY_REQUESTS
        } else {
            room_status(err)
        });
    }
    drop(hub);
    Ok(ws.on_upgrade(move |socket| session(socket, app, code, query.role, user.id)))
}

async fn require_user(app: &App, headers: &HeaderMap) -> Result<Identity, StatusCode> {
    let token = bearer(headers).ok_or(StatusCode::UNAUTHORIZED)?;
    app.auth
        .lock()
        .await
        .session(&token, Instant::now())
        .map_err(auth_status)
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(axum::http::header::AUTHORIZATION)?.to_str().ok()?;
    let token = value.strip_prefix("Bearer ")?.trim();
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

fn auth_status(err: AuthError) -> StatusCode {
    match err {
        AuthError::NotConfigured => StatusCode::SERVICE_UNAVAILABLE,
        AuthError::BadPort => StatusCode::BAD_REQUEST,
        AuthError::UnknownState | AuthError::UnknownTicket | AuthError::UnknownSession | AuthError::Expired => {
            StatusCode::UNAUTHORIZED
        }
        AuthError::Discord(_) => StatusCode::BAD_GATEWAY,
    }
}

fn room_status(err: RoomError) -> StatusCode {
    match err {
        RoomError::NotFound | RoomError::Expired => StatusCode::NOT_FOUND,
        RoomError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        RoomError::Full | RoomError::HostTaken => StatusCode::CONFLICT,
        RoomError::Forbidden | RoomError::BadMessage => StatusCode::BAD_REQUEST,
    }
}

fn session_json(token: &str, user: &Identity) -> serde_json::Value {
    json!({
        "token": token,
        "user": user_json(user),
    })
}

fn user_json(user: &Identity) -> serde_json::Value {
    json!({
        "id": user.id,
        "username": user.username,
        "global_name": user.global_name,
        "avatar": user.avatar,
    })
}

async fn session(mut socket: WebSocket, app: App, code: String, role: String, user_id: String) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let peer = {
        let mut hub = app.hub.lock().await;
        let now = Instant::now();
        let joined = if role == "host" {
            hub.join_host(&code, now, &user_id, tx).map(|()| PeerId::Host)
        } else {
            hub.join_viewer(&code, now, tx).map(PeerId::Viewer)
        };
        match joined {
            Ok(peer) => peer,
            Err(err) => {
                let message = match err {
                    RoomError::Full => "sala cheia",
                    RoomError::HostTaken => "a sala já tem transmissor",
                    RoomError::Expired => "sala expirada",
                    RoomError::Forbidden => "esta sala não é sua",
                    _ => "sala indisponível",
                };
                let _ = socket
                    .send(Message::Text(
                        json!({"kind": "error", "message": message}).to_string().into(),
                    ))
                    .await;
                return;
            }
        }
    };

    let (mut sink, mut stream) = socket.split();
    let mut outgoing = tokio::spawn(async move {
        while let Some(text) = rx.recv().await {
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    loop {
        let next = tokio::select! {
            incoming = stream.next() => incoming,
            _ = &mut outgoing => break,
        };
        let Some(Ok(message)) = next else { break };
        let Message::Text(text) = message else {
            if matches!(message, Message::Close(_)) {
                break;
            }
            continue;
        };
        let mut hub = app.hub.lock().await;
        if hub
            .on_message(&code, peer, Instant::now(), text.as_str())
            .is_err()
        {
            break;
        }
    }
    app.hub.lock().await.leave(&code, peer);
}

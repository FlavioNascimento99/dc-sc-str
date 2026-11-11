use std::collections::HashMap;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const STATE_TTL: Duration = Duration::from_secs(10 * 60);
const TICKET_TTL: Duration = Duration::from_secs(60);
const SESSION_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub id: String,
    pub username: String,
    pub global_name: Option<String>,
    pub avatar: Option<String>,
}

#[derive(Clone)]
pub struct DiscordConfig {
    pub client_id: String,
    pub client_secret: String,
    pub public_url: String,
}

pub struct Pending {
    pub verifier: String,
    pub local_port: u16,
    pub redirect_uri: String,
}

struct Ticket {
    token: String,
    expires: Instant,
}

struct Session {
    user: Identity,
    expires: Instant,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AuthError {
    NotConfigured,
    BadPort,
    UnknownState,
    Expired,
    UnknownTicket,
    UnknownSession,
    Discord(String),
}

pub struct Auth {
    config: Option<DiscordConfig>,
    states: HashMap<String, (Pending, Instant)>,
    tickets: HashMap<String, Ticket>,
    sessions: HashMap<String, Session>,
}

impl Auth {
    pub fn from_env() -> Self {
        let client_id = std::env::var("DISCORD_CLIENT_ID").ok().filter(|v| !v.is_empty());
        let client_secret = std::env::var("DISCORD_CLIENT_SECRET").ok().filter(|v| !v.is_empty());
        let public_url = std::env::var("PUBLIC_URL").ok().filter(|v| !v.is_empty());
        let config = match (client_id, client_secret, public_url) {
            (Some(client_id), Some(client_secret), Some(public_url)) => Some(DiscordConfig {
                client_id,
                client_secret,
                public_url: public_url.trim_end_matches('/').to_string(),
            }),
            _ => None,
        };
        Self {
            config,
            states: HashMap::new(),
            tickets: HashMap::new(),
            sessions: HashMap::new(),
        }
    }

    pub fn configured(&self) -> bool {
        self.config.is_some()
    }

    pub fn begin(&mut self, local_port: u16, now: Instant) -> Result<String, AuthError> {
        if local_port == 0 {
            return Err(AuthError::BadPort);
        }
        let config = self.config.clone().ok_or(AuthError::NotConfigured)?;
        self.sweep(now);
        let state = random_token();
        let verifier = random_token();
        let redirect_uri = format!("{}/auth/callback", config.public_url);
        let challenge = challenge(&verifier);
        self.states.insert(
            state.clone(),
            (
                Pending {
                    verifier,
                    local_port,
                    redirect_uri: redirect_uri.clone(),
                },
                now + STATE_TTL,
            ),
        );
        Ok(format!(
            "https://discord.com/oauth2/authorize?client_id={}&redirect_uri={}&response_type=code&scope=identify&state={}&code_challenge={}&code_challenge_method=S256",
            enc(&config.client_id),
            enc(&redirect_uri),
            enc(&state),
            enc(&challenge),
        ))
    }

    pub fn take_pending(&mut self, state: &str, now: Instant) -> Result<(DiscordConfig, Pending), AuthError> {
        self.sweep(now);
        let config = self.config.clone().ok_or(AuthError::NotConfigured)?;
        let (pending, expires) = self.states.remove(state).ok_or(AuthError::UnknownState)?;
        if now >= expires {
            return Err(AuthError::Expired);
        }
        Ok((config, pending))
    }

    pub fn issue(&mut self, user: Identity, local_port: u16, now: Instant) -> String {
        self.sweep(now);
        let token = random_token();
        let ticket = random_token();
        self.sessions.insert(
            token.clone(),
            Session {
                user,
                expires: now + SESSION_TTL,
            },
        );
        self.tickets.insert(
            ticket.clone(),
            Ticket {
                token,
                expires: now + TICKET_TTL,
            },
        );
        format!("http://127.0.0.1:{local_port}/callback?ticket={ticket}")
    }

    pub fn redeem(&mut self, ticket: &str, now: Instant) -> Result<(String, Identity), AuthError> {
        self.sweep(now);
        let ticket = self.tickets.remove(ticket).ok_or(AuthError::UnknownTicket)?;
        if now >= ticket.expires {
            return Err(AuthError::Expired);
        }
        let session = self.sessions.get(&ticket.token).ok_or(AuthError::UnknownSession)?;
        Ok((ticket.token, session.user.clone()))
    }

    pub fn session(&mut self, token: &str, now: Instant) -> Result<Identity, AuthError> {
        self.sweep(now);
        let session = self.sessions.get(token).ok_or(AuthError::UnknownSession)?;
        if now >= session.expires {
            self.sessions.remove(token);
            return Err(AuthError::Expired);
        }
        Ok(session.user.clone())
    }

    pub fn logout(&mut self, token: &str) {
        self.sessions.remove(token);
    }

    fn sweep(&mut self, now: Instant) {
        self.states.retain(|_, (_, expires)| now < *expires);
        self.tickets.retain(|_, ticket| now < ticket.expires);
        self.sessions.retain(|_, session| now < session.expires);
    }
}

pub async fn exchange(config: &DiscordConfig, pending: &Pending, code: &str) -> Result<Identity, AuthError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|err| AuthError::Discord(err.to_string()))?;
    let token = client
        .post("https://discord.com/api/oauth2/token")
        .form(&[
            ("client_id", config.client_id.as_str()),
            ("client_secret", config.client_secret.as_str()),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", pending.redirect_uri.as_str()),
            ("code_verifier", pending.verifier.as_str()),
        ])
        .send()
        .await
        .map_err(|err| AuthError::Discord(err.to_string()))?;
    if !token.status().is_success() {
        return Err(AuthError::Discord(format!("token {}", token.status())));
    }
    let token: DiscordToken = token
        .json()
        .await
        .map_err(|err| AuthError::Discord(err.to_string()))?;
    let user = client
        .get("https://discord.com/api/users/@me")
        .bearer_auth(token.access_token)
        .send()
        .await
        .map_err(|err| AuthError::Discord(err.to_string()))?;
    if !user.status().is_success() {
        return Err(AuthError::Discord(format!("usuário {}", user.status())));
    }
    let user: DiscordUser = user
        .json()
        .await
        .map_err(|err| AuthError::Discord(err.to_string()))?;
    Ok(Identity {
        id: user.id,
        username: user.username,
        global_name: user.global_name,
        avatar: user.avatar,
    })
}

#[derive(Deserialize)]
struct DiscordToken {
    access_token: String,
}

#[derive(Deserialize)]
struct DiscordUser {
    id: String,
    username: String,
    global_name: Option<String>,
    avatar: Option<String>,
}

fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("entropia");
    URL_SAFE_NO_PAD.encode(bytes)
}

fn enc(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth() -> Auth {
        let mut auth = Auth::from_env();
        auth.config = Some(DiscordConfig {
            client_id: "123".into(),
            client_secret: "secret".into(),
            public_url: "http://127.0.0.1:8787".into(),
        });
        auth
    }

    #[test]
    fn login_ticket_is_single_use_and_returns_the_user() {
        let mut auth = auth();
        let now = Instant::now();
        let url = auth.begin(43123, now).unwrap();
        assert!(url.contains("scope=identify"));
        assert!(url.contains("code_challenge_method=S256"));
        let state = url.split("state=").nth(1).unwrap().split('&').next().unwrap();
        let (_config, pending) = auth.take_pending(&state.to_string(), now).unwrap();
        assert!(auth.take_pending(state, now).is_err());
        let redirect = auth.issue(
            Identity {
                id: "42".into(),
                username: "flavio".into(),
                global_name: Some("Flavio".into()),
                avatar: None,
            },
            pending.local_port,
            now,
        );
        let ticket = redirect.split("ticket=").nth(1).unwrap();
        let (token, user) = auth.redeem(ticket, now).unwrap();
        assert_eq!(user.username, "flavio");
        assert!(auth.redeem(ticket, now).is_err());
        assert_eq!(auth.session(&token, now).unwrap().id, "42");
        auth.logout(&token);
        assert_eq!(auth.session(&token, now), Err(AuthError::UnknownSession));
    }

    #[test]
    fn zero_port_is_rejected() {
        let mut auth = auth();
        assert_eq!(auth.begin(0, Instant::now()), Err(AuthError::BadPort));
    }
}

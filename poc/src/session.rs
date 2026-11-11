use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use windows::core::PCWSTR;
use windows::Win32::Foundation::LocalFree;
use windows::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB};

#[derive(Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub username: String,
    pub global_name: Option<String>,
    pub avatar: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    token: String,
    user: User,
}

pub fn save(token: &str, user: User) -> Result<()> {
    let stored = serde_json::to_vec(&Stored {
        token: token.to_string(),
        user,
    })?;
    let protected = protect(&stored)?;
    let path = path()?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, protected)?;
    Ok(())
}

pub fn load() -> Result<(String, User)> {
    let bytes = fs::read(path()?).context("nenhuma sessão. Rode poc login --signal http://IP:8787")?;
    let plain = unprotect(&bytes)?;
    let stored: Stored = serde_json::from_slice(&plain)?;
    Ok((stored.token, stored.user))
}

pub fn clear() -> Result<()> {
    let path = path()?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn path() -> Result<PathBuf> {
    let root = std::env::var("LOCALAPPDATA").context("LOCALAPPDATA")?;
    Ok(PathBuf::from(root).join("discord-screen-companion").join("session.bin"))
}

fn protect(data: &[u8]) -> Result<Vec<u8>> {
    unsafe {
        let input = blob(data);
        let mut output = CRYPT_INTEGER_BLOB::default();
        CryptProtectData(
            &input,
            PCWSTR::null(),
            None,
            None,
            None,
            0,
            &mut output,
        )
        .context("CryptProtectData")?;
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(windows::Win32::Foundation::HLOCAL(output.pbData.cast()));
        Ok(bytes)
    }
}

fn unprotect(data: &[u8]) -> Result<Vec<u8>> {
    unsafe {
        let input = blob(data);
        let mut output = CRYPT_INTEGER_BLOB::default();
        CryptUnprotectData(&input, None, None, None, None, 0, &mut output).context("CryptUnprotectData")?;
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(windows::Win32::Foundation::HLOCAL(output.pbData.cast()));
        Ok(bytes)
    }
}

fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    }
}

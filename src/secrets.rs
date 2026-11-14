//! The Anthropic API key, stored in Windows Credential Manager (never in the
//! database). Falls back to the ANTHROPIC_API_KEY environment variable.

use anyhow::{Context as _, Result};

const SERVICE: &str = "monaged";
const ACCOUNT: &str = "anthropic-api-key";

fn entry() -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, ACCOUNT).context("Windows Credential Manager isn't available")
}

pub fn load_api_key() -> Option<String> {
    if let Some(key) = entry().ok().and_then(|e| e.get_password().ok()).filter(|k| !k.trim().is_empty()) {
        return Some(key);
    }
    std::env::var("ANTHROPIC_API_KEY").ok().filter(|k| !k.trim().is_empty())
}

pub fn save_api_key(key: &str) -> Result<()> {
    entry()?.set_password(key.trim()).context("couldn't save the key in Windows Credential Manager")
}

pub fn delete_api_key() -> Result<()> {
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(err) => Err(err).context("couldn't remove the key from Windows Credential Manager"),
    }
}

/// "sk-ant-api03-…AbCd": enough to recognise a key without revealing it.
pub fn mask(key: &str) -> String {
    let key = key.trim();
    let prefix: String = key.chars().take(10).collect();
    let suffix: String = key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{prefix}…{suffix}")
}

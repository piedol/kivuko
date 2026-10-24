//! The Nord access token, in the login keychain (service "Kivuko", account "NordVPN token").
//! The app hands it to the tunnel when you click Connect; the tunnel fetches the WireGuard
//! key with it.

use security_framework::passwords::{get_generic_password, set_generic_password};
use shared::ids::{KEYCHAIN_ACCOUNT, KEYCHAIN_SERVICE};

pub fn token() -> Option<String> {
    let bytes = get_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT).ok()?;
    let t = String::from_utf8(bytes).ok()?.trim().to_string();
    (!t.is_empty()).then_some(t)
}

pub fn set_token(token: &str) -> Result<(), String> {
    set_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT, token.trim().as_bytes()).map_err(|e| e.to_string())
}

pub fn has_token() -> bool {
    token().is_some()
}

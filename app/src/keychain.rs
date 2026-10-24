//! The Nord access token, in the login keychain (service "Kivuko", account "NordVPN token").
//! The token never touches disk or logs; the app uses it to fetch the WireGuard key.

use security_framework::item::{ItemClass, ItemSearchOptions};
use security_framework::passwords::{get_generic_password, set_generic_password};
use shared::ids::{KEYCHAIN_ACCOUNT, KEYCHAIN_SERVICE};

/// Whether a token is saved. Reads attributes only, so macOS doesn't ask for access.
pub fn has_token() -> bool {
    ItemSearchOptions::new()
        .class(ItemClass::generic_password())
        .service(KEYCHAIN_SERVICE)
        .account(KEYCHAIN_ACCOUNT)
        .load_attributes(true)
        .search()
        .is_ok_and(|r| !r.is_empty())
}

/// The token. The first read of a token saved outside the app makes macOS ask once
/// ("Always Allow" stops it asking again).
pub fn token() -> Option<String> {
    let bytes = get_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT).ok()?;
    let t = String::from_utf8(bytes).ok()?.trim().to_string();
    (!t.is_empty()).then_some(t)
}

pub fn set_token(token: &str) -> Result<(), String> {
    set_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT, token.trim().as_bytes()).map_err(|e| e.to_string())
}

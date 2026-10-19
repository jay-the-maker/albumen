// SPDX-License-Identifier: Apache-2.0

//! Opt-in password storage for the camera receiver.
//!
//! The entry name is `albumen-receive` and the account is the camera user.
//! macOS uses the Keychain, Windows uses Credential Manager, and Linux uses
//! the Secret Service. The password is never printed. A missing store is an
//! error only when the user asked for the keyring.

const SERVICE: &str = "albumen-receive";

pub fn load_password(user: &str) -> Result<String, String> {
    let entry = entry(user)?;
    entry
        .get_password()
        .map_err(|err| format!("keyring: {err}"))
}

pub fn store_password(user: &str, password: &str) -> Result<(), String> {
    if password.is_empty() {
        return Err("password must not be empty".to_string());
    }
    let entry = entry(user)?;
    entry
        .set_password(password)
        .map_err(|err| format!("keyring: {err}"))
}

fn entry(user: &str) -> Result<keyring::Entry, String> {
    if user.is_empty() {
        return Err("user must not be empty".to_string());
    }
    keyring::Entry::new(SERVICE, user).map_err(|err| format!("keyring: {err}"))
}

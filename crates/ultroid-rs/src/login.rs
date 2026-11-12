//! Interactive login for the user client (terminal).
//!
//! Port of the Python first-run login: phone number + Telegram login
//! code, then the 2FA password when the account has one. Codes and
//! passwords are read from stdin and never logged. The web setup server
//! (`ultroid-web`) provides the same flow over HTTP; this is the path
//! used when the binary runs directly with an unauthorised session.

use grammers_client::{
    Client, SignInError,
    client::{LoginToken, PasswordToken},
};
use std::io::{Write, stdin, stdout};
use ultroid_core::{Error, Result};

fn prompt(label: &str) -> Result<String> {
    print!("{label}");
    stdout().flush().map_err(Error::Io)?;
    let mut line = String::new();
    stdin().read_line(&mut line).map_err(Error::Io)?;
    Ok(line.trim().to_string())
}

/// Complete authorisation for `client` (user account).
///
/// `api_hash` comes from the config. If the session is already
/// authorised this is a no-op. On a fresh session it walks the
/// phone-code flow, including 2FA. Sign-up is NOT possible from a
/// third-party client — Telegram requires an official app for that.
pub async fn ensure_user_authorised(client: &Client, api_hash: &str) -> Result<()> {
    if client
        .is_authorized()
        .await
        .map_err(|e| Error::Telegram(e.to_string()))?
    {
        return Ok(());
    }
    println!("This session is not authorised yet.");
    println!(
        "(Tip: run the setup server instead — `ultroid-rs` with no API keys in the environment starts it — or complete login here.)"
    );
    let phone = prompt("Phone number (international format, e.g. +91...): ")?;
    if phone.is_empty() {
        return Err(Error::Config(
            "no phone number given; cannot authorise the user session".into(),
        ));
    }
    let token = client
        .request_login_code(&phone, api_hash)
        .await
        .map_err(|e| Error::Telegram(e.to_string()))?;
    finish_login(client, token).await
}

async fn finish_login(client: &Client, token: LoginToken) -> Result<()> {
    let code = prompt("Login code sent by Telegram: ")?;
    match client.sign_in(&token, &code).await {
        Ok(user) => {
            println!(
                "Signed in as {} (id {}).",
                user.first_name().unwrap_or("?"),
                user.id().bot_api_dialog_id_unchecked()
            );
            Ok(())
        }
        Err(SignInError::PasswordRequired(password_token)) => {
            finish_2fa(client, password_token).await
        }
        Err(SignInError::InvalidCode) => Err(Error::Config(
            "invalid login code; restart and try again".into(),
        )),
        Err(SignInError::SignUpRequired) => Err(Error::Config(
            "this phone number has no Telegram account; sign up in an official Telegram app first"
                .into(),
        )),
        Err(SignInError::InvalidPassword(_)) => Err(Error::Config(
            "invalid 2FA password; restart and try again".into(),
        )),
        Err(SignInError::Other(e)) => Err(Error::Telegram(e.to_string())),
    }
}

async fn finish_2fa(client: &Client, token: PasswordToken) -> Result<()> {
    let password = prompt("Two-step verification password: ")?;
    match client.check_password(token, password.into_bytes()).await {
        Ok(user) => {
            println!(
                "Signed in as {} (id {}).",
                user.first_name().unwrap_or("?"),
                user.id().bot_api_dialog_id_unchecked()
            );
            Ok(())
        }
        Err(SignInError::InvalidPassword(_)) => Err(Error::Config(
            "invalid 2FA password; restart and try again".into(),
        )),
        Err(SignInError::Other(e)) => Err(Error::Telegram(e.to_string())),
        Err(e) => Err(Error::Telegram(e.to_string())),
    }
}

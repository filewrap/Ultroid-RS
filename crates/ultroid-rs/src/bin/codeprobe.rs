//! One-off diagnostic: request a login code and print how Telegram
//! delivered it, then force a resend (auth.resendCode) and print the
//! new delivery type. Follows PHONE_MIGRATE (303) by reconnecting to
//! the account's home DC. Sends real MTProto calls - run only on request.

use grammers_client::{Client, InvocationError, sender::SenderPool};
use grammers_session::Session;
use grammers_session::storages::MemorySession;
use grammers_tl_types as tl;
use std::sync::Arc;

fn send_code(phone: &str, api_id: i32, api_hash: &str) -> tl::functions::auth::SendCode {
    tl::functions::auth::SendCode {
        phone_number: phone.to_string(),
        api_id,
        api_hash: api_hash.to_string(),
        settings: tl::types::CodeSettings {
            allow_flashcall: false,
            current_number: false,
            allow_app_hash: false,
            allow_missed_call: false,
            allow_firebase: false,
            logout_tokens: None,
            token: None,
            app_sandbox: None,
            unknown_number: false,
        }
        .into(),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let api_id: i32 = std::env::var("API_ID").expect("API_ID").parse().unwrap();
    let api_hash = std::env::var("API_HASH").expect("API_HASH");
    let phone = std::env::var("PHONE").expect("PHONE");

    let session = Arc::new(MemorySession::default());

    // Connect, following PHONE_MIGRATE (303) across DCs.
    let mut sent;
    let mut client;
    let mut runner_task;
    let mut migrated_from = None;
    loop {
        let SenderPool {
            runner,
            handle,
            updates: _,
        } = SenderPool::new(session.clone(), api_id);
        let c = Client::new(handle);
        let task = tokio::spawn(async move {
            runner.run().await;
        });
        sent = c
            .invoke(&send_code(&phone, api_id, &api_hash))
            .await;
        match &sent {
            Err(InvocationError::Rpc(err)) if err.code == 303 => {
                let new_dc = err.value.expect("migrate carries dc id") as i32;
                let old_dc = session.home_dc_id().expect("home dc");
                println!("MIGRATE: dc {old_dc} -> dc {new_dc}");
                migrated_from = Some(old_dc);
                session.set_home_dc_id(new_dc).await.expect("set home dc");
                drop(c);
                task.abort();
                continue;
            }
            _ => {
                client = c;
                runner_task = task;
                break;
            }
        }
    }
    let _ = migrated_from;

    match sent {
        Ok(tl::enums::auth::SentCode::Code(code)) => {
            println!("SENT_TYPE: {:?}", code.r#type);
            println!("NEXT_TYPE: {:?}", code.next_type);
            println!("TIMEOUT: {:?}", code.timeout);
            let r = client
                .invoke(&tl::functions::auth::ResendCode {
                    phone_number: phone,
                    phone_code_hash: code.phone_code_hash,
                    reason: None,
                })
                .await;
            match r {
                Ok(tl::enums::auth::SentCode::Code(c2)) => {
                    println!("RESEND_TYPE: {:?}", c2.r#type);
                    println!("RESEND_NEXT: {:?}", c2.next_type);
                    println!("RESEND_TIMEOUT: {:?}", c2.timeout);
                }
                Ok(other) => println!("RESEND_OTHER: {:?}", other),
                Err(e) => println!("RESEND_ERR: {e}"),
            }
        }
        Ok(other) => println!("SEND_OTHER: {:?}", other),
        Err(e) => println!("SEND_ERR: {e}"),
    }
    runner_task.abort();
}

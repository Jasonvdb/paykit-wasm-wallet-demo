//! **Real.** Reading back the Payment Receipts of the previous session from the sealed
//! stores, after a page reload.
//!
//! The simulated network lives in WebAssembly memory, so a reload ends it: the switches, and
//! the identities' signers, are gone, and payments cannot go on. The wallets' own state is
//! not gone: it is sealed on disk. `Paykit::load` opens an app from its store alone, without
//! a network call, and the wallet's Payment Receipts and the payee's records are read from
//! that state. To show that no network is involved, the apps are loaded with a transport that
//! has no routes at all (an empty `Router`): any request through it would fail to connect.

use std::sync::Arc;

use paykit_browser::clock::JsClock;
use paykit_browser::timer::JsTimer;
use paykit_sdk::rail::FakeRail;
use paykit_sdk::{AppEnv, PayeeStatus, Paykit, PaykitConfig, ReceiptsOf, RequestRef};
use pubky_switch_client::store::{StateStore, StoreHandle};
use pubky_switch_client::testing::{Router, TestHomeserver};
use serde::Serialize;

use crate::engine::{hex, ReceiptRow};
use crate::error::Failure;
use crate::ledger::sats_from_btc;
use crate::store::{self, ALICE_STORE, BOB_STORE};

/// What the previous session's stores held.
#[derive(Debug, Default, Serialize)]
pub struct PreviousSession {
    /// The Payment Receipts Alice's wallet stored, read back from its sealed store.
    pub alice_receipts: Vec<ReceiptRow>,
    /// How many of Bob's payee records read as receipted, read back from his sealed store.
    pub bob_receipted: usize,
}

/// Reads the previous session's Payment Receipts from the sealed stores, if they hold any.
///
/// Returns `Ok(None)` when there is no previous session (no store, or a store that holds no
/// app). Every store is closed again when it returns.
pub async fn read_back() -> Result<Option<PreviousSession>, Failure> {
    let alice_store = store::open_sealed(ALICE_STORE).await?;
    let bob_store = store::open_sealed(BOB_STORE).await?;
    let (Some(alice_id), Some(bob_id)) = (
        store::app_id(alice_store.as_ref())?,
        store::app_id(bob_store.as_ref())?,
    ) else {
        return Ok(None);
    };
    let alice = load(&alice_store, alice_id, "Alice's wallet").await?;
    let bob = load(&bob_store, bob_id, "Bob's wallet").await?;

    let mut previous = PreviousSession::default();
    let receipts = alice
        .receipts(ReceiptsOf::All)
        .await
        .map_err(|error| Failure::sdk("Alice's wallet lists its Payment Receipts", &error))?;
    for receipt in receipts {
        // A receipt names the request it pays; the amount is in the request.
        let amount_sats = match receipt.request_hash {
            Some(request_hash) => {
                let request = RequestRef {
                    payee: receipt.payee,
                    request_hash,
                };
                let view = alice.request(&request).await.map_err(|error| {
                    Failure::sdk("Alice's wallet reads the request paid", &error)
                })?;
                Some(sats_from_btc(&view.amount)?)
            }
            None => None,
        };
        previous.alice_receipts.push(ReceiptRow {
            request_id: receipt
                .request_hash
                .map(|hash| hex(&hash))
                .unwrap_or_default(),
            amount_sats,
            memo: receipt.payment_reference.clone().unwrap_or_default(),
            time_ms: receipt.accepted_at_ms,
            receipt_id: receipt.msg_id.clone(),
            proof_id: None,
        });
    }
    let sales = bob
        .payee_activity()
        .await
        .map_err(|error| Failure::sdk("Bob's wallet reads its payee records", &error))?;
    previous.bob_receipted = sales
        .iter()
        .filter(|sale| sale.status == PayeeStatus::Receipted)
        .count();
    // Dropping the apps and the last store handles closes the stores, so they can be destroyed.
    drop((alice, bob, alice_store, bob_store));
    Ok(Some(previous))
}

/// Loads app `app_id` from `store` alone: no route to any switch, an empty Homeserver, the
/// browser's clock and timer, and a fresh mock rail.
async fn load(store: &Arc<dyn StateStore>, app_id: [u8; 16], who: &str) -> Result<Paykit, Failure> {
    let env = AppEnv {
        store: StoreHandle::inline(Arc::clone(store)),
        transport: Arc::new(Router::new()),
        clock: Arc::new(JsClock),
        homeserver: Arc::new(TestHomeserver::new()),
        homeserver_writer: None,
    };
    Paykit::load(
        env,
        PaykitConfig::default(),
        app_id,
        Arc::new(FakeRail::new()),
        Arc::new(JsTimer),
    )
    .await
    .map_err(|error| Failure::sdk(&format!("load {who} from its sealed store"), &error))
}

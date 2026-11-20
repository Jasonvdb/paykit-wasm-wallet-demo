//! The engine: two Paykit wallets on one simulated network, and the operations the page
//! offers. Everything Paykit does here is the real SDK; the network and the rail are not
//! (see `world.rs` and `ledger.rs`).
//!
//! - Bob's wallet sends a one-time Payment Request to Alice (Paykit spec 8.1).
//! - Alice's wallet receives it and pays it: Paykit records the attempt, tells the rail to
//!   pay, and sends Bob a Payment Proof.
//! - Bob's wallet receives the Payment Proof, has the rail check it, and confirms the payment
//!   with a Payment Receipt, which Alice's wallet receives and stores with the request.

use std::collections::BTreeMap;
use std::future::Future;

use paykit_browser::clock::JsClock;
use paykit_sdk::rail::Behaviour;
use paykit_sdk::{
    MessageDelivery, PayOptions, PayOutcome, PayeeStatus, Paykit, PaymentEndpointIdentifier,
    PaymentEndpointPayload, PaymentReference, ReceiptsOf, RequestDestination, RequestDraft,
    RequestStatus,
};
use pubky_switch_client::clock::Clock;
use serde::Serialize;
use web_time::Instant;

use crate::error::Failure;
use crate::ledger::{btc_amount, sats_from_btc, Ledger};
use crate::previous::PreviousSession;
use crate::store::{self, StoreInfo, StoreMode, Stores};
use crate::world::World;

/// How long a Payment Request stays open: one day.
const REQUEST_LIFETIME_MS: u64 = 24 * 60 * 60 * 1000;
/// The Payment Endpoint Bob's requests ask to be paid to. The mock rail pays this identifier;
/// the payload is a placeholder, not an invoice.
const ENDPOINT_ID: &str = "btc-lightning-bolt11";
const ENDPOINT_PAYLOAD: &str = "mock-invoice-not-payable";

/// One timed step, for the page's log.
#[derive(Debug, Serialize)]
pub struct Step {
    /// What happened.
    pub step: String,
    /// How long it took, in milliseconds.
    pub ms: f64,
}

/// A Payment Request as the page lists it.
#[derive(Debug, Serialize)]
pub struct RequestRow {
    /// The request hash, in hex: the id the page passes back to [`Engine::pay`].
    pub request_id: String,
    /// The amount requested.
    pub amount_sats: u64,
    /// Bob's memo, sent as the request's Payment Reference.
    pub memo: String,
}

/// A Payment Receipt as the page lists it.
#[derive(Debug, Serialize)]
pub struct ReceiptRow {
    /// The request paid, in hex.
    pub request_id: String,
    /// The amount paid, when known.
    pub amount_sats: Option<u64>,
    /// The request's Payment Reference.
    pub memo: String,
    /// When the receipt was accepted or sent, in milliseconds since 1970, when known.
    pub time_ms: Option<u64>,
    /// The Payment Receipt's message ID, as the SDK gives it.
    pub receipt_id: String,
    /// The Payment Proof's message ID, as the SDK gives it, when this session paid.
    pub proof_id: Option<String>,
}

/// What [`Engine::start`] reports.
#[derive(Debug, Serialize)]
pub struct Started {
    /// Which store is in use, and why.
    pub store: StoreInfo,
    /// What the last session's sealed stores held, if there was one.
    pub previous: Option<PreviousSession>,
    /// Why the last session could not be read back, if it could not.
    pub previous_error: Option<String>,
    /// The steps, timed.
    pub steps: Vec<Step>,
}

/// What [`Engine::request_payment`] reports.
#[derive(Debug, Serialize)]
pub struct Requested {
    /// The request, as Alice's wallet received it.
    pub request: RequestRow,
    /// The steps, timed.
    pub steps: Vec<Step>,
}

/// What [`Engine::pay`] reports.
#[derive(Debug, Serialize)]
pub struct PaidReport {
    /// The Payment Receipt Alice's wallet stored.
    pub receipt: ReceiptRow,
    /// The steps, timed.
    pub steps: Vec<Step>,
}

/// Everything the page shows.
#[derive(Debug, Serialize)]
pub struct Snapshot {
    /// Alice's mock balance.
    pub alice_sats: u64,
    /// Bob's mock balance.
    pub bob_sats: u64,
    /// The Payment Requests Alice's wallet holds open.
    pub open_requests: Vec<RequestRow>,
    /// The Payment Receipts Alice's wallet stored.
    pub alice_receipts: Vec<ReceiptRow>,
    /// The Payment Receipts Bob's wallet sent.
    pub bob_receipts: Vec<ReceiptRow>,
    /// Which store is in use, and why.
    pub store: StoreInfo,
}

/// What Bob's wallet sent for one request: kept by the engine for the page, as the SDK's
/// payee records do not carry the memo or the receipt's message ID.
#[derive(Debug, Default)]
struct SentNote {
    memo: String,
    receipt_id: Option<String>,
    receipted_at_ms: Option<u64>,
}

/// Two wallets on one simulated network, their stores and their mock balances.
pub struct Engine {
    mode: StoreMode,
    store: StoreInfo,
    world: World,
    /// Alice's wallet: the payer.
    alice: Paykit,
    /// Bob's wallet: the payee.
    bob: Paykit,
    ledger: Ledger,
    /// Bob's notes, by request hash in hex.
    sent: BTreeMap<String, SentNote>,
    /// The Payment Proof's message ID of each payment Alice made, by request hash in hex.
    proofs: BTreeMap<String, String>,
}

impl Engine {
    /// Starts a new world: reads back the last session's sealed stores (in
    /// [`StoreMode::Sealed`]), opens fresh stores, and joins both wallets.
    pub async fn start(mode: StoreMode) -> Result<(Engine, Started), Failure> {
        let mut steps = Vec::new();
        let (previous, previous_error) = if mode == StoreMode::Sealed {
            match timed(
                &mut steps,
                "read back the last session",
                crate::previous::read_back(),
            )
            .await
            {
                Ok(previous) => (previous, None),
                // A failed read-back must not stop the demo: show why, and start afresh.
                Err(why) => (None, Some(why.to_string())),
            }
        } else {
            (None, None)
        };
        let Stores { alice, bob, info } =
            timed(&mut steps, "open fresh stores", store::open_fresh(mode)).await;

        let world = World::new();
        let alice_wallet = timed(
            &mut steps,
            "join Alice's wallet and invite Bob",
            world.join_alice(alice.clone()),
        )
        .await?;
        let bob_wallet =
            timed(&mut steps, "join Bob's wallet", world.join_bob(bob.clone())).await?;
        // Remember which app each store holds, for the read-back after a reload.
        store::keep_app_id(alice.as_ref(), alice_wallet.app_id())?;
        store::keep_app_id(bob.as_ref(), bob_wallet.app_id())?;

        let engine = Engine {
            mode,
            store: info.clone(),
            world,
            alice: alice_wallet,
            bob: bob_wallet,
            ledger: Ledger::new(),
            sent: BTreeMap::new(),
            proofs: BTreeMap::new(),
        };
        let started = Started {
            store: info,
            previous,
            previous_error,
            steps,
        };
        Ok((engine, started))
    }

    /// The store mode this engine was started with.
    pub fn mode(&self) -> StoreMode {
        self.mode
    }

    /// Bob's wallet sends Alice a one-time Payment Request for `amount_sats`, with `memo` as
    /// its Payment Reference, and Alice's wallet receives it.
    pub async fn request_payment(
        &mut self,
        amount_sats: u64,
        memo: &str,
    ) -> Result<Requested, Failure> {
        let mut steps = Vec::new();
        self.world.sync_clock();
        let reference = PaymentReference::new(memo).map_err(|_| {
            Failure::new("the memo must be 1 to 256 characters, with no control characters")
        })?;
        let endpoints = BTreeMap::from([(
            PaymentEndpointIdentifier::new(ENDPOINT_ID)
                .map_err(|_| Failure::new("the endpoint identifier is not valid"))?,
            PaymentEndpointPayload::new(ENDPOINT_PAYLOAD)
                .map_err(|_| Failure::new("the endpoint payload is not valid"))?,
        )]);
        let draft = RequestDraft::new(
            btc_amount(amount_sats)?,
            reference,
            RequestDestination::Endpoints(endpoints),
        )
        .with_proposal_expires_at_ms(JsClock.now_ms() + REQUEST_LIFETIME_MS);

        let alice_id = self.world.alice_id();
        let sent = timed(
            &mut steps,
            "Bob sends the Payment Request",
            self.bob.send_request(alice_id, draft),
        )
        .await
        .map_err(|error| Failure::sdk("Bob sends the Payment Request", &error))?;
        if !matches!(sent.delivery, MessageDelivery::Accepted { .. }) {
            return Err(Failure::new(
                "Alice's switch did not accept the Payment Request",
            ));
        }
        let request = sent
            .request
            .ok_or_else(|| Failure::new("Bob's wallet did not record the Payment Request"))?;
        timed(
            &mut steps,
            "Alice receives the Payment Request",
            self.alice.receive(None),
        )
        .await
        .map_err(|error| Failure::sdk("Alice receives the Payment Request", &error))?;
        let view = self
            .alice
            .request(&request)
            .await
            .map_err(|error| Failure::sdk("Alice reads the Payment Request", &error))?;
        let request_id = hex(&request.request_hash);
        self.sent.insert(
            request_id.clone(),
            SentNote {
                memo: memo.to_owned(),
                ..SentNote::default()
            },
        );
        Ok(Requested {
            request: RequestRow {
                request_id,
                amount_sats: sats_from_btc(&view.amount)?,
                memo: view.payment_reference,
            },
            steps,
        })
    }

    /// Alice's wallet pays the open Payment Request `request_id`; Bob's wallet receives the
    /// Payment Proof and sends a Payment Receipt, which Alice's wallet stores.
    pub async fn pay(&mut self, request_id: &str) -> Result<PaidReport, Failure> {
        let mut steps = Vec::new();
        self.world.sync_clock();
        let view = self
            .alice
            .requests()
            .await
            .map_err(|error| Failure::sdk("Alice lists her Payment Requests", &error))?
            .into_iter()
            .find(|view| hex(&view.request.request_hash) == request_id)
            .ok_or_else(|| Failure::new("Alice's wallet holds no such Payment Request"))?;
        if view.status != RequestStatus::Open {
            return Err(Failure::new("the Payment Request is not open"));
        }
        let request = view.request;
        let amount_sats = sats_from_btc(&view.amount)?;

        // The mock rail refuses when Alice's mock balance is too low. Paykit then records the
        // attempt as failed, as it would for a real rail that refused.
        let too_low = amount_sats > self.ledger.alice_sats;
        if too_low {
            self.world.rail.next(Behaviour::Refuse);
        }
        let outcome = timed(
            &mut steps,
            "Alice pays (attempt, mock rail, Payment Proof)",
            self.alice.pay_request(&request, PayOptions::default()),
        )
        .await
        .outcome
        .map_err(|error| Failure::sdk("Alice pays", &error))?;
        let (attempt_id, proof_id) = match outcome {
            PayOutcome::Paid {
                attempt_id,
                proof_msg_id,
                ..
            } => (attempt_id, proof_msg_id),
            PayOutcome::Failed { .. } if too_low => {
                return Err(Failure::new(
                    "the mock rail refused: Alice's mock balance is too low (Paykit recorded \
                     the attempt as failed)",
                ))
            }
            _ => return Err(Failure::new("the payment did not complete")),
        };
        // The mock balances move by what the mock rail says it paid.
        let paid = self
            .world
            .rail
            .paid_amount(&attempt_id)
            .ok_or_else(|| Failure::new("the mock rail has no record of the payment"))?;
        self.ledger.move_alice_to_bob(sats_from_btc(&paid)?)?;
        if let Some(proof_id) = &proof_id {
            self.proofs.insert(request_id.to_owned(), proof_id.clone());
        }

        timed(
            &mut steps,
            "Bob receives the Payment Proof",
            self.bob.receive(None),
        )
        .await
        .map_err(|error| Failure::sdk("Bob receives the Payment Proof", &error))?;
        let sale = self
            .bob
            .payee_activity()
            .await
            .map_err(|error| Failure::sdk("Bob reads his payee records", &error))?
            .into_iter()
            .find(|sale| sale.request_hash == Some(request.request_hash))
            .ok_or_else(|| Failure::new("Bob's wallet holds no record of the payment"))?;
        if sale.status != PayeeStatus::ProofReceived {
            return Err(Failure::new(
                "Bob's wallet did not receive the Payment Proof",
            ));
        }
        let receipt_id = timed(
            &mut steps,
            "Bob verifies the Payment Proof and sends a Payment Receipt",
            self.bob.confirm_payment(&sale.record),
        )
        .await
        .map_err(|error| Failure::sdk("Bob confirms the payment", &error))?
        .ok_or_else(|| Failure::new("the Payment Proof did not verify"))?;
        let note = self.sent.entry(request_id.to_owned()).or_default();
        note.receipt_id = Some(receipt_id);
        note.receipted_at_ms = Some(JsClock.now_ms());

        timed(
            &mut steps,
            "Alice receives the Payment Receipt",
            self.alice.receive(None),
        )
        .await
        .map_err(|error| Failure::sdk("Alice receives the Payment Receipt", &error))?;
        let view = self
            .alice
            .request(&request)
            .await
            .map_err(|error| Failure::sdk("Alice reads the request", &error))?;
        let receipt = view
            .receipt
            .ok_or_else(|| Failure::new("Alice's wallet did not store the Payment Receipt"))?;
        Ok(PaidReport {
            receipt: ReceiptRow {
                request_id: request_id.to_owned(),
                amount_sats: Some(amount_sats),
                memo: view.payment_reference,
                time_ms: receipt.accepted_at_ms,
                receipt_id: receipt.msg_id,
                proof_id,
            },
            steps,
        })
    }

    /// Everything the page shows, read from the wallets.
    pub async fn snapshot(&self) -> Result<Snapshot, Failure> {
        let requests = self
            .alice
            .requests()
            .await
            .map_err(|error| Failure::sdk("Alice lists her Payment Requests", &error))?;
        let mut open_requests = Vec::new();
        for view in &requests {
            if view.status == RequestStatus::Open {
                open_requests.push(RequestRow {
                    request_id: hex(&view.request.request_hash),
                    amount_sats: sats_from_btc(&view.amount)?,
                    memo: view.payment_reference.clone(),
                });
            }
        }

        let mut alice_receipts = Vec::new();
        let receipts = self
            .alice
            .receipts(ReceiptsOf::All)
            .await
            .map_err(|error| Failure::sdk("Alice lists her Payment Receipts", &error))?;
        for receipt in receipts {
            let request_id = receipt
                .request_hash
                .map(|hash| hex(&hash))
                .unwrap_or_default();
            let amount_sats = match requests
                .iter()
                .find(|view| Some(view.request.request_hash) == receipt.request_hash)
            {
                Some(view) => Some(sats_from_btc(&view.amount)?),
                None => None,
            };
            alice_receipts.push(ReceiptRow {
                proof_id: self.proofs.get(&request_id).cloned(),
                request_id,
                amount_sats,
                memo: receipt.payment_reference.unwrap_or_default(),
                time_ms: receipt.accepted_at_ms,
                receipt_id: receipt.msg_id,
            });
        }

        let mut bob_receipts = Vec::new();
        let sales = self
            .bob
            .payee_activity()
            .await
            .map_err(|error| Failure::sdk("Bob reads his payee records", &error))?;
        for sale in sales {
            if sale.status != PayeeStatus::Receipted {
                continue;
            }
            let request_id = sale.request_hash.map(|hash| hex(&hash)).unwrap_or_default();
            let note = self.sent.get(&request_id);
            bob_receipts.push(ReceiptRow {
                amount_sats: sale.amount.as_ref().map(sats_from_btc).transpose()?,
                memo: note.map(|note| note.memo.clone()).unwrap_or_default(),
                time_ms: note.and_then(|note| note.receipted_at_ms),
                receipt_id: note
                    .and_then(|note| note.receipt_id.clone())
                    .unwrap_or_default(),
                proof_id: None,
                request_id,
            });
        }

        Ok(Snapshot {
            alice_sats: self.ledger.alice_sats,
            bob_sats: self.ledger.bob_sats,
            open_requests,
            alice_receipts,
            bob_receipts,
            store: self.store.clone(),
        })
    }
}

/// Runs `future`, records how long it took as `step`, and returns its output.
async fn timed<T>(steps: &mut Vec<Step>, step: &str, future: impl Future<Output = T>) -> T {
    let started = Instant::now();
    let output = future.await;
    steps.push(Step {
        step: step.to_owned(),
        ms: started.elapsed().as_secs_f64() * 1000.0,
    });
    output
}

/// `bytes` in lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

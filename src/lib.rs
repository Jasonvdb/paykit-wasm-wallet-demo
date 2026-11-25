//! # The wallet engine
//!
//! Paykit v0.2 compiled to WebAssembly, behind five functions the page's worker calls:
//! [`start`], [`request_payment`], [`pay`], [`state`] and [`reset`]. Only plain data crosses
//! to JavaScript: numbers and strings in, a JSON string out, or an `Error` whose message says
//! which step failed.
//!
//! ## What is real and what is mocked
//!
//! - **Real:** the Paykit SDK (`paykit-sdk`), compiled to `wasm32-unknown-unknown`: joining a
//!   Paykit App, the Payment Request, paying it, the Payment Proof and the Payment Receipt,
//!   and reading Payment Receipts back from the sealed store after a reload. The sealed store
//!   (`store.rs`) is real too.
//! - **Simulated:** the network. The Pubky Dispatch services the wallets talk to are
//!   in-memory test Dispatches in this same WebAssembly instance (`world.rs`).
//! - **Mocked:** the payment rail and the balances (`ledger.rs`). Nothing of value moves.
//!
//! ## Why the engine runs in a dedicated worker
//!
//! - The sealed store needs synchronous file handles on the Origin Private File System, and
//!   only a dedicated worker has them.
//! - Paykit's calls can take a while (the sealed store writes on every commit), and a worker
//!   keeps the page responsive meanwhile.
//! - A Rust panic stops the whole WebAssembly instance for good (see [`start`]). In a worker
//!   the page survives it, and can start a new worker.
//! - The worker's script has its own Content Security Policy, the only one that allows
//!   `'wasm-unsafe-eval'`; the page's policy does not.
//!
//! ## How JavaScript promises and Rust futures meet
//!
//! Each exported function is an `async fn`. wasm-bindgen turns it into a function that
//! returns a JavaScript `Promise`, and `wasm-bindgen-futures` drives the Rust future on the
//! worker's own event loop (its microtask queue): there is no thread and no executor of our
//! own. Inside, the browser's own promises (OPFS file handles, IndexedDB, `setTimeout`) are
//! awaited as Rust futures through `wasm_bindgen_futures::JsFuture`.
//!
//! One wrinkle: the SDK's host traits (its `Timer`, the Dispatch client's transport and so
//! on) ask for futures that are `Send`, meaning safe to move to another thread, because
//! native apps run them on a thread pool. A future that awaits a JavaScript promise is not
//! `Send`. `paykit_browser::send::run_local` bridges the two without `unsafe` code: it runs
//! the browser future as a local task on this thread and hands the SDK a `Send` future that
//! only waits on a channel for the result. The engine uses it indirectly, through
//! `paykit_browser::timer::JsTimer`. The module docs of `paykit_browser::send` explain why
//! that is sound.
//!
//! ## Where the engine is kept between calls
//!
//! Between calls, the engine sits in a thread-local slot ([`ENGINE`]). Each call takes it out,
//! awaits its work, and puts it back. A `RefCell` borrow must never be held across an
//! `.await`: while the call waits, JavaScript can run and call in again, and the second
//! borrow would panic. Taking the engine out avoids that. A call that arrives while another
//! holds the engine gets a "busy" error instead; the worker sends one call at a time anyway.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod engine;
mod error;
mod ledger;
mod previous;
mod store;
mod world;

use std::cell::RefCell;

use serde::Serialize;
use wasm_bindgen::prelude::{wasm_bindgen, JsError};

use crate::engine::Engine;
use crate::error::Failure;
use crate::store::StoreMode;

thread_local! {
    /// The running engine, while no call holds it. `thread_local!` because wasm32 here is
    /// single-threaded, and a plain `static` would need a `Sync` type such as a `Mutex`.
    static ENGINE: RefCell<Option<Engine>> = const { RefCell::new(None) };
}

/// Starts the engine: a new simulated world with two wallets, Alice (who pays) and Bob (who
/// gets paid).
///
/// `store_mode` is `"sealed"`, for the sealed store on the Origin Private File System with a
/// fallback to memory, or `"memory"`. In `"sealed"` mode it first reads back the Payment
/// Receipts the previous session left in the sealed stores, then destroys those stores.
///
/// It also installs a panic hook that writes a Rust panic's message to the console. A panic
/// stops this WebAssembly instance for good: the page must then start a new worker.
///
/// Resolves to a JSON object: `{ store, previous, previous_error, steps }`.
///
/// # Errors
///
/// An `Error` naming the step that failed, for an unknown store mode, an engine already
/// started, or a wallet that could not join.
#[wasm_bindgen]
pub async fn start(store_mode: String) -> Result<String, JsError> {
    paykit_browser::panic::install_panic_hook();
    let result = async {
        let mode = StoreMode::parse(&store_mode)?;
        if ENGINE.with_borrow(Option::is_some) {
            return Err(Failure::new("the engine is already started: use reset"));
        }
        // `Box::pin` puts the large future on the heap: the SDK's futures nest deep, and a
        // future is built on the stack before it is first polled.
        let (engine, started) = Box::pin(Engine::start(mode)).await?;
        put(engine);
        Ok(started)
    }
    .await;
    respond(result)
}

/// Bob's wallet sends Alice's wallet a one-time Payment Request for `amount_sats`, with
/// `memo` as its Payment Reference, and Alice's wallet receives it.
///
/// Resolves to a JSON object: `{ request: { request_id, amount_sats, memo }, steps }`.
///
/// # Errors
///
/// An `Error` for an amount that is not a whole number of sats from 1 to 21 million bitcoin,
/// a memo that is empty or longer than 256 characters, or a step that failed.
#[wasm_bindgen(js_name = requestPayment)]
pub async fn request_payment(amount_sats: f64, memo: String) -> Result<String, JsError> {
    let result = async {
        let sats = ledger::sats_from_number(amount_sats)?;
        let mut engine = take()?;
        let result = Box::pin(engine.request_payment(sats, &memo)).await;
        put(engine);
        result
    }
    .await;
    respond(result)
}

/// Alice's wallet pays the open Payment Request `request_id` (its request hash in hex, as
/// [`request_payment`] and [`state`] give it) through the mock rail. Bob's wallet receives
/// the Payment Proof and confirms it with a Payment Receipt, which Alice's wallet stores.
///
/// Resolves to a JSON object: `{ receipt: { request_id, amount_sats, memo, time_ms,
/// receipt_id, proof_id }, steps }`.
///
/// # Errors
///
/// An `Error` for a request Alice does not hold open, a mock balance too low (the mock rail
/// refuses, and Paykit records the attempt as failed), or a step that failed.
#[wasm_bindgen]
pub async fn pay(request_id: String) -> Result<String, JsError> {
    let result = async {
        let mut engine = take()?;
        let result = Box::pin(engine.pay(&request_id)).await;
        put(engine);
        result
    }
    .await;
    respond(result)
}

/// Everything the page shows: the mock balances, the open Payment Requests, both wallets'
/// Payment Receipts, and the store in use.
///
/// Resolves to a JSON object: `{ alice_sats, bob_sats, open_requests, alice_receipts,
/// bob_receipts, store }`.
///
/// # Errors
///
/// An `Error` when the engine is not started, or a wallet could not be read.
#[wasm_bindgen]
pub async fn state() -> Result<String, JsError> {
    let result = async {
        let engine = take()?;
        let result = Box::pin(engine.snapshot()).await;
        put(engine);
        result
    }
    .await;
    respond(result)
}

/// Ends the current world and starts a new one, in the same store mode. In `"sealed"` mode
/// the read-back runs first, so the payments just made show as the previous session's.
///
/// Resolves to the same JSON object as [`start`].
///
/// # Errors
///
/// An `Error` when the engine is not started, or the new world could not start. The engine
/// is then stopped, and [`start`] may be called again.
#[wasm_bindgen]
pub async fn reset() -> Result<String, JsError> {
    let result = async {
        let engine = take()?;
        let mode = engine.mode();
        // Dropping the engine drops both wallets and their stores, which closes the store
        // files so the new world can destroy them.
        drop(engine);
        let (engine, started) = Box::pin(Engine::start(mode)).await?;
        put(engine);
        Ok(started)
    }
    .await;
    respond(result)
}

/// Takes the engine out of its slot.
fn take() -> Result<Engine, Failure> {
    ENGINE
        .take()
        .ok_or_else(|| Failure::new("the engine is not started, or is busy with another call"))
}

/// Puts the engine back in its slot.
fn put(engine: Engine) {
    ENGINE.set(Some(engine));
}

/// A result as the page gets it: a JSON string, or an `Error` with the failure's text.
fn respond<T: Serialize>(result: Result<T, Failure>) -> Result<String, JsError> {
    let value = result.map_err(|failure| JsError::new(&failure.to_string()))?;
    serde_json::to_string(&value).map_err(|_| JsError::new("the result could not be encoded"))
}

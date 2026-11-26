//! **Simulated.** The network the two wallets talk over: an in-memory Pubky Dispatch world
//! inside this same WebAssembly instance.
//!
//! A real wallet joins a Paykit App to its identity's Dispatch service over HTTPS. Here,
//! the Dispatch client's test module (`pubky_dispatch_client::testing`) stands in for all
//! of that:
//!
//! - each identity (Alice, Bob) has a `TestSigner`, which approves its app's join the way
//!   the identity's signer would, and a `ScriptedDispatch`, an in-memory Dispatch at a
//!   made-up origin;
//! - a `Router` is the "network": it hands each request to the Dispatch at its origin;
//! - a `TestHomeserver` holds the identities' public documents in memory;
//! - a `TestClock` is the Dispatches' clock. The engine keeps it at the browser's wall
//!   clock ([`World::sync_clock`]), so the times the Dispatches stamp are real times.
//!
//! The keys are derived from a seed made from the start time: test keys with no value. The
//! code that runs *on top* of this world is the real SDK, the same as on a phone.

use std::sync::Arc;

use paykit_browser::clock::JsClock;
use paykit_browser::timer::JsTimer;
use paykit_lib::scopes::{keyed_scopes, PAYKIT_SCOPE_NAMES};
use paykit_sdk::rail::FakeRail;
use paykit_sdk::{AppScopes, Paykit, PaykitConfig, RequestedScopes};
use pubky_dispatch_client::clock::{Clock, TestClock};
use pubky_dispatch_client::store::StateStore;
use pubky_dispatch_client::testing::{
    AppSpec, ProtocolSpec, Router, Seeds, TestHomeserver, TestIdentity, TestSigner,
};

use crate::error::Failure;

/// Where Alice's simulated Dispatch is. No request ever leaves the WebAssembly instance.
const ALICE_DISPATCH: &str = "https://alice-dispatch.example";
/// Where Bob's simulated Dispatch is.
const BOB_DISPATCH: &str = "https://bob-dispatch.example";

/// The simulated network, the two identities on it, and the mock payment rail.
pub struct World {
    clock: Arc<TestClock>,
    alice: TestSigner,
    bob: TestSigner,
    router: Arc<Router>,
    /// The mock payment rail both wallets use: Alice's wallet pays through it, and Bob's
    /// checks the Payment Proof it made.
    pub rail: Arc<FakeRail>,
}

impl World {
    /// A new world, with identities no earlier world had.
    pub fn new() -> Self {
        let now = JsClock.now_ms();
        let clock = Arc::new(TestClock::new(now));
        // A different seed for every world, so each one starts with new identities.
        let seeds = Seeds::new(&format!("paykit-wasm-wallet-demo/{now}/"));
        let homeserver = Arc::new(TestHomeserver::new());
        // The scopes Paykit gives keys to (Paykit spec 3.5), so the signer's approval carries
        // the keys each app's scopes need.
        let spec = ProtocolSpec {
            keyed_scopes: keyed_scopes(&PAYKIT_SCOPE_NAMES.map(str::to_owned)),
            ..ProtocolSpec::default()
        };
        let signer = |name: &str, origin: &str| {
            TestSigner::at(
                TestIdentity::new(&seeds, name),
                origin,
                Arc::clone(&clock),
                Arc::clone(&homeserver),
                spec.clone(),
            )
        };
        let alice = signer("alice", ALICE_DISPATCH);
        let bob = signer("bob", BOB_DISPATCH);
        let router = Arc::new(Router::new());
        router.add_dispatch(alice.origin(), alice.dispatch());
        router.add_dispatch(bob.origin(), bob.dispatch());
        // The rail stamps each payment's time from the same clock as the Dispatches.
        let rail = Arc::new(FakeRail::with_clock(Arc::clone(&clock) as Arc<dyn Clock>));
        Self {
            clock,
            alice,
            bob,
            router,
            rail,
        }
    }

    /// Moves the Dispatches' clock to the browser's wall clock, never backwards.
    pub fn sync_clock(&self) {
        let now = JsClock.now_ms();
        if now > self.clock.now_ms() {
            self.clock.set_ms(now);
        }
    }

    /// Alice's identity, as 32 public key bytes.
    pub fn alice_id(&self) -> [u8; 32] {
        self.alice.identity().public()
    }

    /// Joins Alice's wallet, which may pay (`paykit_pay`), on `store`, and invites Bob, so
    /// that Bob's Payment Requests are admitted (Paykit spec 1).
    pub async fn join_alice(&self, store: Arc<dyn StateStore>) -> Result<Paykit, Failure> {
        let scopes = AppScopes::new()
            .pay()
            .build()
            .map_err(|error| Failure::sdk("Alice's wallet asks for its scopes", &error))?;
        let mut wallet = self
            .join(&self.alice, "alice-wallet", scopes, store)
            .await?;
        let bob = self.bob.identity().public();
        wallet
            .app_mut()
            .invite(&bob)
            .await
            .map_err(|error| Failure::sdk("Alice's wallet invites Bob", &error.into()))?;
        Ok(wallet)
    }

    /// Joins Bob's wallet, which may send Payment Requests (`paykit_request`), on `store`.
    pub async fn join_bob(&self, store: Arc<dyn StateStore>) -> Result<Paykit, Failure> {
        let scopes = AppScopes::new()
            .request()
            .build()
            .map_err(|error| Failure::sdk("Bob's wallet asks for its scopes", &error))?;
        self.join(&self.bob, "bob-wallet", scopes, store).await
    }

    /// Joins app `name` of `signer` with `scopes` on `store`, approved by the signer, and
    /// finishes the join with a first receive.
    async fn join(
        &self,
        signer: &TestSigner,
        name: &str,
        scopes: RequestedScopes,
        store: Arc<dyn StateStore>,
    ) -> Result<Paykit, Failure> {
        let keys = signer.identity().app_keys(name);
        let names: Vec<&str> = scopes.scopes.iter().map(String::as_str).collect();
        let spec = AppSpec::new(&names).with_permissions(&scopes.permissions);
        let request = signer
            .approve(&keys, &spec)
            .map_err(|_| Failure::new(format!("the signer approves {name}")))?;
        let env = signer.env_with(store, Arc::clone(&self.router) as Arc<_>);
        let rail = Arc::clone(&self.rail) as Arc<_>;
        let mut app = Paykit::join(
            env,
            PaykitConfig::default(),
            request,
            rail,
            Arc::new(JsTimer),
        )
        .await
        .map_err(|error| Failure::sdk(&format!("{name} joins"), &error))?;
        app.receive(None)
            .await
            .map_err(|error| Failure::sdk(&format!("{name} receives"), &error))?;
        Ok(app)
    }
}

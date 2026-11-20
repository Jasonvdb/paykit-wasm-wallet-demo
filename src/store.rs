//! **Real.** Where each wallet keeps its Paykit state: the sealed store on the browser's
//! Origin Private File System, or memory.
//!
//! Paykit's state is signed messages, private records and app keys, so it is sensitive.
//! `paykit_browser::store::OpfsStore` seals every snapshot it writes (XChaCha20-Poly1305
//! under a random data key), so the files on disk hold no plaintext. The data key itself is
//! kept in IndexedDB, wrapped under a non-extractable WebCrypto key (`WrappingKey::Origin`).
//! Sealing protects the files at rest, for example against a tool that reads the browser's
//! storage folder; it does not protect against script running in this origin, which can do
//! anything the wallet can. paykit-browser's README ("Durable store") lists the limits.
//!
//! `OpfsStore` works only in a dedicated worker: the Switch client calls its store
//! synchronously, and only a dedicated worker has synchronous file handles
//! (`FileSystemSyncAccessHandle`). That is one reason the engine lives in a worker.
//!
//! Each wallet has one store, under a fixed name. A new world destroys the stores of the last
//! one and opens fresh ones; before that, the engine reads back the last session's Payment
//! Receipts from them (see `previous.rs`).

use std::sync::Arc;

use paykit_browser::store::{OpfsStore, WrappingKey};
use pubky_switch_client::store::{MemoryStore, StateStore, WriteBatch};
use serde::Serialize;

use crate::error::Failure;

/// The name of Alice's store. A store name is kept in the clear by the browser, so it names
/// the role, never an identity or an App ID.
pub const ALICE_STORE: &str = "demo-alice";
/// The name of Bob's store.
pub const BOB_STORE: &str = "demo-bob";

/// The engine's own namespace in each store. The Switch client and the SDK write only their
/// own namespaces.
const HOST_NAMESPACE: &str = "demo";
/// The key, in [`HOST_NAMESPACE`], of the App ID of the app whose state the store keeps.
const APP_ID_KEY: &[u8] = b"app_id";

/// Where the wallets keep their state, as the page asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreMode {
    /// The sealed store on the Origin Private File System, falling back to memory.
    Sealed,
    /// Memory only (`?store=memory` on the page).
    Memory,
}

impl StoreMode {
    /// The mode named `name`: `sealed` or `memory`.
    pub fn parse(name: &str) -> Result<Self, Failure> {
        match name {
            "sealed" => Ok(StoreMode::Sealed),
            "memory" => Ok(StoreMode::Memory),
            _ => Err(Failure::new(
                "the store mode must be \"sealed\" or \"memory\"",
            )),
        }
    }
}

/// Which store is in use, and why, as the page shows it.
#[derive(Clone, Debug, Serialize)]
pub struct StoreInfo {
    /// `sealed-opfs` or `memory`.
    pub kind: &'static str,
    /// Why this kind is in use.
    pub reason: String,
}

/// The two wallets' stores.
pub struct Stores {
    /// Alice's store.
    pub alice: Arc<dyn StateStore>,
    /// Bob's store.
    pub bob: Arc<dyn StateStore>,
    /// Which kind they are.
    pub info: StoreInfo,
}

/// Opens fresh, empty stores for a new world, as `mode` asks.
///
/// In [`StoreMode::Sealed`], it destroys whatever the last world left under the store names,
/// then opens sealed stores there. If that fails, for example in an engine with no Origin
/// Private File System (Playwright's WebKit on Linux) or where the browser refuses storage,
/// both wallets fall back to memory, and [`StoreInfo::reason`] says why.
pub async fn open_fresh(mode: StoreMode) -> Stores {
    if mode == StoreMode::Memory {
        return memory_stores("the page asked for the in-memory store (?store=memory)".into());
    }
    match open_sealed_pair().await {
        Ok((alice, bob)) => Stores {
            alice,
            bob,
            info: StoreInfo {
                kind: "sealed-opfs",
                reason: "sealed store on the Origin Private File System, key held by the origin"
                    .into(),
            },
        },
        Err(why) => memory_stores(format!("the sealed store could not be opened ({why})")),
    }
}

/// Destroys both wallets' sealed stores, their key records first. A store that does not
/// exist is not an error. Every handle to them must be dropped first.
pub async fn destroy_sealed() -> Result<(), Failure> {
    for name in [ALICE_STORE, BOB_STORE] {
        OpfsStore::destroy(name)
            .await
            .map_err(|error| Failure::new(format!("destroy the store {name}: {error}")))?;
    }
    Ok(())
}

/// Opens the sealed store `name` as it is, with the origin-held key.
pub async fn open_sealed(name: &str) -> Result<Arc<dyn StateStore>, Failure> {
    let store = OpfsStore::open(name, WrappingKey::Origin)
        .await
        .map_err(|error| Failure::new(format!("open the store {name}: {error}")))?;
    Ok(Arc::new(store))
}

/// The App ID `store` holds in the engine's namespace, if any.
pub fn app_id(store: &dyn StateStore) -> Result<Option<[u8; 16]>, Failure> {
    let held = store
        .get(HOST_NAMESPACE, APP_ID_KEY)
        .map_err(|error| Failure::new(format!("read the App ID: {error}")))?;
    // `Option<Result<..>>` to `Result<Option<..>>`: no App ID is fine, a bad one is an error.
    held.map(|bytes| {
        <[u8; 16]>::try_from(bytes.as_slice())
            .map_err(|_| Failure::new("the stored App ID is not 16 bytes"))
    })
    .transpose()
}

/// Keeps `app_id` in `store`, in the engine's namespace, so that a later session knows which
/// app's state the store holds and can load it with `Paykit::load`.
pub fn keep_app_id(store: &dyn StateStore, app_id: [u8; 16]) -> Result<(), Failure> {
    let mut batch = WriteBatch::new();
    batch
        .put(HOST_NAMESPACE, APP_ID_KEY.to_vec(), app_id.to_vec())
        .map_err(|_| Failure::new("batch the App ID"))?;
    store
        .commit(batch)
        .map_err(|error| Failure::new(format!("keep the App ID: {error}")))
}

/// Destroys what the last world left, then opens both sealed stores. On a failure after
/// Alice's store opened, it is dropped (closed) before the error returns.
async fn open_sealed_pair() -> Result<(Arc<dyn StateStore>, Arc<dyn StateStore>), Failure> {
    destroy_sealed().await?;
    let alice = open_sealed(ALICE_STORE).await?;
    let bob = open_sealed(BOB_STORE).await?;
    Ok((alice, bob))
}

/// Two in-memory stores. Nothing in them outlives the worker.
fn memory_stores(reason: String) -> Stores {
    Stores {
        alice: Arc::new(MemoryStore::new()),
        bob: Arc::new(MemoryStore::new()),
        info: StoreInfo {
            kind: "memory",
            reason,
        },
    }
}

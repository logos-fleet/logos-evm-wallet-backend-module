//! Logos module glue for `wallet_backend_module` (rust-first authoring) — the
//! coordinator.
//!
//! Depends on `eth_rpc_module`, `keystore_module`, and `token_list_module`
//! (declared in metadata.json `dependencies`), reached as
//! `modules().<dep>.<method>(...)`. Owns the central proxy + chain config and
//! pushes each chain's `{ endpoint, proxy, proxyRequired }` down into eth_rpc.
//! Multi-chain balances are fetched with Multicall3 (one `eth_call` per chain);
//! sends are built (alloy) → signed (keystore) → broadcast (eth_rpc) → recorded.
//!
//! Compiled only with the default `logos_module` feature; the pure cores
//! (`txbuild`, `config`, `history`, `jobs`) are tested with
//! `cargo test --no-default-features`.
//!
//! ## Two spellings for every method that leaves the process
//!
//! A `web` (wasm) image has one Worker and one event loop and cannot wait for a
//! reply (ADR 0004), so every outbound call site here is written once as a
//! `Flow` — a state machine over `Call` values — and driven two ways. See the
//! block below `include!` for why the native path keeps its SYNCHRONOUS clients
//! rather than following `uniswap_module` onto "dispatch and wait".
//!
//!   `<method>`         answers directly, having waited. NATIVE ONLY; on a `web`
//!                      image it dispatches nothing and names its twin.
//!   `start_<method>`   answers `{ok, jobId}` at once. Works everywhere, and it
//!                      is the spelling a `web` image must use.
//!   `take_result(id)`  collects a `start_*` job, once.

use alloy::primitives::{Address, U256};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc; // `Mutex` is already in scope from the generated provider glue.

use crate::config::{ChainInfo, ConfigStore, ProxySettings};
use crate::history::{now_secs, History, TxRecord};
use crate::jobs::{self, Job, JobBoard};
use crate::{txbuild, txbuild::Fee};

pub trait WalletBackendModule: Send + 'static {
    // ── central config ──
    fn set_proxy_config(&mut self, proxy_json: String) -> bool;
    fn get_proxy_config(&mut self) -> String;
    fn set_chains(&mut self, chains_json: String) -> bool;
    fn get_chains(&mut self) -> String;
    /// Ask eth_rpc whether the configured endpoint really is this chain.
    ///
    /// Waits for the reply, so it answers directly. NOT AVAILABLE on a `web`
    /// (wasm) image — use [`Self::start_test_endpoint`] + [`Self::take_result`],
    /// which work everywhere.
    fn test_endpoint(&mut self, chain_id: i64) -> String;

    // ── accounts (signing stays in the keystore) ──
    /// Import a mnemonic into the keystore and label the account it answers.
    /// Waits for the reply; see [`Self::start_import_mnemonic`].
    fn import_mnemonic(&mut self, phrase_json: String, label: String) -> String;
    /// The keystore's accounts. Waits for the reply; see
    /// [`Self::start_list_accounts`].
    fn list_accounts(&mut self) -> String;
    /// Drive a parked signing request forward: `{ ok, state, hash?, reason? }`.
    /// `state` is `awaiting_approval` until a human decides. There is no
    /// `unlock`/`lock` any more — the wallet never handles a vault password,
    /// and there is no unlocked state for it to toggle.
    ///
    /// Waits for each of its replies; see [`Self::start_send_status`].
    fn send_status(&mut self, request_id: String) -> String;

    // ── watched tokens ──
    fn set_watched_tokens(&mut self, chain_id: i64, addresses_json: String) -> bool;
    fn get_watched_tokens(&mut self, chain_id: i64) -> String;

    // ── tokens passthrough ──
    /// token_list's catalogue for a chain. Waits for the reply; see
    /// [`Self::start_get_tokens`].
    fn get_tokens(&mut self, chain_id: i64) -> String;
    /// Add a token to token_list's catalogue. Waits for the reply; see
    /// [`Self::start_add_custom_token`]. `false` on a `web` image, which cannot
    /// wait — there is no third answer a `bool` can carry, which is why the
    /// `start_*` twin exists.
    fn add_custom_token(&mut self, token_json: String) -> bool;

    // ── balances (Multicall3-batched) ──
    /// Fan out one balance read per chain and return AT ONCE; the cache is
    /// written and `balances_updated` emitted when the last reply lands. Async
    /// since before this port, so it works unchanged on every target.
    fn refresh_balances(&mut self, address: String) -> bool;
    fn get_balances(&mut self, address: String) -> String;

    // ── market (Uniswap prices for held tokens) ──
    /// The last refreshed market view. Offline on every target: it reads the
    /// prices and the token metadata that `refresh_market` cached.
    fn get_market(&mut self, address: String) -> String;
    /// Refresh prices for held tokens across all chains concurrently (per chain:
    /// `token_list.get_tokens` for the decimals, then one `uniswap.get_prices`),
    /// caches both, and emits `market_updated`. `get_market` then reads the
    /// cache. Returns immediately, on every target.
    fn refresh_market(&mut self, address: String) -> bool;

    // ── send ──
    /// Quote a send's fee through fee_module. Waits for the reply; see
    /// [`Self::start_estimate_fee`].
    fn estimate_fee(&mut self, send_json: String) -> String;
    /// Build a native send and put it in front of a human: nonce → fee → gas →
    /// `keystore.request_approval`, answering `{ ok, pending, requestId }`.
    /// Waits for each reply; see [`Self::start_send_native`].
    fn send_native(&mut self, send_json: String) -> String;
    /// [`Self::send_native`] for an ERC-20 transfer. See
    /// [`Self::start_send_erc20`].
    fn send_erc20(&mut self, send_json: String) -> String;

    // ── history ──
    fn get_history(&mut self, address: String) -> String;
    /// Poll a broadcast tx's receipt and update its history record. Waits for
    /// the reply; see [`Self::start_refresh_tx_status`].
    fn refresh_tx_status(&mut self, hash_hex: String, chain_id: i64) -> String;

    // ── the async spelling: `start_*` + `take_result` ──
    //
    // The shape the platform actually has, EVERYWHERE, and the only one a `web`
    // (wasm) image can use: each `start_*` issues its first outbound call and
    // answers `{ ok, jobId }` at once; the answer its waiting twin would have
    // returned is parked on the job board and collected with `take_result`.

    /// [`Self::test_endpoint`] without waiting.
    fn start_test_endpoint(&mut self, chain_id: i64) -> String;
    /// [`Self::import_mnemonic`] without waiting.
    fn start_import_mnemonic(&mut self, phrase_json: String, label: String) -> String;
    /// [`Self::list_accounts`] without waiting.
    fn start_list_accounts(&mut self) -> String;
    /// [`Self::get_tokens`] without waiting.
    fn start_get_tokens(&mut self, chain_id: i64) -> String;
    /// [`Self::add_custom_token`] without waiting. The job answers
    /// `{ ok: true, added: bool }` — the `bool` its twin returns, plus the
    /// room to report a transport failure that a bare `bool` has nowhere to put.
    fn start_add_custom_token(&mut self, token_json: String) -> String;
    /// [`Self::estimate_fee`] without waiting.
    fn start_estimate_fee(&mut self, send_json: String) -> String;
    /// [`Self::send_native`] without waiting.
    fn start_send_native(&mut self, send_json: String) -> String;
    /// [`Self::send_erc20`] without waiting.
    fn start_send_erc20(&mut self, send_json: String) -> String;
    /// [`Self::send_status`] without waiting.
    fn start_send_status(&mut self, request_id: String) -> String;
    /// [`Self::refresh_tx_status`] without waiting.
    fn start_refresh_tx_status(&mut self, hash_hex: String, chain_id: i64) -> String;
    /// Collect a `start_*` job: `{ok:false, pending:true}` while its calls are
    /// in flight, then the answer — ONCE. A second collect, an id that was never
    /// started, and one evicted at `jobs::CAPACITY` all report an error, so a
    /// poller is never left waiting on a slot that will never fill.
    fn take_result(&mut self, job_id: String) -> String;

    fn on_context_ready(&mut self, _ctx: &RustModuleContext) {}
}

pub trait WalletBackendModuleEvents {
    fn balances_updated(&self, address: String);
    fn market_updated(&self, address: String);
    fn tx_status_changed(&self, hash_hex: String);
    fn proxy_error(&self, context: String);
}

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/generated/provider_gen.rs"));

// ── THE OUTBOUND CALLS, AND THE TWO WAYS TO DRIVE THEM ───────────────────────
//
// This module is the wallet's COORDINATOR: nearly every method it has is one or
// more calls to a dependency, and several of them are a CHAIN — a nonce, then a
// fee derived from nothing, then a gas estimate over a tx built from both, then
// a human. That chain is the reason this file looks the way it does.
//
// A `web` (wasm) image can only make those calls with the generated `_async`
// clients. A Worker is a single event loop with no ASYNCIFY (ADR 0004), so a
// call that blocked for its reply would deadlock the loop that delivers it, and
// logos-rust-sdk does not compile the synchronous `lp_invoke` on that target at
// all — a missed call site is a compile error here rather than an undefined
// symbol at wasm-ld.
//
// THE NATIVE PATH KEEPS ITS SYNCHRONOUS CLIENTS, and that is a decision, not an
// omission. `uniswap_module` (#167) could rewrite its one call site to
// "dispatch async, wait on a channel" because it is `concurrency: "multi"`: its
// dispatch runs on a worker QThread while the completion is marshalled onto the
// consumer's owner thread (the Qt main thread), so the thread that waits is
// never the thread that must deliver. THIS module is `concurrency: "single"` —
// dispatch IS the Qt main thread — so the same rewrite would deadlock every
// method it touched. The synchronous client, which spins its own wait, is the
// correct spelling here and stays.
//
// So each flow is written ONCE, as a state machine over `Call` values, and
// DRIVEN TWICE:
//
//   run_waiting  native only. Issue each call with the synchronous client and
//                feed the reply back in, in a loop. Same calls, same order,
//                same answers as the straight-line code this replaces.
//   drive        everywhere. Issue each call with the `_async` client and feed
//                the reply back in from the callback; park the answer on the
//                job board for `take_result`.
//
// REPLIES ARE NOT ORDERED, which is exactly why a flow is a state machine and
// not a fan-out: every call in a chain is issued only once its predecessor's
// reply is in hand, so there is never more than one of a flow's calls in flight
// and nothing here can be confused by arrival order. The two places this module
// DOES fan out — `refresh_balances` and `refresh_market` — collect with
// `gather`, which is order-independent by construction (each task writes its own
// slot).

/// Answers parked while their outbound calls are in flight.
///
/// A `static`, not a field on the impl: the callback an async client takes is
/// `FnOnce + Send + 'static` and cannot borrow the module. That is true of every
/// async callback in the SDK, not a property of this module.
static JOBS: JobBoard = JobBoard::new();

/// One outbound call to a dependency, described BY VALUE so that the same
/// description can be issued either way.
///
/// Every reply is normalised to `Result<String, String>`; the two `bool`-valued
/// methods answer `"true"` / `"false"`, which is what a flow would read back out
/// of JSON anyway, so no flow has to know which shape its dependency happens to
/// use.
enum Call {
    EthVerifyChainId(i64),
    EthTransactionCount(i64, String),
    EthEstimateGas(i64, String),
    EthSendRawTransaction(i64, String),
    EthTransactionReceipt(i64, String),
    KeystoreRequestApproval(String),
    KeystoreApprovalStatus(String, String),
    KeystoreFetchResult(String, String),
    KeystoreAckResult(String, String),
    KeystoreImportMnemonic(String),
    KeystoreListAccounts,
    TokenListGetTokens(i64),
    TokenListAddCustomToken(String),
    FeeEstimate(i64, String),
}

impl Call {
    /// Issue this call and WAIT for its answer, with the synchronous client.
    ///
    /// Native only — `lp_invoke` does not exist on wasm32 and the generated sync
    /// clients are `#[cfg(not(target_os = "emscripten"))]` accordingly.
    #[cfg(not(target_os = "emscripten"))]
    fn invoke_waiting(self) -> std::result::Result<String, String> {
        let m = modules();
        match self {
            Call::EthVerifyChainId(c) => m.eth_rpc_module.verify_chain_id(c).map_err(|e| e.to_string()),
            Call::EthTransactionCount(c, a) => m.eth_rpc_module.get_transaction_count(c, &a).map_err(|e| e.to_string()),
            Call::EthEstimateGas(c, tx) => m.eth_rpc_module.estimate_gas(c, &tx).map_err(|e| e.to_string()),
            Call::EthSendRawTransaction(c, raw) => m.eth_rpc_module.send_raw_transaction(c, &raw).map_err(|e| e.to_string()),
            Call::EthTransactionReceipt(c, h) => m.eth_rpc_module.get_transaction_receipt(c, &h).map_err(|e| e.to_string()),
            Call::KeystoreRequestApproval(i) => m.keystore_module.request_approval(&i).map_err(|e| e.to_string()),
            Call::KeystoreApprovalStatus(h, r) => m.keystore_module.approval_status(&h, &r).map_err(|e| e.to_string()),
            Call::KeystoreFetchResult(h, r) => m.keystore_module.fetch_result(&h, &r).map_err(|e| e.to_string()),
            Call::KeystoreAckResult(h, r) => m.keystore_module.ack_result(&h, &r).map(|b| b.to_string()).map_err(|e| e.to_string()),
            Call::KeystoreImportMnemonic(p) => m.keystore_module.import_mnemonic(&p).map_err(|e| e.to_string()),
            Call::KeystoreListAccounts => m.keystore_module.list_accounts().map_err(|e| e.to_string()),
            Call::TokenListGetTokens(c) => m.token_list_module.get_tokens(c).map_err(|e| e.to_string()),
            Call::TokenListAddCustomToken(t) => m.token_list_module.add_custom_token(&t).map(|b| b.to_string()).map_err(|e| e.to_string()),
            Call::FeeEstimate(c, r) => m.fee_module.estimate(c, &r).map_err(|e| e.to_string()),
        }
    }

    /// Issue this call and hand its answer to `done`, with the `_async` client.
    ///
    /// `done` runs on the protocol's completion path, not on the thread that
    /// called this, which is why it is `Send + 'static` and carries everything
    /// it needs by value.
    fn invoke_async(self, done: impl FnOnce(std::result::Result<String, String>) + Send + 'static) {
        let m = modules();
        match self {
            Call::EthVerifyChainId(c) => m.eth_rpc_module.verify_chain_id_async(c, move |res| done(res.map_err(|e| e.to_string()))),
            Call::EthTransactionCount(c, a) => m.eth_rpc_module.get_transaction_count_async(c, &a, move |res| done(res.map_err(|e| e.to_string()))),
            Call::EthEstimateGas(c, tx) => m.eth_rpc_module.estimate_gas_async(c, &tx, move |res| done(res.map_err(|e| e.to_string()))),
            Call::EthSendRawTransaction(c, raw) => m.eth_rpc_module.send_raw_transaction_async(c, &raw, move |res| done(res.map_err(|e| e.to_string()))),
            Call::EthTransactionReceipt(c, h) => m.eth_rpc_module.get_transaction_receipt_async(c, &h, move |res| done(res.map_err(|e| e.to_string()))),
            Call::KeystoreRequestApproval(i) => m.keystore_module.request_approval_async(&i, move |res| done(res.map_err(|e| e.to_string()))),
            Call::KeystoreApprovalStatus(h, r) => m.keystore_module.approval_status_async(&h, &r, move |res| done(res.map_err(|e| e.to_string()))),
            Call::KeystoreFetchResult(h, r) => m.keystore_module.fetch_result_async(&h, &r, move |res| done(res.map_err(|e| e.to_string()))),
            Call::KeystoreAckResult(h, r) => m.keystore_module.ack_result_async(&h, &r, move |res| done(res.map(|b| b.to_string()).map_err(|e| e.to_string()))),
            Call::KeystoreImportMnemonic(p) => m.keystore_module.import_mnemonic_async(&p, move |res| done(res.map_err(|e| e.to_string()))),
            Call::KeystoreListAccounts => m.keystore_module.list_accounts_async(move |res| done(res.map_err(|e| e.to_string()))),
            Call::TokenListGetTokens(c) => m.token_list_module.get_tokens_async(c, move |res| done(res.map_err(|e| e.to_string()))),
            Call::TokenListAddCustomToken(t) => m.token_list_module.add_custom_token_async(&t, move |res| done(res.map(|b| b.to_string()).map_err(|e| e.to_string()))),
            Call::FeeEstimate(c, r) => m.fee_module.estimate_async(c, &r, move |res| done(res.map_err(|e| e.to_string()))),
        }
    }
}

/// What a flow wants next: one more outbound call, or its answer.
enum Step {
    Call(Call),
    Done(String),
}

/// A method's outbound work as a state machine — written once, driven twice.
///
/// `Send + 'static` because the async driver hands the flow to a callback; the
/// module state a flow needs after its last reply (the pending-job map, the
/// history store, the persistence dir) is carried in by `Arc`, never borrowed.
trait Flow: Send + 'static {
    /// `reply` is `None` on the first step and the previous call's answer after
    /// that. `Step::Done` ends the flow; nothing calls `step` again.
    fn step(&mut self, reply: Option<std::result::Result<String, String>>) -> Step;
}

/// Drive `flow` with the synchronous clients and answer what it decides.
#[cfg(not(target_os = "emscripten"))]
fn run_waiting(mut flow: impl Flow) -> String {
    let mut reply = None;
    loop {
        match flow.step(reply) {
            Step::Done(answer) => return answer,
            Step::Call(call) => reply = Some(call.invoke_waiting()),
        }
    }
}

/// Drive `flow` with the `_async` clients, parking its answer on `job`.
///
/// Recursive, and bounded by the flow: each `Step::Call` re-enters exactly once,
/// from the callback, and a flow that returns `Step::Done` stops.
fn drive<F: Flow>(mut flow: F, reply: Option<std::result::Result<String, String>>, job: String) {
    match flow.step(reply) {
        Step::Done(answer) => JOBS.complete(&job, answer),
        Step::Call(call) => call.invoke_async(move |r| drive(flow, Some(r), job)),
    }
}

/// Start `flow` and answer its job id at once — the `start_*` spelling, and the
/// one that works on every target.
fn start_flow(flow: impl Flow) -> String {
    let job_id = JOBS.start();
    drive(flow, None, job_id.clone());
    json!({ "ok": true, "jobId": job_id }).to_string()
}

/// Run `flow` to its answer — the WAITING spelling of a method.
///
/// On a native host this issues each call with the synchronous client, which is
/// what every one of these methods has always done.
#[cfg(not(target_os = "emscripten"))]
fn answer(flow: impl Flow, _start: &str) -> String {
    run_waiting(flow)
}

/// The same entry point on a `web` (wasm) image, where waiting is the one thing
/// that cannot be done: one Worker, one event loop, no ASYNCIFY (ADR 0004), so
/// the thread that would wait is the thread that must deliver. NOTHING is
/// dispatched — a call whose reply can never be collected is worse than none —
/// and the refusal names the method that does work, so a caller reading the
/// error can act on it.
#[cfg(target_os = "emscripten")]
fn answer(flow: impl Flow, start: &str) -> String {
    drop(flow);
    err(format!(
        "this build cannot wait for a reply (one Worker, one event loop): use {start}, then take_result"
    ))
}

/// The flow of a method that makes exactly ONE outbound call: issue it, then
/// answer whatever `finish` makes of the reply.
struct OneCall<F> {
    call: Option<Call>,
    finish: Option<F>,
}

impl<F> Flow for OneCall<F>
where
    F: FnOnce(std::result::Result<String, String>) -> String + Send + 'static,
{
    fn step(&mut self, reply: Option<std::result::Result<String, String>>) -> Step {
        match self.call.take() {
            Some(call) => Step::Call(call),
            None => {
                let finish = self.finish.take().expect("a OneCall flow finishes once");
                Step::Done(finish(reply.expect("the reply to the call just issued")))
            }
        }
    }
}

fn one_call(
    call: Call,
    finish: impl FnOnce(std::result::Result<String, String>) -> String + Send + 'static,
) -> impl Flow {
    OneCall { call: Some(call), finish: Some(finish) }
}

/// A flow that never leaves the process: it already has its answer.
///
/// What a `plan_*` returns when it failed before the network — a malformed
/// request, an address that will not parse — so that the two spellings report it
/// identically and `start_*` still hands back a collectable job rather than a
/// bare error a poller cannot tell from a transport failure.
struct Answered(Option<String>);

impl Flow for Answered {
    fn step(&mut self, _reply: Option<std::result::Result<String, String>>) -> Step {
        Step::Done(self.0.take().expect("an Answered flow answers once"))
    }
}

/// The reply of a dependency method whose answer is passed straight through:
/// its own JSON on success, this module's error envelope on a transport failure.
fn passthrough(reply: std::result::Result<String, String>) -> String {
    match reply {
        Ok(s) => s,
        Err(e) => err(e),
    }
}

#[derive(Default)]
struct WalletBackendModuleImpl {
    state: Option<State>,
}

/// chainId -> (token address -> (eth, usd)) prices.
type PriceCache = Arc<Mutex<std::collections::HashMap<u64, std::collections::HashMap<String, (Option<f64>, Option<f64>)>>>>;
/// chainId -> (lowercased token address -> (symbol, decimals)).
type MetaCache = Arc<Mutex<std::collections::HashMap<u64, std::collections::HashMap<String, (String, u8)>>>>;
/// Keystore approval handle -> the transaction parked on it.
type JobMap = Arc<Mutex<std::collections::HashMap<String, PendingJob>>>;

struct State {
    cfg: ConfigStore,
    /// `Arc` so a flow that records a broadcast tx from an async callback (a
    /// `'static` closure that cannot borrow `&self`) writes the SAME store the
    /// module reads. `History` is a path and its methods take `&self`, so there
    /// is nothing here to lock.
    history: Arc<History>,
    dir: std::path::PathBuf,
    /// chainId -> watched token addresses (persisted in watched.json).
    watched: std::collections::HashMap<u64, Vec<String>>,
    /// address -> last aggregate balances (persisted in balances_cache.json).
    /// `Arc<Mutex<_>>` so the async balance-refresh fan-out's completion callback
    /// (a `'static` closure that can't borrow `&self`) can write it; the event
    /// loop is single-threaded, so the lock is effectively uncontended.
    balances: Arc<Mutex<std::collections::HashMap<String, Value>>>,
    /// chainId -> (token address -> (eth, usd)) prices — populated by the
    /// `refresh_market` fan-out, read by `get_market`. Ephemeral (not persisted).
    market_prices: PriceCache,
    /// Symbols and decimals per chain, populated by the same fan-out.
    ///
    /// `refresh_market` already had to ask `token_list_module` for decimals
    /// before it could build a price request, and `get_market` then asked for
    /// the SAME answer again to label the holdings. That second ask was a
    /// synchronous call from a method that has nothing else to wait for, so it
    /// is now the first leg of the fan-out and `get_market` reads what it left
    /// here — which also makes `get_market` an entirely offline method, on every
    /// target.
    token_meta: MetaCache,
    /// Signing requests awaiting a human. Keyed by the keystore approval
    /// handle. Nothing here blocks a dispatch: a request returns immediately
    /// and the UI drives it forward with `send_status`. Behind an `Arc<Mutex<_>>`
    /// for the reason `balances` is: the send flow's last step runs in a callback.
    jobs: JobMap,
}

/// A transaction parked on a human: broadcast it and record it once approved.
struct PendingJob {
    chain_id: u64,
    receipt: String,
    record: TxRecord,
}

/// The approval intent asking a human to sign `legs` for `address`.
///
/// The ask itself is a step in [`SendFlow`] now rather than a helper that waits
/// for it: `keystore_module` answers in microseconds, but "in microseconds" is
/// still a reply, and a `web` image cannot wait for one.
fn signing_intent(address: &str, purpose: &str, legs: Vec<Value>) -> String {
    json!({ "address": address, "purpose": purpose, "legs": legs }).to_string()
}

/// The `{ handle, receipt }` pair `request_approval` answers. The receipt
/// authorises collecting the result and is never re-derivable.
fn approval_handle(resp: &Value) -> std::result::Result<(String, String), String> {
    let handle = resp["handle"].as_str().ok_or("keystore: no handle")?.to_string();
    let receipt = resp["receipt"].as_str().ok_or("keystore: no receipt")?.to_string();
    Ok((handle, receipt))
}

/// One `tx` leg of an approval intent.
fn tx_leg(chain_id: u64, unsigned: &Value) -> Value {
    json!({ "kind": "tx", "chain_id": chain_id, "tx": unsigned })
}

// ── small helpers ────────────────────────────────────────────────────────────

fn err(e: impl std::fmt::Display) -> String {
    json!({ "ok": false, "error": e.to_string() }).to_string()
}

/// Parse a dependency's `{ ok, ... }` JSON reply, surfacing `{ok:false}` as Err.
fn ok_value(s: String) -> std::result::Result<Value, String> {
    let v: Value = serde_json::from_str(&s).map_err(|e| e.to_string())?;
    if v.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(v.get("error").and_then(Value::as_str).unwrap_or("dependency error").to_string());
    }
    Ok(v)
}

fn parse_addr(s: &str) -> std::result::Result<Address, String> {
    let t = s.trim();
    let h = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t);
    let b = hex::decode(h).map_err(|e| e.to_string())?;
    if b.len() != 20 {
        return Err(format!("address must be 20 bytes: {s}"));
    }
    Ok(Address::from_slice(&b))
}

fn parse_hex_u64(s: &str) -> u64 {
    let t = s.trim();
    let h = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t);
    u64::from_str_radix(h, 16).unwrap_or(0)
}

/// `balance / 10^decimals * usd` as a display value (f64 is ample for UI).
fn value_usd(balance: &str, decimals: u8, usd: Option<f64>) -> Option<f64> {
    let usd = usd?;
    let bal: f64 = parse_u256_str(balance).to_string().parse().unwrap_or(0.0);
    Some(bal / 10f64.powi(decimals as i32) * usd)
}

/// Fallback symbol for an unknown token: `0x1234…abcd`.
fn short_addr(a: &str) -> String {
    let t = a.trim_start_matches("0x");
    if t.len() >= 8 {
        format!("0x{}…{}", &t[..4], &t[t.len() - 4..])
    } else {
        a.to_string()
    }
}

fn parse_u256_str(s: &str) -> U256 {
    let t = s.trim();
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        U256::from_str_radix(h, 16).unwrap_or(U256::ZERO)
    } else {
        t.parse::<U256>().unwrap_or(U256::ZERO)
    }
}

// ── async balance fan-out (over the concurrency:"multi" eth_rpc) ──────────────
//
// `refresh_balances` fires one async eth_rpc call per chain (a single Multicall3
// `eth_call` when available, else native + per-token via a nested gather) and
// returns immediately. Because eth_rpc is `concurrency: "multi"`, the calls run
// in parallel; `gather` collects them and the final completion writes the cache
// and emits `balances_updated` — the same contract every consumer already uses.

type GatherDone<T> = Box<dyn FnOnce(T) + Send>;
type GatherTask<T> = Box<dyn FnOnce(GatherDone<T>) + Send>;

/// Fire every task; invoke `on_all` with all results (in task order) once the last
/// completes. Each task fires its async dep call(s) and hands its result to `done`.
/// Generic + dependency-free — a candidate to lift into the rust SDK.
#[allow(clippy::type_complexity)]
fn gather<T: Send + 'static>(tasks: Vec<GatherTask<T>>, on_all: impl FnOnce(Vec<T>) + Send + 'static) {
    let n = tasks.len();
    if n == 0 {
        on_all(Vec::new());
        return;
    }
    let state: Arc<Mutex<(Vec<Option<T>>, usize, Option<Box<dyn FnOnce(Vec<T>) + Send>>)>> =
        Arc::new(Mutex::new(((0..n).map(|_| None).collect(), n, Some(Box::new(on_all)))));
    for (i, task) in tasks.into_iter().enumerate() {
        let state = Arc::clone(&state);
        task(Box::new(move |result: T| {
            let mut s = state.lock().unwrap();
            s.0[i] = Some(result);
            s.1 -= 1;
            if s.1 == 0 {
                let results: Vec<T> = s.0.drain(..).map(|o| o.expect("gather slot filled")).collect();
                let cb = s.2.take().expect("gather on_all fires once");
                drop(s);
                cb(results);
            }
        }));
    }
}

/// One chain's balances → `{ chainId, native, tokens }`, fetched async. Common
/// case: a single Multicall3 `eth_call`; else native + per-token via nested gather.
fn fetch_chain_async(
    chain_id: u64,
    holder: Address,
    holder_hex: String,
    multicall: Option<String>,
    tokens: Vec<String>,
    done: GatherDone<Value>,
) {
    if let Some(mc) = multicall {
        if let Some(call_json) = build_multicall_call_json(&mc, holder, &tokens) {
            // `None` for the deadline: eth_rpc's `call` takes the CALLER's own wall
            // budget, and this module has none — the refresh is driven by a user opening
            // a screen, not by a deadline it could state. Absent leaves the chain's
            // configured `timeoutSecs` in charge, which is exactly the behaviour this
            // fan-out had before the parameter existed.
            modules().eth_rpc_module.call_async(chain_id as i64, &call_json, None, move |res| {
                done(decode_multicall_chain(chain_id, &tokens, res.ok()));
            });
            return;
        }
    }
    fetch_chain_individual_async(chain_id, holder, holder_hex, tokens, done);
}

/// Fallback for chains without Multicall3: native `eth_getBalance` + one `eth_call`
/// per token, gathered into the same chain value.
fn fetch_chain_individual_async(
    chain_id: u64,
    holder: Address,
    holder_hex: String,
    tokens: Vec<String>,
    done: GatherDone<Value>,
) {
    let mut tasks: Vec<GatherTask<(usize, U256)>> = Vec::with_capacity(tokens.len() + 1);
    tasks.push(Box::new(move |d| {
        modules()
            .eth_rpc_module
            .get_balance_async(chain_id as i64, &holder_hex, move |res| d((0, decode_balance_reply(res.ok()))));
    }));
    for (i, t) in tokens.iter().enumerate() {
        let call_json = erc20_balance_call_json(t, holder);
        let slot = i + 1;
        tasks.push(Box::new(move |d| {
            modules()
                .eth_rpc_module
                .call_async(chain_id as i64, &call_json, None, move |res| d((slot, decode_call_balance_reply(res.ok()))));
        }));
    }
    gather(tasks, move |parts: Vec<(usize, U256)>| {
        let mut by_slot: std::collections::HashMap<usize, U256> = std::collections::HashMap::new();
        for (s, v) in parts {
            by_slot.insert(s, v);
        }
        let native = by_slot.get(&0).copied().unwrap_or(U256::ZERO);
        let token_balances: Vec<Value> = tokens
            .iter()
            .enumerate()
            .map(|(i, t)| json!({ "address": t, "balance": by_slot.get(&(i + 1)).copied().unwrap_or(U256::ZERO).to_string() }))
            .collect();
        done(json!({ "chainId": chain_id, "native": native.to_string(), "tokens": token_balances }));
    });
}

fn build_multicall_call_json(mc: &str, holder: Address, tokens: &[String]) -> Option<String> {
    let mc_addr = parse_addr(mc).ok()?;
    let mut calls: Vec<(Address, Vec<u8>)> = vec![(mc_addr, txbuild::multicall3_get_eth_balance_calldata(holder))];
    for t in tokens {
        calls.push((parse_addr(t).ok()?, txbuild::erc20_balance_of_calldata(holder)));
    }
    let data = txbuild::multicall3_aggregate3_calldata(&calls);
    Some(json!({ "to": mc, "data": format!("0x{}", hex::encode(data)) }).to_string())
}

fn erc20_balance_call_json(token: &str, holder: Address) -> String {
    let data = txbuild::erc20_balance_of_calldata(holder);
    json!({ "to": token, "data": format!("0x{}", hex::encode(data)) }).to_string()
}

fn empty_chain(chain_id: u64, tokens: &[String]) -> Value {
    let token_balances: Vec<Value> = tokens.iter().map(|t| json!({ "address": t, "balance": "0" })).collect();
    json!({ "chainId": chain_id, "native": "0", "tokens": token_balances })
}

/// Decode a Multicall3 `aggregate3` reply (the `{ ok, result }` string from
/// eth_rpc.call) into `{ chainId, native, tokens }`. Any failure degrades to zeros.
fn decode_multicall_chain(chain_id: u64, tokens: &[String], reply: Option<String>) -> Value {
    let rets = reply
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .filter(|v| v.get("ok").and_then(Value::as_bool) != Some(false))
        .and_then(|v| v["result"].as_str().map(String::from))
        .and_then(|h| hex::decode(h.trim_start_matches("0x")).ok())
        .and_then(|b| txbuild::decode_aggregate3_returns(&b));
    let Some(rets) = rets else {
        return empty_chain(chain_id, tokens);
    };
    let native = rets.first().and_then(|o| o.as_ref()).and_then(|d| txbuild::decode_uint256(d)).unwrap_or(U256::ZERO);
    let token_balances: Vec<Value> = tokens
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let bal = rets.get(i + 1).and_then(|o| o.as_ref()).and_then(|d| txbuild::decode_uint256(d)).unwrap_or(U256::ZERO);
            json!({ "address": t, "balance": bal.to_string() })
        })
        .collect();
    json!({ "chainId": chain_id, "native": native.to_string(), "tokens": token_balances })
}

/// Decode a native `eth_getBalance` reply (`{ ok, result: "0x.." }`) → U256.
fn decode_balance_reply(reply: Option<String>) -> U256 {
    reply
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .filter(|v| v.get("ok").and_then(Value::as_bool) != Some(false))
        .and_then(|v| v["result"].as_str().map(parse_u256_str))
        .unwrap_or(U256::ZERO)
}

/// Decode an ERC20 `balanceOf` `eth_call` reply (`{ ok, result: "0x..32" }`) → U256.
fn decode_call_balance_reply(reply: Option<String>) -> U256 {
    reply
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .filter(|v| v.get("ok").and_then(Value::as_bool) != Some(false))
        .and_then(|v| v["result"].as_str().map(String::from))
        .and_then(|h| hex::decode(h.trim_start_matches("0x")).ok())
        .and_then(|b| txbuild::decode_uint256(&b))
        .unwrap_or(U256::ZERO)
}

/// What one chain's leg of the `refresh_market` fan-out answers: the chain, the
/// token metadata that built its price request, and the prices themselves.
type MarketLeg = (
    u64,
    std::collections::HashMap<String, (String, u8)>,
    std::collections::HashMap<String, (Option<f64>, Option<f64>)>,
);

/// Decode a `token_list.get_tokens` reply (`{ tokens: [{address, symbol,
/// decimals}] }`) into a lowercased `address -> (symbol, decimals)` map. Any
/// failure yields an empty map, which prices the holding at 18 decimals and
/// labels it by its short address — what the synchronous version did too.
fn decode_token_meta(reply: Option<String>) -> std::collections::HashMap<String, (String, u8)> {
    let mut map = std::collections::HashMap::new();
    let Some(v) = reply.and_then(|s| serde_json::from_str::<Value>(&s).ok()) else {
        return map;
    };
    if let Some(arr) = v.get("tokens").and_then(Value::as_array) {
        for t in arr {
            if let Some(addr) = t.get("address").and_then(Value::as_str) {
                let sym = t.get("symbol").and_then(Value::as_str).unwrap_or("?").to_string();
                let dec = t.get("decimals").and_then(Value::as_u64).unwrap_or(18) as u8;
                map.insert(addr.to_lowercase(), (sym, dec));
            }
        }
    }
    map
}

/// Decode a uniswap `get_prices` reply (`{ prices: [{address, eth, usd}] }`) into
/// a `token address -> (eth, usd)` map. Any failure yields an empty map.
fn decode_uniswap_prices(reply: Option<String>) -> std::collections::HashMap<String, (Option<f64>, Option<f64>)> {
    let mut out = std::collections::HashMap::new();
    let Some(v) = reply.and_then(|s| serde_json::from_str::<Value>(&s).ok()) else {
        return out;
    };
    if let Some(arr) = v.get("prices").and_then(Value::as_array) {
        for p in arr {
            if let Some(addr) = p.get("address").and_then(Value::as_str) {
                let eth = p.get("eth").and_then(Value::as_f64);
                let usd = p.get("usd").and_then(Value::as_f64);
                out.insert(addr.to_string(), (eth, usd));
            }
        }
    }
    out
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendParams {
    from: String,
    to: String,
    chain_id: u64,
    #[serde(default)]
    amount: String, // wei (native) or token base units (erc20)
    #[serde(default)]
    token_address: String, // erc20 only

    // Fee controls, all optional and all passed straight to fee_module. A
    // caller that supplies both fee fields is obeyed verbatim; otherwise
    // `tier` selects a suggestion and defaults to "normal".
    #[serde(default)]
    tier: Option<String>, // "slow" | "normal" | "fast"
    #[serde(default)]
    max_fee_per_gas: Option<String>,
    #[serde(default)]
    max_priority_fee_per_gas: Option<String>,
    #[serde(default)]
    gas_limit: Option<String>,
}

// ── the single-call plans ────────────────────────────────────────────────────
//
// Free functions rather than methods: none of them reads module state, and the
// two spellings of each method then share one definition instead of agreeing by
// inspection.

/// `test_endpoint`: ask eth_rpc whether the endpoint really is this chain.
fn plan_test_endpoint(chain_id: i64) -> impl Flow {
    one_call(Call::EthVerifyChainId(chain_id), passthrough)
}

/// `list_accounts`: the keystore's answer, passed straight through.
fn plan_list_accounts() -> impl Flow {
    one_call(Call::KeystoreListAccounts, passthrough)
}

/// `get_tokens`: token_list's catalogue for a chain, passed straight through.
fn plan_get_tokens(chain_id: i64) -> impl Flow {
    one_call(Call::TokenListGetTokens(chain_id), passthrough)
}

/// `add_custom_token`: token_list's `bool`, in an envelope that also has room
/// for a transport failure. The waiting twin flattens both back to a `bool`.
fn plan_add_custom_token(token_json: String) -> impl Flow {
    one_call(Call::TokenListAddCustomToken(token_json), |reply| match reply {
        Ok(b) => json!({ "ok": true, "added": b == "true" }).to_string(),
        Err(e) => err(e),
    })
}

/// `estimate_fee`: one fee_module quote over the send the caller described.
///
/// Delegated to fee_module. The gas limit was hardcoded 21_000 / 90_000 here,
/// which is right for a bare transfer and wrong for any ERC-20 that does more
/// than move a balance -- so hand fee_module the actual call and let it run
/// estimate_gas. Without a `tx` it has nothing to measure and correctly returns
/// 0, which is how this first shipped.
fn plan_estimate_fee(send_json: &str) -> Box<dyn Flow> {
    let p: SendParams = match serde_json::from_str(send_json) {
        Ok(p) => p,
        Err(e) => return Box::new(Answered(Some(err(e)))),
    };
    let tx = match parse_addr(&p.to) {
        Ok(to_addr) => {
            if p.token_address.is_empty() {
                json!({ "from": p.from, "to": p.to,
                        "value": format!("0x{:x}", parse_u256_str(&p.amount)) })
            } else {
                let data = txbuild::erc20_transfer_calldata(to_addr, parse_u256_str(&p.amount));
                json!({ "from": p.from, "to": p.token_address,
                        "data": format!("0x{}", hex::encode(data)) })
            }
        }
        // An unparseable recipient is the caller's problem, not a reason to
        // refuse a fee quote: fall back to no tx and report gasLimit 0.
        Err(_) => Value::Null,
    };
    let mut fee_req = json!({
        "tier": p.tier.clone().unwrap_or_else(|| "normal".into()),
        "maxFeePerGas": p.max_fee_per_gas.clone(),
        "maxPriorityFeePerGas": p.max_priority_fee_per_gas.clone(),
        "gasLimit": p.gas_limit.clone(),
    });
    if !tx.is_null() {
        fee_req["tx"] = tx;
    }
    Box::new(one_call(Call::FeeEstimate(p.chain_id as i64, fee_req.to_string()), passthrough))
}

/// A boxed flow is a flow, so a `plan_*` that may answer before the network can
/// hand back either shape.
impl Flow for Box<dyn Flow> {
    fn step(&mut self, reply: Option<std::result::Result<String, String>>) -> Step {
        (**self).step(reply)
    }
}

// ── send: nonce → fee → gas → a human ────────────────────────────────────────

/// Which of [`SendFlow`]'s four calls is in flight.
enum SendStage {
    Nonce,
    Fee,
    Gas,
    Approve,
}

/// `send_native` / `send_erc20`, as the chain of calls it has always been.
///
/// Strictly sequential, and not by choice: the fee request is issued for the
/// chain the nonce was read on, the gas estimate is over a tx built from both,
/// and the approval asks a human to sign the result. Nothing here can be fanned
/// out, and nothing here depends on reply ORDER either — each call is issued
/// only once the previous reply is in hand.
///
/// Everything that does NOT depend on a reply — the recipient address, the
/// estimate's tx shape, the ERC-20 calldata, the default gas limit — is
/// resolved by `plan_send` before the first call goes out. A recipient that will
/// not parse is therefore reported without spending a nonce read and a fee
/// quote on it first; the error a caller sees is the same one it always was.
struct SendFlow {
    stage: SendStage,
    chain_id: u64,
    from: String,
    /// The recipient as the caller spelled it, for the history record.
    to: String,
    to_addr: Address,
    /// `Some((token, amount))` for an ERC-20 transfer, `None` for a native send.
    erc20: Option<(Address, U256)>,
    /// The native value, or the token amount, as `txbuild` needs it.
    amount: U256,
    fee_req: String,
    est_tx: String,
    default_gas: u64,
    jobs: JobMap,

    // Filled in as the replies land.
    nonce: u64,
    fee: Option<Fee>,
    gas_limit: u64,
}

impl SendFlow {
    /// The unsigned transaction the human is asked to approve. Only callable
    /// once the nonce, the fee and the gas limit are all in.
    fn unsigned(&self) -> Value {
        let fee = self.fee.as_ref().expect("the fee reply landed before the approval");
        match &self.erc20 {
            Some((token, amount)) => {
                txbuild::unsigned_erc20_tx(*token, self.to_addr, *amount, self.nonce, self.gas_limit, fee)
            }
            None => txbuild::unsigned_native_tx(self.to_addr, self.amount, self.nonce, self.gas_limit, fee),
        }
    }

    /// Park the approved-but-unsigned transaction and answer the request id.
    fn park(&mut self, resp: &Value) -> std::result::Result<Value, String> {
        let (handle, receipt) = approval_handle(resp)?;
        let (kind, token, value) = match &self.erc20 {
            Some((token, amount)) => ("erc20", Some(format!("{token}")), amount.to_string()),
            None => ("native", None, self.amount.to_string()),
        };
        let record = TxRecord {
            // Filled in when the signature comes back and is broadcast.
            hash: String::new(),
            chain_id: self.chain_id,
            from: self.from.clone(),
            to: self.to.clone(),
            value,
            kind: kind.into(),
            token,
            status: "pending".into(),
            timestamp: now_secs(),
        };
        // History is written when the tx is actually broadcast — recording it
        // now would show the user a transaction they have not yet approved.
        self.jobs.lock().unwrap().insert(
            handle.clone(),
            PendingJob { chain_id: self.chain_id, receipt, record },
        );
        Ok(json!({ "ok": true, "pending": true, "requestId": handle }))
    }
}

impl Flow for SendFlow {
    fn step(&mut self, reply: Option<std::result::Result<String, String>>) -> Step {
        let Some(reply) = reply else {
            return Step::Call(Call::EthTransactionCount(self.chain_id as i64, self.from.clone()));
        };
        match self.stage {
            SendStage::Nonce => {
                let v = match reply.and_then(ok_value) {
                    Ok(v) => v,
                    Err(e) => return Step::Done(err(e)),
                };
                self.nonce = parse_hex_u64(v["result"].as_str().unwrap_or("0x0"));
                self.stage = SendStage::Fee;
                Step::Call(Call::FeeEstimate(self.chain_id as i64, self.fee_req.clone()))
            }
            SendStage::Fee => {
                let v = match reply.and_then(ok_value) {
                    Ok(v) => v,
                    Err(e) => return Step::Done(err(e)),
                };
                self.fee = Some(Fee::Eip1559 {
                    max_fee_per_gas: parse_u256_str(v["maxFeePerGas"].as_str().unwrap_or("0")),
                    max_priority_fee_per_gas: parse_u256_str(v["maxPriorityFeePerGas"].as_str().unwrap_or("0")),
                });
                self.stage = SendStage::Gas;
                Step::Call(Call::EthEstimateGas(self.chain_id as i64, self.est_tx.clone()))
            }
            SendStage::Gas => {
                // A gas estimate that fails is not a reason to refuse the send:
                // the hardcoded default is right for a bare transfer and the
                // human sees the limit either way.
                self.gas_limit = reply
                    .ok()
                    .and_then(|s| ok_value(s).ok())
                    .and_then(|v| v["result"].as_str().map(parse_hex_u64))
                    .unwrap_or(self.default_gas);
                self.stage = SendStage::Approve;
                let unsigned = self.unsigned();
                Step::Call(Call::KeystoreRequestApproval(signing_intent(
                    &self.from,
                    "send",
                    vec![tx_leg(self.chain_id, &unsigned)],
                )))
            }
            SendStage::Approve => Step::Done(
                match reply.and_then(ok_value).and_then(|v| self.park(&v)) {
                    Ok(v) => v.to_string(),
                    Err(e) => err(e),
                },
            ),
        }
    }
}

// ── send_status: approval → signatures → broadcast → acknowledge ─────────────

/// Which of [`StatusFlow`]'s calls is in flight.
enum StatusStage {
    Status,
    Fetch,
    Broadcast,
    Ack,
}

/// `send_status`: poll the human's decision, and on approval put the signature
/// into the transaction, broadcast it and record it.
///
/// Safe to call repeatedly, which is the whole contract: it short-circuits to
/// `awaiting_approval` while nobody has decided, and `fetch_result` is
/// idempotent until `ack_result` acknowledges it.
struct StatusFlow {
    stage: StatusStage,
    request_id: String,
    receipt: String,
    jobs: JobMap,
    history: Arc<History>,
    /// The record taken off the parked job, carried from the step that removed
    /// it to the step that learns its hash.
    pending: Option<TxRecord>,
    /// The broadcast hash, kept so the final answer can carry it after the
    /// best-effort acknowledgement has gone out.
    hash: String,
}

impl Flow for StatusFlow {
    fn step(&mut self, reply: Option<std::result::Result<String, String>>) -> Step {
        let Some(reply) = reply else {
            return Step::Call(Call::KeystoreApprovalStatus(
                self.request_id.clone(),
                self.receipt.clone(),
            ));
        };
        match self.stage {
            StatusStage::Status => {
                let st = match reply.and_then(ok_value) {
                    Ok(v) => v,
                    Err(e) => return Step::Done(err(e)),
                };
                match st["state"].as_str().unwrap_or("") {
                    "offered" | "rendered" => {
                        return Step::Done(json!({ "ok": true, "state": "awaiting_approval" }).to_string())
                    }
                    "settled" => {}
                    other => return Step::Done(err(format!("unexpected approval state {other:?}"))),
                }
                if st["reason"].as_str() != Some("approved") {
                    let reason = st["reason"].as_str().unwrap_or("settled").to_string();
                    self.jobs.lock().unwrap().remove(&self.request_id);
                    return Step::Done(json!({ "ok": true, "state": "declined", "reason": reason }).to_string());
                }
                self.stage = StatusStage::Fetch;
                Step::Call(Call::KeystoreFetchResult(self.request_id.clone(), self.receipt.clone()))
            }
            StatusStage::Fetch => {
                let fetched = match reply.and_then(ok_value) {
                    Ok(v) => v,
                    Err(e) => return Step::Done(err(e)),
                };
                // `signed`, which is what `fetch_result` answers:
                // `{ ok, signed: [...] }`. Reading `results` collected nothing,
                // every time, and no doctest caught it because they all stop at
                // the approval.
                let sigs: Vec<String> = fetched["signed"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                let Some(raw) = sigs.into_iter().next() else {
                    return Step::Done(err("keystore: approval produced no signatures"));
                };
                let Some(job) = self.jobs.lock().unwrap().remove(&self.request_id) else {
                    return Step::Done(err("unknown request"));
                };
                self.stage = StatusStage::Broadcast;
                self.pending = Some(job.record);
                Step::Call(Call::EthSendRawTransaction(job.chain_id as i64, raw))
            }
            StatusStage::Broadcast => {
                let bcast = match reply.and_then(ok_value) {
                    Ok(v) => v,
                    Err(e) => return Step::Done(err(e)),
                };
                let Some(hash) = bcast["hash"].as_str() else {
                    return Step::Done(err("broadcast: no hash"));
                };
                self.hash = hash.to_string();
                let mut record = self.pending.take().expect("the record taken off the parked job");
                record.hash = self.hash.clone();
                let from = record.from.clone();
                self.history.add(&from, record);
                emit_tx_status_changed(&self.hash);
                self.stage = StatusStage::Ack;
                // Tell the keystore we have them so it can wipe its copy. Best
                // effort: the transaction is already on the chain.
                Step::Call(Call::KeystoreAckResult(self.request_id.clone(), self.receipt.clone()))
            }
            StatusStage::Ack => Step::Done(
                json!({ "ok": true, "state": "done", "hash": self.hash }).to_string(),
            ),
        }
    }
}

impl WalletBackendModuleImpl {
    fn st(&mut self) -> std::result::Result<&mut State, String> {
        let st = self
            .state
            .as_mut()
            .ok_or_else(|| "backend not initialized (context not ready)".to_string())?;

        // THE CHAIN CONFIGS USED TO BE SENT FROM HERE on `web`, behind a
        // `configs_sent` flag, because the wasm host installed the outbound door
        // AFTER the setters that fire `on_context_ready` -- so the load-time
        // send was refused inline with nothing on the wire. The host opens the
        // door first now (logos-workspace#195), the hook sends them on every
        // target again, and this accessor is back to being an accessor.
        Ok(st)
    }

    fn save_watched(st: &State) {
        let _ = std::fs::write(
            st.dir.join("watched.json"),
            serde_json::to_string_pretty(&st.watched).unwrap_or_default(),
        );
    }

    fn save_balances(st: &State) {
        let _ = std::fs::write(
            st.dir.join("balances_cache.json"),
            serde_json::to_string_pretty(&*st.balances.lock().unwrap()).unwrap_or_default(),
        );
    }

    /// Push every configured chain's RPC + proxy config into eth_rpc.
    ///
    /// THE ONE PLACE THIS MODULE STILL SPELLS A CALL TWICE, and deliberately.
    /// Nothing reads the reply — `set_chain_config` answers a `bool` nobody
    /// looks at — so the async form loses no information. What it would lose is
    /// ORDER: a caller that does `set_chains(...)` and then `refresh_balances`
    /// expects eth_rpc to already know the endpoint, and on a native host the
    /// synchronous client and the async completion path are different
    /// mechanisms, so a queued config could still be in flight when the first
    /// balance read overtakes it. The native spelling therefore stays
    /// synchronous, which is what it always was.
    #[cfg(not(target_os = "emscripten"))]
    fn push_chain_configs(st: &State) {
        for c in &st.cfg.config().chains {
            if let Some(cfg) = st.cfg.eth_rpc_config(c.chain_id) {
                let _ = modules().eth_rpc_module.set_chain_config(c.chain_id as i64, &cfg.to_string());
            }
        }
    }

    /// The same push on a `web` (wasm) image, where there is no synchronous
    /// client to use.
    ///
    /// ONE AT A TIME, each from the last one's callback, rather than all at
    /// once. Two reasons, and the second is the one that costs if you get it
    /// wrong: the pushes stay strictly ordered, and the image runs the
    /// `capability_module.requestModule` handshake ONCE. The outbound door has
    /// no in-flight de-duplication — it checks the token store, and six calls
    /// fired before any grant has landed all find it empty and all ask
    /// (`wasm_lp_abi.cpp`, `lp_invoke_async`). Six handshakes to say one thing
    /// is a startup this module can simply not have.
    #[cfg(target_os = "emscripten")]
    fn push_chain_configs(st: &State) {
        let mut queue: Vec<(i64, String)> = st
            .cfg
            .config()
            .chains
            .iter()
            .filter_map(|c| st.cfg.eth_rpc_config(c.chain_id).map(|cfg| (c.chain_id as i64, cfg.to_string())))
            .collect();
        queue.reverse(); // `pop` takes the front
        push_next_chain_config(queue);
    }

    /// Combine cached balances (tokens with balance > 0) with Uniswap prices for
    /// the wallet's **Market** view. Per chain: pull held tokens from the cached
    /// aggregate, look up symbol/decimals from `token_list`, ask `uniswap_module`
    /// for token→ETH/USD prices (best-rate, one Multicall3 eth_call), and attach
    /// a `valueUsd` to each holding (and to native ETH). Pricing failures degrade
    /// gracefully to null prices — the holding still shows.
    fn build_market(&mut self, address: &str) -> std::result::Result<Value, String> {
        let cached = {
            let st = self.st()?;
            st.balances.lock().unwrap().get(address).cloned()
        };
        let Some(cached) = cached else {
            return Ok(json!({ "ok": true, "address": address, "chains": [] }));
        };
        let empty = Vec::new();
        let chains = cached.get("chains").and_then(Value::as_array).unwrap_or(&empty);

        let mut chains_out = Vec::new();
        for chain in chains {
            let chain_id = chain.get("chainId").and_then(Value::as_u64).unwrap_or(0);
            if chain_id == 0 {
                continue;
            }

            // symbol/decimals lookup (lowercased address -> (symbol, decimals)).
            let meta = self.cached_token_meta(chain_id);

            // Held tokens (balance > 0) → the set we price.
            let empty_toks = Vec::new();
            let toks = chain.get("tokens").and_then(Value::as_array).unwrap_or(&empty_toks);
            let mut held: Vec<(String, u8)> = Vec::new();
            for t in toks {
                let addr = t.get("address").and_then(Value::as_str).unwrap_or("");
                let bal = t.get("balance").and_then(Value::as_str).unwrap_or("0");
                if addr.is_empty() || parse_u256_str(bal).is_zero() {
                    continue;
                }
                let dec = meta.get(&addr.to_lowercase()).map(|m| m.1).unwrap_or(18);
                held.push((addr.to_string(), dec));
            }

            let prices = self.cached_prices(chain_id);

            // Native ETH item.
            let native_bal = chain.get("native").and_then(Value::as_str).unwrap_or("0");
            let eth_usd = prices.get("ETH").and_then(|p| p.1);
            let mut items = vec![json!({
                "address": "native",
                "symbol": "ETH",
                "decimals": 18,
                "balance": native_bal,
                "eth": 1.0,
                "usd": eth_usd,
                "valueUsd": value_usd(native_bal, 18, eth_usd),
            })];

            for (addr, dec) in &held {
                let (symbol, _) = meta.get(&addr.to_lowercase()).cloned().unwrap_or_else(|| (short_addr(addr), *dec));
                let bal = toks
                    .iter()
                    .find(|t| t.get("address").and_then(Value::as_str) == Some(addr.as_str()))
                    .and_then(|t| t.get("balance").and_then(Value::as_str))
                    .unwrap_or("0");
                let (eth, usd) = prices.get(addr.as_str()).copied().unwrap_or((None, None));
                items.push(json!({
                    "address": addr,
                    "symbol": symbol,
                    "decimals": dec,
                    "balance": bal,
                    "eth": eth,
                    "usd": usd,
                    "valueUsd": value_usd(bal, *dec, usd),
                }));
            }

            chains_out.push(json!({ "chainId": chain_id, "items": items }));
        }
        Ok(json!({ "ok": true, "address": address, "chains": chains_out }))
    }

    /// The symbols and decimals `refresh_market` cached for a chain.
    ///
    /// Was a synchronous `token_list.get_tokens` per chain per call. It is the
    /// SAME answer the fan-out already had to fetch to build its price request,
    /// so it is fetched once there and read here — which is what makes
    /// `get_market` an offline method on every target rather than a method that
    /// waits for one call per chain.
    fn cached_token_meta(&mut self, chain_id: u64) -> std::collections::HashMap<String, (String, u8)> {
        self.st()
            .ok()
            .and_then(|st| st.token_meta.lock().unwrap().get(&chain_id).cloned())
            .unwrap_or_default()
    }

    /// The token→(eth, usd) prices `refresh_market` cached for a chain, keyed by
    /// the address string it priced (plus an `"ETH"` entry for native). Empty
    /// until the fan-out has run, which prices every holding at `null`.
    fn cached_prices(&mut self, chain_id: u64) -> std::collections::HashMap<String, (Option<f64>, Option<f64>)> {
        self.st()
            .ok()
            .and_then(|st| st.market_prices.lock().unwrap().get(&chain_id).cloned())
            .unwrap_or_default()
    }

    /// Plan a send: everything that does not need the network, resolved before
    /// the first call goes out. `erc20` carries (token, amount) when set.
    fn plan_send(&mut self, p: &SendParams, erc20: Option<(Address, U256)>) -> std::result::Result<SendFlow, String> {
        let to_addr = parse_addr(&p.to)?;
        let jobs = Arc::clone(&self.st()?.jobs);
        let chain_id = p.chain_id;

        // Fees come from fee_module, which derives the tip from eth_feeHistory.
        // This used to be `max_fee = gas_price * 2, max_priority = gas_price` --
        // and since eth_gasPrice is roughly baseFee + tip, that tipped
        // approximately the whole base fee and cost exactly 2x on every send.
        // `tier` rides in on SendParams so a UI can offer slow/normal/fast; an
        // explicit maxFeePerGas/maxPriorityFeePerGas is passed straight through
        // and obeyed verbatim.
        let fee_req = json!({
            "tier": p.tier.clone().unwrap_or_else(|| "normal".into()),
            "maxFeePerGas": p.max_fee_per_gas.clone(),
            "maxPriorityFeePerGas": p.max_priority_fee_per_gas.clone(),
        })
        .to_string();

        // The from/to/value/data shape the gas estimate is run against.
        let (est_to, est_value, est_data, default_gas): (String, String, String, u64) = match &erc20 {
            Some((token, amount)) => {
                let data = txbuild::erc20_transfer_calldata(to_addr, *amount);
                (format!("{token}"), "0x0".into(), format!("0x{}", hex::encode(data)), 90_000)
            }
            None => (p.to.clone(), format!("0x{:x}", parse_u256_str(&p.amount)), "0x".into(), 21_000),
        };
        let est_tx =
            json!({ "from": p.from, "to": est_to, "value": est_value, "data": est_data }).to_string();

        Ok(SendFlow {
            stage: SendStage::Nonce,
            chain_id,
            from: p.from.clone(),
            to: p.to.clone(),
            to_addr,
            erc20,
            amount: erc20.map(|(_, a)| a).unwrap_or_else(|| parse_u256_str(&p.amount)),
            fee_req,
            est_tx,
            default_gas,
            jobs,
            nonce: 0,
            fee: None,
            gas_limit: default_gas,
        })
    }

    /// Parse a native send request and plan it. Shared by the two spellings of
    /// `send_native`, which differ only in how they drive the flow.
    fn plan_send_native(&mut self, send_json: &str) -> std::result::Result<SendFlow, String> {
        let p: SendParams = serde_json::from_str(send_json).map_err(|e| e.to_string())?;
        self.plan_send(&p, None)
    }

    /// Parse an ERC-20 send request and plan it. Shared by the two spellings of
    /// `send_erc20`, which differ only in how they drive the flow.
    fn plan_send_erc20(&mut self, send_json: &str) -> std::result::Result<SendFlow, String> {
        let p: SendParams = serde_json::from_str(send_json).map_err(|e| e.to_string())?;
        let token = parse_addr(&p.token_address)?;
        let amount = parse_u256_str(&p.amount);
        self.plan_send(&p, Some((token, amount)))
    }

    /// Plan a `send_status` poll. The receipt is read here, under `&mut self`,
    /// so the flow itself never has to reach back into module state for it.
    fn plan_send_status(&mut self, request_id: &str) -> std::result::Result<StatusFlow, String> {
        let st = self.st()?;
        let jobs = Arc::clone(&st.jobs);
        let history = Arc::clone(&st.history);
        let receipt = match jobs.lock().unwrap().get(request_id) {
            Some(job) => job.receipt.clone(),
            None => return Err("unknown request".into()),
        };
        Ok(StatusFlow {
            stage: StatusStage::Status,
            request_id: request_id.to_string(),
            receipt,
            jobs,
            history,
            pending: None,
            hash: String::new(),
        })
    }

    /// Plan the label-on-import: one keystore call, then a local labels.json
    /// write that the reply is passed through unchanged around.
    fn plan_import_mnemonic(&mut self, phrase_json: String, label: String) -> impl Flow {
        let dir = self.st().ok().map(|st| st.dir.clone());
        one_call(Call::KeystoreImportMnemonic(phrase_json), move |reply| match reply {
            Ok(keystore_reply) => label_imported_account(dir, keystore_reply, label),
            Err(e) => err(e),
        })
    }

    /// Plan a receipt poll: one eth_rpc call, then a local history update.
    fn plan_refresh_tx_status(&mut self, hash_hex: String, chain_id: i64) -> impl Flow {
        let state = self.st().ok().map(|st| (Arc::clone(&st.history), st.dir.clone()));
        one_call(Call::EthTransactionReceipt(chain_id, hash_hex.clone()), move |reply| {
            let receipt = match reply {
                Ok(s) => s,
                Err(e) => return err(e),
            };
            let v = match ok_value(receipt) {
                Ok(v) => v,
                Err(e) => return err(e),
            };
            // null result => still pending
            let status = match v.get("result") {
                Some(Value::Null) | None => "pending",
                Some(r) => {
                    if r.get("status").and_then(Value::as_str) == Some("0x1") {
                        "confirmed"
                    } else {
                        "failed"
                    }
                }
            };
            // update the owning account's record (search all known history files is
            // overkill; the UI passes the address-scoped call separately if needed).
            if status != "pending" {
                if let Some((history, dir)) = state {
                    // best-effort: update across the sender's file when present
                    for entry in std::fs::read_dir(dir.join("history")).into_iter().flatten().flatten() {
                        if let Some(name) = entry.file_name().to_str().and_then(|n| n.strip_suffix(".json")) {
                            if history.update_status(name, &hash_hex, status) {
                                break;
                            }
                        }
                    }
                }
            }
            emit_tx_status_changed(&hash_hex);
            json!({ "ok": true, "status": status }).to_string()
        })
    }
}

/// Push one chain's config, then the next, from its callback.
#[cfg(target_os = "emscripten")]
fn push_next_chain_config(mut queue: Vec<(i64, String)>) {
    let Some((chain_id, cfg)) = queue.pop() else { return };
    modules()
        .eth_rpc_module
        .set_chain_config_async(chain_id, &cfg, move |_| push_next_chain_config(queue));
}

/// Persist an address->label from a keystore reply, and pass the reply on
/// unchanged. Import is the only caller left: creating an account became
/// Tier D and left this module's contract with it.
///
/// A free function taking the directory rather than a method: it runs in the
/// callback of the import call, by which time the method that asked has
/// returned and `&self` is long gone. `None` = the context was never ready, in
/// which case the label is dropped and the reply still passed through, exactly
/// as before.
fn label_imported_account(dir: Option<std::path::PathBuf>, keystore_reply: String, label: String) -> String {
    let v = match ok_value(keystore_reply.clone()) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    if let Some(addr) = v.get("address").and_then(Value::as_str) {
        if let Some(dir) = dir {
            let p = dir.join("labels.json");
            let mut labels: std::collections::HashMap<String, String> =
                std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
            labels.insert(addr.to_lowercase(), label);
            let _ = std::fs::write(p, serde_json::to_string_pretty(&labels).unwrap_or_default());
        }
    }
    keystore_reply
}

impl WalletBackendModule for WalletBackendModuleImpl {
    fn on_context_ready(&mut self, ctx: &RustModuleContext) {
        let dir = std::path::PathBuf::from(&ctx.instance_persistence_path);
        let cfg = ConfigStore::with_path(dir.join("config.json"));
        let history = Arc::new(History::new(dir.clone()));
        let watched = std::fs::read_to_string(dir.join("watched.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let balances = Arc::new(Mutex::new(
            std::fs::read_to_string(dir.join("balances_cache.json"))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default(),
        ));
        let market_prices = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let token_meta = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let st = State {
            cfg,
            history,
            dir,
            watched,
            balances,
            market_prices,
            token_meta,
            jobs: Default::default(),
        };
        // EVERY TARGET, FROM THE HOOK. A `web` image can call out from here
        // since logos-workspace#195; the two spellings below differ only in
        // whether there is a synchronous client to use.
        Self::push_chain_configs(&st);
        self.state = Some(st);
    }

    fn set_proxy_config(&mut self, proxy_json: String) -> bool {
        let proxy: ProxySettings = match serde_json::from_str(&proxy_json) {
            Ok(p) => p,
            Err(_) => return false,
        };
        match self.st() {
            Ok(st) => {
                st.cfg.set_proxy(proxy);
                Self::push_chain_configs(st);
                true
            }
            Err(_) => false,
        }
    }

    fn get_proxy_config(&mut self) -> String {
        match self.st() {
            Ok(st) => json!({ "ok": true, "proxy": st.cfg.config().proxy }).to_string(),
            Err(e) => err(e),
        }
    }

    fn set_chains(&mut self, chains_json: String) -> bool {
        let chains: Vec<ChainInfo> = match serde_json::from_str(&chains_json) {
            Ok(c) => c,
            Err(_) => return false,
        };
        match self.st() {
            Ok(st) => {
                st.cfg.set_chains(chains);
                Self::push_chain_configs(st);
                true
            }
            Err(_) => false,
        }
    }

    fn get_chains(&mut self) -> String {
        match self.st() {
            Ok(st) => json!({ "ok": true, "chains": st.cfg.config().chains }).to_string(),
            Err(e) => err(e),
        }
    }

    fn test_endpoint(&mut self, chain_id: i64) -> String {
        answer(plan_test_endpoint(chain_id), "start_test_endpoint")
    }

    fn import_mnemonic(&mut self, phrase_json: String, label: String) -> String {
        answer(self.plan_import_mnemonic(phrase_json, label), "start_import_mnemonic")
    }

    fn list_accounts(&mut self) -> String {
        answer(plan_list_accounts(), "start_list_accounts")
    }

    fn set_watched_tokens(&mut self, chain_id: i64, addresses_json: String) -> bool {
        let addrs: Vec<String> = match serde_json::from_str(&addresses_json) {
            Ok(a) => a,
            Err(_) => return false,
        };
        match self.st() {
            Ok(st) => {
                st.watched.insert(chain_id as u64, addrs);
                Self::save_watched(st);
                true
            }
            Err(_) => false,
        }
    }

    fn get_watched_tokens(&mut self, chain_id: i64) -> String {
        match self.st() {
            Ok(st) => json!({ "ok": true, "tokens": st.watched.get(&(chain_id as u64)).cloned().unwrap_or_default() }).to_string(),
            Err(e) => err(e),
        }
    }

    fn get_tokens(&mut self, chain_id: i64) -> String {
        answer(plan_get_tokens(chain_id), "start_get_tokens")
    }

    /// `false` on a `web` image. A `bool` has no room for "ask again later", so
    /// the refusal that every other waiting method spells out in words is just
    /// the failure value here — which is why `start_add_custom_token` exists and
    /// why the trait doc says so.
    fn add_custom_token(&mut self, token_json: String) -> bool {
        #[cfg(target_os = "emscripten")]
        {
            let _ = token_json;
            false
        }
        #[cfg(not(target_os = "emscripten"))]
        {
            serde_json::from_str::<Value>(&run_waiting(plan_add_custom_token(token_json)))
                .ok()
                .and_then(|v| v["added"].as_bool())
                .unwrap_or(false)
        }
    }

    fn refresh_balances(&mut self, address: String) -> bool {
        let holder = match parse_addr(&address) {
            Ok(a) => a,
            Err(_) => return false,
        };
        // Snapshot what the fan-out needs from State, then drop the borrow: the
        // async callbacks fire later (on the event loop) and can't touch `&self`.
        let (specs, cache, dir) = {
            let st = match self.st() {
                Ok(s) => s,
                Err(_) => return false,
            };
            let chains: Vec<(u64, Option<String>)> =
                st.cfg.config().chains.iter().map(|c| (c.chain_id, c.multicall3_addr())).collect();
            let specs: Vec<(u64, Option<String>, Vec<String>)> = chains
                .into_iter()
                .map(|(id, mc)| (id, mc, st.watched.get(&id).cloned().unwrap_or_default()))
                .collect();
            (specs, Arc::clone(&st.balances), st.dir.clone())
        };

        // One concurrent task per chain (eth_rpc is concurrency:"multi", so they
        // overlap instead of serializing). Fire-and-return; the final completion
        // writes the cache and emits `balances_updated` — contract unchanged.
        let tasks: Vec<GatherTask<Value>> = specs
            .into_iter()
            .map(|(chain_id, mc, tokens)| {
                let holder_hex = address.clone();
                let t: GatherTask<Value> =
                    Box::new(move |done| fetch_chain_async(chain_id, holder, holder_hex, mc, tokens, done));
                t
            })
            .collect();

        let addr = address;
        gather(tasks, move |chains: Vec<Value>| {
            let aggregate = json!({ "address": addr, "chains": chains });
            {
                let mut map = cache.lock().unwrap();
                map.insert(addr.clone(), aggregate);
                let _ = std::fs::write(
                    dir.join("balances_cache.json"),
                    serde_json::to_string_pretty(&*map).unwrap_or_default(),
                );
            }
            emit_balances_updated(&addr);
        });
        true
    }

    fn refresh_market(&mut self, address: String) -> bool {
        let (cached, prices_cache, meta_cache) = {
            let st = match self.st() {
                Ok(s) => s,
                Err(_) => return false,
            };
            (
                st.balances.lock().unwrap().get(&address).cloned(),
                Arc::clone(&st.market_prices),
                Arc::clone(&st.token_meta),
            )
        };
        let Some(cached) = cached else {
            // No balances yet — nothing to price; signal done so the UI doesn't wait.
            emit_market_updated(&address);
            return true;
        };
        // Pull (chainId, tokens) from the cached aggregate (no &self borrow).
        let chain_data: Vec<(u64, Vec<Value>)> = cached
            .get("chains")
            .and_then(Value::as_array)
            .map(|chains| {
                chains
                    .iter()
                    .filter_map(|c| {
                        let id = c.get("chainId").and_then(Value::as_u64)?;
                        if id == 0 {
                            return None;
                        }
                        Some((id, c.get("tokens").and_then(Value::as_array).cloned().unwrap_or_default()))
                    })
                    .collect()
            })
            .unwrap_or_default();

        // One task per chain, and each task is now a two-step CHAIN rather than a
        // single call: `token_list.get_tokens` for the decimals (which the price
        // request cannot be built without), then `uniswap.get_prices` over the
        // held tokens. The second call is issued from the first one's callback,
        // so nothing here depends on reply order; the tasks themselves overlap,
        // and `gather` collects them into their own slots.
        //
        // The token metadata is kept as well as used: `get_market` reads it back
        // to label the holdings, which is the call it used to make itself.
        let tasks: Vec<GatherTask<MarketLeg>> = chain_data
            .into_iter()
            .map(|(chain_id, toks)| {
                let t: GatherTask<MarketLeg> = Box::new(move |done| {
                    modules().token_list_module.get_tokens_async(chain_id as i64, move |res| {
                        let meta = decode_token_meta(res.ok());
                        let held: Vec<Value> = toks
                            .iter()
                            .filter_map(|t| {
                                let addr = t.get("address").and_then(Value::as_str)?;
                                let bal = t.get("balance").and_then(Value::as_str).unwrap_or("0");
                                if addr.is_empty() || parse_u256_str(bal).is_zero() {
                                    return None;
                                }
                                let dec = meta.get(&addr.to_lowercase()).map(|m| m.1).unwrap_or(18);
                                Some(json!({ "address": addr, "decimals": dec }))
                            })
                            .collect();
                        let req = json!({ "tokens": held }).to_string();
                        modules().uniswap_module.get_prices_async(chain_id as i64, &req, move |res| {
                            done((chain_id, meta, decode_uniswap_prices(res.ok())));
                        });
                    });
                });
                t
            })
            .collect();

        let addr = address;
        gather(tasks, move |results: Vec<MarketLeg>| {
            {
                let mut prices = prices_cache.lock().unwrap();
                let mut meta = meta_cache.lock().unwrap();
                for (chain, chain_meta, chain_prices) in results {
                    prices.insert(chain, chain_prices);
                    meta.insert(chain, chain_meta);
                }
            }
            emit_market_updated(&addr);
        });
        true
    }

    fn get_balances(&mut self, address: String) -> String {
        match self.st() {
            Ok(st) => match st.balances.lock().unwrap().get(&address) {
                Some(v) => json!({ "ok": true, "balances": v }).to_string(),
                None => json!({ "ok": true, "balances": { "address": address, "chains": [] } }).to_string(),
            },
            Err(e) => err(e),
        }
    }

    fn get_market(&mut self, address: String) -> String {
        match self.build_market(&address) {
            Ok(v) => v.to_string(),
            Err(e) => err(e),
        }
    }

    fn estimate_fee(&mut self, send_json: String) -> String {
        answer(plan_estimate_fee(&send_json), "start_estimate_fee")
    }

    fn send_native(&mut self, send_json: String) -> String {
        match self.plan_send_native(&send_json) {
            Ok(flow) => answer(flow, "start_send_native"),
            Err(e) => err(e),
        }
    }

    fn send_erc20(&mut self, send_json: String) -> String {
        match self.plan_send_erc20(&send_json) {
            Ok(flow) => answer(flow, "start_send_erc20"),
            Err(e) => err(e),
        }
    }

    fn send_status(&mut self, request_id: String) -> String {
        match self.plan_send_status(&request_id) {
            Ok(flow) => answer(flow, "start_send_status"),
            Err(e) => err(e),
        }
    }

    fn get_history(&mut self, address: String) -> String {
        match self.st() {
            Ok(st) => json!({ "ok": true, "history": st.history.list(&address) }).to_string(),
            Err(e) => err(e),
        }
    }

    fn refresh_tx_status(&mut self, hash_hex: String, chain_id: i64) -> String {
        answer(self.plan_refresh_tx_status(hash_hex, chain_id), "start_refresh_tx_status")
    }

    // ── the async spelling ──────────────────────────────────────────────────

    fn start_test_endpoint(&mut self, chain_id: i64) -> String {
        start_flow(plan_test_endpoint(chain_id))
    }

    fn start_import_mnemonic(&mut self, phrase_json: String, label: String) -> String {
        start_flow(self.plan_import_mnemonic(phrase_json, label))
    }

    fn start_list_accounts(&mut self) -> String {
        start_flow(plan_list_accounts())
    }

    fn start_get_tokens(&mut self, chain_id: i64) -> String {
        start_flow(plan_get_tokens(chain_id))
    }

    fn start_add_custom_token(&mut self, token_json: String) -> String {
        start_flow(plan_add_custom_token(token_json))
    }

    fn start_estimate_fee(&mut self, send_json: String) -> String {
        start_flow(plan_estimate_fee(&send_json))
    }

    fn start_send_native(&mut self, send_json: String) -> String {
        match self.plan_send_native(&send_json) {
            Ok(flow) => start_flow(flow),
            Err(e) => err(e),
        }
    }

    fn start_send_erc20(&mut self, send_json: String) -> String {
        match self.plan_send_erc20(&send_json) {
            Ok(flow) => start_flow(flow),
            Err(e) => err(e),
        }
    }

    fn start_send_status(&mut self, request_id: String) -> String {
        match self.plan_send_status(&request_id) {
            Ok(flow) => start_flow(flow),
            Err(e) => err(e),
        }
    }

    fn start_refresh_tx_status(&mut self, hash_hex: String, chain_id: i64) -> String {
        start_flow(self.plan_refresh_tx_status(hash_hex, chain_id))
    }

    fn take_result(&mut self, job_id: String) -> String {
        match JOBS.take(&job_id) {
            Job::Ready(answer) => answer,
            Job::Pending => json!({ "ok": false, "pending": true, "jobId": job_id }).to_string(),
            Job::Unknown => err(format!(
                "unknown job '{job_id}': never started, already collected, or evicted after {} newer ones",
                jobs::CAPACITY
            )),
        }
    }
}

#[no_mangle]
pub extern "Rust" fn logos_module_install() {
    install::<WalletBackendModuleImpl>();
}

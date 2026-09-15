# logos-evm-wallet-backend-module

The **coordinator** for the Logos multi-chain EVM wallet (Rust, rust-first
cdylib). It depends on
[`eth_rpc_module`](https://github.com/logos-co/logos-evm-eth-rpc-module),
[`keystore_module`](https://github.com/logos-co/logos-evm-keystore-module), and
[`token_list_module`](https://github.com/logos-co/logos-evm-token-list-module),
and it also folds in the **tx-builder** (offline alloy ABI/tx construction).

It owns the central proxy + chain config (and pushes each chain's `{endpoint,
proxy, proxyRequired}` down into `eth_rpc`), fetches multi-chain balances with
**Multicall3** (one `eth_call` per chain, falling back to per-call), orchestrates
sends (build → sign → broadcast → record), and stores this wallet's own
transaction history (the only history available without a proprietary indexer).

## Contract (`WalletBackendModule`)

Config: `set_proxy_config`/`get_proxy_config`, `set_chains`/`get_chains`,
`test_endpoint`. Accounts: `import_mnemonic`, `list_accounts`,
`unlock`/`lock`. Watched tokens: `set_watched_tokens`/`get_watched_tokens`.
Tokens: `get_tokens`, `add_custom_token`. Balances: `refresh_balances`,
`get_balances`. Send: `estimate_fee`, `send_native`, `send_erc20`. History:
`get_history`, `refresh_tx_status`. Events: `balances_updated`,
`tx_status_changed`, `proxy_error`.

Every method that leaves the process has an async twin: `start_test_endpoint`,
`start_import_mnemonic`, `start_list_accounts`, `start_get_tokens`,
`start_add_custom_token`, `start_estimate_fee`, `start_send_native`,
`start_send_erc20`, `start_send_status`, `start_refresh_tx_status` — each answers
`{ok, jobId}` at once and the answer is collected with `take_result(jobId)`,
exactly once. **A `web` (wasm) image must use that spelling**: it has one Worker
and one event loop and cannot wait for a reply (ADR 0004), so the waiting twin
dispatches nothing there and answers an error naming its `start_*` form. See
`docs/specs.md` §3.0.

## Build & test

```bash
cd rust-lib && cargo test --no-default-features   # tx-builder, config, history, jobs
nix build .#install                                # -> result/modules/wallet_backend_module/
nix build .#web                                    # the emscripten image
nix flake check                                    # incl. checks.<system>.web-variant
```

`checks.<system>.web-variant` builds the `web` image and drives it from node
against stubbed dependencies: the four-call `send_native` chain (nonce → fee →
gas → a human), the four-call `send_status` chain that follows an approval, the
capability handshake, and the refusal a target grants no token for. It SKIPS with
a message when the pinned `logos-module-builder`/`logos-protocol` publish no
`web` output; force the real thing from the workspace, whose pins do:

```bash
ws test logos-evm-wallet-backend-module --local logos-evm-wallet-backend-module
```

Building the full module pulls the three dependency modules' published `.lidl`
contracts to generate the typed `modules().<dep>` clients. In the Logos workspace
they resolve via flake `follows`/`--override-input` to local checkouts.

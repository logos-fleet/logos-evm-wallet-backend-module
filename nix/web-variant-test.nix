# THE `web` VARIANT, DRIVEN THROUGH THE CALL CHAINS THAT DEFINE IT.
#
# This module is the wallet's COORDINATOR: it holds almost no answer of its own
# and composes five dependencies into the ones it gives. Inside a Wasm image
# that composition is the part that could not exist until logos-protocol grew an
# outbound door (#166) — and, unlike `uniswap_module` (#167), the interesting
# thing here is not that ONE call can go out. It is that a SEQUENCE can:
# `send_native` is a nonce, then a fee, then a gas estimate over a transaction
# built from both, then a human. Everything the module's own unit tests cover —
# RLP/EIP-1559 field construction, Multicall3 encode/decode, the history store,
# the job board — is pure and runs the same everywhere. What is NOT pure, and
# what nothing could ask before this file, is:
#
#   * whether the image can CALL OUT AT ALL. A wasm image holds no credential
#     for its dependencies at startup — the core pushes a module its own token
#     and its callers', never an outbound one — so the door runs a
#     `capability_module.requestModule` handshake before a target is dialled,
#     ONCE PER TARGET, and this module has five of them;
#
#   * whether a CHAIN of calls survives being turned inside out. Each of the
#     four calls behind `start_send_native` is issued from the previous one's
#     callback, on the completion path, after the method that asked returned.
#     The drive answers them one at a time and asserts each frame, so a flow
#     that skipped a step, reordered two, or lost the nonce between them shows
#     up as a wrong frame rather than as a plausible-looking answer;
#
#   * whether the REFUSAL is honest. The wasm door refuses to forward a call it
#     was not granted — unlike the native client, which forwards tokenless — so
#     a capability_module that grants nothing must leave the target undialled
#     AND must tell the module so, naming the target, rather than leaving a job
#     pending for ever;
#
#   * and whether the WAITING spelling refuses here. `test_endpoint`,
#     `send_native`, `send_status` and the rest block for their replies, which
#     is safe on a native host and impossible in a Worker (one event loop, no
#     ASYNCIFY — ADR 0004). On this target they must dispatch NOTHING and say
#     which method to call instead;
#
#   * and whether the STARTUP is one handshake rather than six.
#     `on_context_ready` pushes every seeded chain's endpoint into eth_rpc, and
#     the outbound door has no in-flight de-duplication: six pushes fired at
#     once would all find the token store empty and all ask capability_module
#     for the same grant. The emscripten spelling of `push_chain_configs`
#     therefore chains them, and this drive counts the frames that proves it.
#
# THE HARNESS IS THE FAR SIDE OF THE CHANNEL, which is what makes all of that
# checkable without a browser, a container or five other modules: the image's
# one message port is `Module.logosOut`, so a frame the image SENDS is an entry
# in a transcript and a reply is one `logos_wasm_deliver` away. The dependencies
# are present here only as the contracts their `.lidl`s publish; node answers
# for them, in the answer shapes their own glue produces.
#
# WHAT IS NOT ASSERTED, deliberately: that the history file a completed send
# writes survives a page reload. The store persists with plain `std::fs`, which
# in an image reaches the durable medium only when somebody calls the storage
# barrier — so on this target a recorded transaction lives for the life of the
# page. That is a change to the module's storage, not to its call sites, and it
# is not what this issue is about.
#
# WHAT DOES THE DRIVING is `web-variant-drive.js` beside this file. This
# derivation only builds the variant, puts the two beside each other and runs
# them.
{ pkgs, webVariant }:

pkgs.runCommand "wallet-backend-web-variant-tests" {
  nativeBuildInputs = [ pkgs.nodejs ];
} ''
  set -euo pipefail
  variant=${webVariant}/wallet_backend_module_web
  test -s "$variant/wallet_backend_module_wasm.js" \
    || { echo "FAIL: no wasm host in the web variant"; exit 1; }

  # Side by side in the build directory: the harness requires the host by relative
  # path, and node resolves that against the SCRIPT's own directory. The image
  # itself is loaded by the glue relative to the same place.
  cp "$variant/wallet_backend_module_wasm.js" ./host.js
  cp "$variant/wallet_backend_module_wasm_image.wasm" ./wallet_backend_module_wasm_image.wasm
  cp ${./web-variant-drive.js} ./drive.js

  node drive.js
  mkdir -p $out
  echo ok > $out/result
''

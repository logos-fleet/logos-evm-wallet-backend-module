// THE CONTAINER'S JOB AND FIVE DEPENDENCIES', ALL DONE IN NODE.
//
// `nix/web-variant-test.nix` builds the `web` variant, puts this file beside its
// wasm host and runs it; the header there says what this asserts and why the
// harness has to be the far side of the wire rather than five more modules.
//
// Every frame below is exactly what the Web container puts on the wire — a
// {type, payload} envelope with logos-protocol's MessageType tags — so nothing
// here stands in for the transport, only for the browser and for the
// dependencies.
'use strict';

const factory = require('./host.js');

const CALL = 1, RESULT = 2, METHODS = 7, METHODS_RESULT = 8;

const fail = (why, transcript) => {
  console.error('FAIL: ' + why);
  if (transcript) console.error(JSON.stringify(transcript, null, 2));
  process.exit(1);
};

// ── the fixture ─────────────────────────────────────────────────────────────
//
// One chain, one account, one send. Small on purpose: what is under test is the
// SHAPE of the call chain, and a second chain would only repeat it.
const CHAIN_ID = 31337;
const FROM = '0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266';
const TO = '0x70997970C51812dc3A010C7d01b50e0d17dc79C8';
const USDC = '0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48';

// Arbitrary, and therefore checkable: each of these can only reach the answer
// by being decoded out of the frame this drive sends.
const NONCE_HEX = '0x7';
const NONCE = 7;
const MAX_FEE = '30000000000';
const MAX_PRIORITY = '1500000000';
const GAS_HEX = '0x5398';        // 21400, deliberately not the 21000 default
const GAS = 21400;
const TX_HASH = '0x' + 'ab'.repeat(32);
const SIGNED_RAW = '0x02f8' + 'cd'.repeat(20);
const AMOUNT = '1000000000000000';  // 0.001 ETH in wei

// What the capability handshake mints per target, and therefore what every
// outbound frame to that target has to carry afterwards.
const token = (target) => 'tok-for-' + target;

// ── the dependencies' answer shapes ─────────────────────────────────────────
//
// Each is the envelope that dependency's own glue produces, because the module
// reads fields off it: a stub that answered a bare value would be testing a
// decoder nobody has.
const ethResult = (hex) => JSON.stringify({ ok: true, result: hex });
const feeQuote = () => JSON.stringify({
  ok: true, maxFeePerGas: MAX_FEE, maxPriorityFeePerGas: MAX_PRIORITY, gasLimit: String(GAS),
});
const tokenCatalogue = () => JSON.stringify({
  ok: true,
  tokens: [{ address: USDC, symbol: 'USDC', decimals: 6 }],
});

// ── one image, with its message port wired to an array ──────────────────────
//
// A second call to this is the page after a reload, and — more to the point
// here — an image that has NOT been granted any credential yet.
async function spawn() {
  const heard = [];
  let hello = null;
  const mod = await factory({
    logosOut: (text) => {
      let msg;
      try { msg = JSON.parse(text); } catch { return; }
      if (msg.logosWasmHost) hello = msg; else heard.push(msg);
    },
    print: () => {},
    printErr: (s) => console.error('[image] ' + s),
  });
  const deliver = mod.cwrap('logos_wasm_deliver', null, ['string']);
  let nextId = 0;
  let answered = 0;

  const image = {
    heard,
    hello: () => hello,
    send: (type, payload) => deliver(JSON.stringify({ type, payload })),
    // Frames the IMAGE sent, i.e. its outbound calls.
    outbound: () => heard.filter((m) => m.type === CALL),
    // ...and the ones no `answer*` helper has replied to yet. The drive answers
    // them strictly in order, so a step inserted below does not renumber the
    // steps after it.
    unanswered: () => image.outbound().slice(answered),
    take: () => {
      const frame = image.unanswered()[0];
      if (frame) answered += 1;
      return frame;
    },
    // Answers are synchronous on this transport: the image is driven on this
    // thread and the RESULT is already in `heard` when deliver() returns.
    call: (method, args) => {
      const id = ++nextId;
      image.send(CALL, { id, authToken: '', object: 'wallet_backend_module', method, args });
      const res = heard.find((m) => m.type === RESULT && m.payload.id === id);
      if (!res) fail(method + ' did not answer at all', heard);
      if (!res.payload.ok) fail(method + ' failed at the transport: ' + res.payload.err, heard);
      return res.payload.value;
    },
    json: (method, args) => {
      const raw = image.call(method, args);
      try { return JSON.parse(raw); } catch { fail(method + ' answered malformed JSON: ' + raw); }
    },
    // The interface the image publishes. Matched on TYPE rather than id: a
    // MethodsResult can only be the answer to the one Methods frame sent here.
    methods: () => {
      const id = ++nextId;
      image.send(METHODS, { id, authToken: '', object: 'wallet_backend_module' });
      const res = heard.find((m) => m.type === METHODS_RESULT);
      if (!res || !res.payload.ok) fail('the image answered no Methods', heard);
      return res.payload.methods;
    },
  };
  return image;
}

// The handshake the door runs the first time it dials a target it holds no
// credential for, answered with `grant`. `""` grants nothing, which is the
// refusal case.
function answerHandshake(image, target, grant) {
  const ask = image.take();
  if (!ask || ask.payload.object !== 'capability_module'
      || ask.payload.method !== 'requestModule') {
    fail('the image did not ask capability_module for a token before dialling '
         + target + ': ' + JSON.stringify(ask && ask.payload), image.heard);
  }
  if (ask.payload.args[1] !== target) {
    fail('the handshake named ' + JSON.stringify(ask.payload.args[1]) + ', not ' + target);
  }
  image.send(RESULT, { id: ask.payload.id, ok: true, value: grant });
}

// One outbound call, asserted frame by frame and then answered. `check` sees the
// frame's args; returning a string from it fails the run with that message.
function answerCall(image, target, method, value, check) {
  const call = image.take();
  if (!call || call.payload.object !== target || call.payload.method !== method) {
    fail('expected ' + target + '.' + method + ', the image sent: '
         + JSON.stringify(image.outbound().map((m) => m.payload.object + '.' + m.payload.method)),
         image.heard);
  }
  if (call.payload.authToken !== token(target)) {
    fail(target + '.' + method + ' carries authToken='
         + JSON.stringify(call.payload.authToken)
         + ', not the credential capability_module granted');
  }
  if (check) {
    const why = check(call.payload.args);
    if (why) fail(target + '.' + method + ': ' + why + ' -- args were '
                  + JSON.stringify(call.payload.args));
  }
  image.send(RESULT, { id: call.payload.id, ok: true, value });
  return call.payload.args;
}

// How many frames the image has sent that nothing has answered. `0` is "the
// image dispatched nothing"; `1` is "exactly the one call this step expects".
function expectUnanswered(image, count, why) {
  const pending = image.unanswered();
  if (pending.length !== count) {
    fail(why + ': ' + JSON.stringify(pending.map((m) => m.payload.object + '.' + m.payload.method)),
         image.heard);
  }
}

// `on_context_ready` sends every seeded chain's endpoint to eth_rpc, AT LOAD.
// The frames are therefore already on the wire when `spawn()` returns and this
// is called before the image has been asked to do anything -- which is the
// assertion, not an accident of ordering. The module used to defer the send to
// its first dispatch because the wasm host installed the outbound door after the
// setters that fire the hook, so a call made in it was refused inline with
// nothing on the wire (logos-workspace#195); draining here is what would fail if
// that ever came back.
//
// The emscripten spelling issues the configs one at a time, each from the last
// one's callback, so with a grant this is ONE handshake followed by one
// `set_chain_config` per chain, never two frames at once. With `""` it is one
// refused handshake per chain and no config call at all — each attempt is
// independent, so the chain keeps going and keeps being refused.
function drainStartupConfigs(image, grant) {
  let handshakes = 0;
  let configs = 0;
  for (;;) {
    const pending = image.unanswered();
    if (pending.length === 0) break;
    if (pending.length !== 1) {
      fail('startup sent ' + pending.length + ' frames at once; the configs must be '
           + 'issued one at a time or the door runs a handshake for each',
           pending.map((m) => m.payload));
    }
    if (pending[0].payload.object === 'capability_module') {
      answerHandshake(image, 'eth_rpc_module', grant);
      handshakes += 1;
    } else {
      answerCall(image, 'eth_rpc_module', 'set_chain_config', 'true');
      configs += 1;
    }
  }
  return { handshakes, configs };
}

(async () => {
  const a = await spawn();

  // ── the image is a live wallet_backend_module ─────────────────────────────
  const hello = a.hello();
  if (!hello || hello.logosWasmHost !== 'wallet_backend_module') {
    fail('the image did not announce itself: ' + JSON.stringify(hello));
  }
  const names = a.methods().map((m) => m.name).sort();
  for (const want of ['add_custom_token', 'estimate_fee', 'get_balances', 'get_chains',
                      'get_history', 'get_market', 'get_tokens', 'import_mnemonic',
                      'list_accounts', 'refresh_balances', 'refresh_market',
                      'refresh_tx_status', 'send_erc20', 'send_native', 'send_status',
                      'start_add_custom_token', 'start_estimate_fee', 'start_get_tokens',
                      'start_import_mnemonic', 'start_list_accounts',
                      'start_refresh_tx_status', 'start_send_erc20', 'start_send_native',
                      'start_send_status', 'start_test_endpoint', 'take_result',
                      'test_endpoint']) {
    if (!names.includes(want)) fail('the published interface is missing ' + want + ': ' + names);
  }

  // ── THE CONFIGS GO OUT AT LOAD, AND ONE HANDSHAKE CARRIES THEM ───────────
  //
  // Nothing has been DISPATCHED to this image yet: `spawn` instantiates it and
  // `hello`/`methods` above ask the transport, not the module. So every frame
  // drained here was put on the wire by `on_context_ready`, from inside the
  // host's main(). An empty drain is logos-workspace#195 returning.
  const startup = drainStartupConfigs(a, token('eth_rpc_module'));
  if (startup.handshakes === 0 && startup.configs === 0) {
    fail('on_context_ready put NOTHING on the wire: the outbound door was not '
         + 'open when the hook fired', a.heard);
  }
  const chains = a.json('get_chains', []);
  if (startup.configs < 2) {
    fail('startup configured ' + startup.configs + ' chains; the seeded default set '
         + 'is larger than that, so the config store did not load');
  }
  if (startup.handshakes !== 1) {
    fail('startup ran ' + startup.handshakes + ' capability handshakes for one target; '
         + 'the outbound door has no in-flight de-duplication, so the configs must be '
         + 'issued one at a time');
  }
  if (!chains.ok || (chains.chains || []).length !== startup.configs) {
    fail('the image configured ' + startup.configs + ' chains in eth_rpc but reports '
         + JSON.stringify((chains.chains || []).length));
  }
  console.log('PASS: the image serves wallet_backend_module, configured ' + startup.configs
              + ' seeded chains FROM on_context_ready and ran ONE capability handshake');

  // ── THE WAITING SPELLING IS REFUSED HERE, AND SAYS WHAT TO CALL INSTEAD ───
  //
  // One Worker, one event loop, no ASYNCIFY (ADR 0004): the thread that would
  // wait for the reply is the thread that must deliver it. These methods do not
  // dispatch on this target — a call whose reply can never be collected is
  // worse than none — and the whole point of the refusal is that it names the
  // method that does work.
  for (const [method, args, twin] of [
    ['test_endpoint', [CHAIN_ID], 'start_test_endpoint'],
    ['list_accounts', [], 'start_list_accounts'],
    ['get_tokens', [CHAIN_ID], 'start_get_tokens'],
    ['estimate_fee', [JSON.stringify({ from: FROM, to: TO, chainId: CHAIN_ID, amount: AMOUNT })],
     'start_estimate_fee'],
    ['send_native', [JSON.stringify({ from: FROM, to: TO, chainId: CHAIN_ID, amount: AMOUNT })],
     'start_send_native'],
  ]) {
    const refused = a.json(method, args);
    if (refused.ok !== false || !(refused.error || '').includes(twin)) {
      fail(method + ' did not refuse on wasm naming ' + twin + ': ' + JSON.stringify(refused));
    }
  }
  // `add_custom_token` answers a bare bool and has nowhere to put a sentence.
  if (a.call('add_custom_token', [JSON.stringify({ address: USDC })]) !== false) {
    fail('add_custom_token did not refuse on wasm');
  }
  expectUnanswered(a, 0, 'a waiting method dispatched a call it can never collect');
  console.log('PASS: every waiting method refuses on a `web` image, dispatches nothing, '
              + 'and names its async twin');

  // ── ONE CALL, END TO END: start_get_tokens ────────────────────────────────
  const gettingTokens = a.json('start_get_tokens', [CHAIN_ID]);
  if (!gettingTokens.ok || !gettingTokens.jobId) {
    fail('start_get_tokens: ' + JSON.stringify(gettingTokens));
  }
  // ASYNC, AND THE TRANSCRIPT SAYS SO: the call is on the wire and nothing has
  // answered it, so the job has no answer to give.
  const notYet = a.json('take_result', [gettingTokens.jobId]);
  if (notYet.pending !== true) {
    fail('the job answered before the dependency did -- the call is not async: '
         + JSON.stringify(notYet));
  }
  // token_list_module is a target this image has not dialled yet, so the door
  // runs its handshake first.
  answerHandshake(a, 'token_list_module', token('token_list_module'));
  answerCall(a, 'token_list_module', 'get_tokens', tokenCatalogue(),
             (args) => (args[0] === CHAIN_ID ? null : 'went to chain ' + args[0]));
  const tokens = a.json('take_result', [gettingTokens.jobId]);
  if (!tokens.ok || ((tokens.tokens || [])[0] || {}).symbol !== 'USDC') {
    fail('take_result did not carry token_list\'s catalogue: ' + JSON.stringify(tokens));
  }
  console.log('PASS: a `web` image dialled token_list_module and collected its answer');

  // ONCE, and the board says so rather than leaving a poller on a slot that
  // will never fill.
  const again = a.json('take_result', [gettingTokens.jobId]);
  if (again.ok !== false || again.pending === true) {
    fail('a collected job answered twice: ' + JSON.stringify(again));
  }
  console.log('PASS: a job is collected once, and a second collect is an error');

  // ── THE ACCEPTANCE CRITERION: a four-call CHAIN, in order ─────────────────
  //
  // nonce → fee → gas → a human. Each call is issued from the previous one's
  // callback, so the drive answers one at a time and asserts that nothing else
  // is on the wire while it does. A flow that fanned these out, or that lost
  // the nonce between two of them, cannot pass this.
  const sending = a.json('start_send_native', [JSON.stringify({
    from: FROM, to: TO, chainId: CHAIN_ID, amount: AMOUNT, tier: 'fast' })]);
  if (!sending.ok || !sending.jobId) fail('start_send_native: ' + JSON.stringify(sending));

  expectUnanswered(a, 1, 'the send fanned out instead of chaining');
  answerCall(a, 'eth_rpc_module', 'get_transaction_count', ethResult(NONCE_HEX),
             (args) => (args[0] === CHAIN_ID && args[1] === FROM
                        ? null : 'asked for the wrong account/chain'));

  expectUnanswered(a, 1, 'the fee quote was not the second call, alone');
  answerHandshake(a, 'fee_module', token('fee_module'));
  answerCall(a, 'fee_module', 'estimate', feeQuote(), (args) => {
    if (args[0] !== CHAIN_ID) return 'quoted the wrong chain';
    const req = JSON.parse(args[1]);
    // `tier` rides in from the caller, which is what lets a UI offer
    // slow/normal/fast; the module must not flatten it to its default.
    return req.tier === 'fast' ? null : 'lost the caller\'s tier: ' + req.tier;
  });

  expectUnanswered(a, 1, 'the gas estimate was not the third call, alone');
  answerCall(a, 'eth_rpc_module', 'estimate_gas', ethResult(GAS_HEX), (args) => {
    const tx = JSON.parse(args[1]);
    if (tx.from !== FROM || tx.to !== TO) return 'estimated the wrong transfer';
    if (BigInt(tx.value) !== BigInt(AMOUNT)) return 'estimated the wrong value: ' + tx.value;
    return null;
  });

  expectUnanswered(a, 1, 'the approval was not the fourth call, alone');
  answerHandshake(a, 'keystore_module', token('keystore_module'));
  answerCall(a, 'keystore_module', 'request_approval',
             JSON.stringify({ ok: true, handle: 'h-1', receipt: 'r-1' }), (args) => {
    const intent = JSON.parse(args[0]);
    if (intent.address !== FROM) return 'asked the wrong account to sign';
    const tx = (intent.legs || [])[0] && intent.legs[0].tx;
    if (!tx) return 'the intent carries no tx leg';
    // THE POINT OF THE WHOLE CHAIN: the transaction put in front of the human
    // carries the nonce from call 1, the fee from call 2 and the gas from
    // call 3, and each of those arrived in a separate callback.
    //
    // The leg is `txbuild`'s own shape: snake_case keys, 0x-hex quantities —
    // what `keystore_module` signs, so the drive reads it the way the keystore
    // does rather than the way a JSON-RPC node would.
    if (BigInt(tx.nonce) !== BigInt(NONCE)) {
      return 'the nonce from call 1 is not in the tx: ' + tx.nonce;
    }
    if (BigInt(tx.gas_limit) !== BigInt(GAS)) {
      return 'the gas from call 3 is not in the tx: ' + tx.gas_limit;
    }
    if (BigInt(tx.max_fee_per_gas) !== BigInt(MAX_FEE)) {
      return 'the fee from call 2 is not in the tx: ' + tx.max_fee_per_gas;
    }
    if (BigInt(tx.max_priority_fee_per_gas) !== BigInt(MAX_PRIORITY)) {
      return 'the tip from call 2 is not in the tx: ' + tx.max_priority_fee_per_gas;
    }
    if (BigInt(tx.value) !== BigInt(AMOUNT)) {
      return 'the caller\'s amount is not in the tx: ' + tx.value;
    }
    return null;
  });

  const sent = a.json('take_result', [sending.jobId]);
  if (!sent.ok || sent.pending !== true || sent.requestId !== 'h-1') {
    fail('the send did not park on the keystore handle: ' + JSON.stringify(sent));
  }
  expectUnanswered(a, 0, 'the send made a fifth call');
  console.log('PASS: a `web` image composed nonce -> fee -> gas -> approval in four '
              + 'chained calls and parked the send on request ' + sent.requestId);

  // The waiting `send_status` refuses too — asked with a REAL request id, so
  // that what refuses it is the target and not a plan that failed first.
  const waited = a.json('send_status', [sent.requestId]);
  if (waited.ok !== false || !/start_send_status/.test(waited.error || '')) {
    fail('send_status did not refuse on wasm naming its twin: ' + JSON.stringify(waited));
  }
  expectUnanswered(a, 0, 'the waiting send_status polled the keystore anyway');

  // ── THE SECOND CHAIN: the human said yes ─────────────────────────────────
  //
  // approval_status → fetch_result → send_raw_transaction → ack_result, and the
  // hash comes back out of the broadcast the drive answered.
  const driving = a.json('start_send_status', [sent.requestId]);
  if (!driving.ok || !driving.jobId) fail('start_send_status: ' + JSON.stringify(driving));

  answerCall(a, 'keystore_module', 'approval_status',
             JSON.stringify({ ok: true, state: 'settled', reason: 'approved' }),
             (args) => (args[0] === 'h-1' && args[1] === 'r-1'
                        ? null : 'polled with the wrong handle/receipt'));
  expectUnanswered(a, 1, 'the signature fetch was not the second call, alone');
  answerCall(a, 'keystore_module', 'fetch_result',
             JSON.stringify({ ok: true, signed: [SIGNED_RAW] }));
  expectUnanswered(a, 1, 'the broadcast was not the third call, alone');
  answerCall(a, 'eth_rpc_module', 'send_raw_transaction',
             JSON.stringify({ ok: true, hash: TX_HASH }),
             (args) => (args[1] === SIGNED_RAW ? null : 'broadcast something other than the '
                                                        + 'signature the keystore returned'));
  expectUnanswered(a, 1, 'the acknowledgement was not the fourth call, alone');
  answerCall(a, 'keystore_module', 'ack_result', 'true');

  const done = a.json('take_result', [driving.jobId]);
  if (!done.ok || done.state !== 'done' || done.hash !== TX_HASH) {
    fail('the approved send did not complete: ' + JSON.stringify(done));
  }
  console.log('PASS: an approved send was fetched, broadcast and acknowledged in wasm '
              + '(' + done.hash.slice(0, 12) + '...)');

  // ...and it is in this wallet's history, which is the module's own state and
  // not something a dependency answered for it.
  const history = a.json('get_history', [FROM]);
  const recorded = (history.history || []).find((r) => r.hash === TX_HASH);
  if (!recorded || recorded.status !== 'pending' || recorded.to !== TO) {
    fail('the broadcast tx was not recorded in the local history: ' + JSON.stringify(history));
  }
  console.log('PASS: the broadcast transaction was recorded in the local history');

  // ── A DECLINED APPROVAL IS NOT A FAILURE ─────────────────────────────────
  //
  // ...and it must free the parked job rather than leave a poller on it.
  const second = a.json('start_send_native', [JSON.stringify({
    from: FROM, to: TO, chainId: CHAIN_ID, amount: AMOUNT })]);
  answerCall(a, 'eth_rpc_module', 'get_transaction_count', ethResult(NONCE_HEX));
  answerCall(a, 'fee_module', 'estimate', feeQuote());
  answerCall(a, 'eth_rpc_module', 'estimate_gas', ethResult(GAS_HEX));
  answerCall(a, 'keystore_module', 'request_approval',
             JSON.stringify({ ok: true, handle: 'h-2', receipt: 'r-2' }));
  if (a.json('take_result', [second.jobId]).requestId !== 'h-2') {
    fail('the second send did not park');
  }

  const declining = a.json('start_send_status', ['h-2']);
  answerCall(a, 'keystore_module', 'approval_status',
             JSON.stringify({ ok: true, state: 'settled', reason: 'rejected' }));
  const declined = a.json('take_result', [declining.jobId]);
  if (!declined.ok || declined.state !== 'declined' || declined.reason !== 'rejected') {
    fail('a refused approval was not reported as declined: ' + JSON.stringify(declined));
  }
  expectUnanswered(a, 0, 'a declined approval still fetched signatures');
  // The job is gone, so a second poll finds nothing to drive.
  const stale = a.json('start_send_status', ['h-2']);
  if (stale.ok !== false || !/unknown request/.test(stale.error || '')) {
    fail('a declined request was not released: ' + JSON.stringify(stale));
  }
  console.log('PASS: a declined approval stops the chain, releases the job and fetches '
              + 'no signatures');

  // ── THE REFUSAL, which is the security-relevant half ─────────────────────
  //
  // A fresh image, because the one above now holds credentials. The door
  // REFUSES to forward a call it was not granted — unlike the native client,
  // which forwards tokenless — so a capability_module that grants nothing
  // leaves the target undialled, and the module hears about it in its own
  // callback rather than from whatever the far side happens to check.
  const b = await spawn();
  // Its load-time chain configs are refused first, and the chain stops there:
  // one refused handshake per chain, no `set_chain_config` frame at all.
  const refusedStartup = drainStartupConfigs(b, '');
  if (refusedStartup.configs !== 0) {
    fail('eth_rpc was configured ' + refusedStartup.configs + ' times without a token');
  }
  expectUnanswered(b, 0, 'eth_rpc was dialled without a token');
  const refusedJob = b.json('start_get_tokens', [CHAIN_ID]);
  if (!refusedJob.ok || !refusedJob.jobId) {
    fail('start_get_tokens (b): ' + JSON.stringify(refusedJob));
  }
  answerHandshake(b, 'token_list_module', '');
  expectUnanswered(b, 0, 'the target was dialled without a token');
  const refused = b.json('take_result', [refusedJob.jobId]);
  if (refused.ok !== false || !/token_list_module/.test(refused.error || '')) {
    fail('an ungranted call was not reported to the module as a failure naming the '
         + 'target: ' + JSON.stringify(refused), b.heard);
  }
  console.log('PASS: a target this image holds no token for is refused, not forwarded, '
              + 'and the job carries the refusal');
})().catch((e) => fail('the harness threw: ' + ((e && e.stack) || e)));

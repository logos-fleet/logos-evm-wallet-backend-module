{
  description = "Logos wallet backend module — coordinator + tx builder (multi-chain balances via Multicall3, send orchestration, local history).";

  inputs = {
    logos-module-builder.url = "github:logos-co/logos-module-builder";

    # Dependency modules. Their published `.lidl` contracts drive the generated
    # `modules().<dep>` typed clients. The `follows` makes each dependency use the
    # SAME module-builder as this module (so `codegen.rust.source` is supported).
    # In the workspace these resolve via `follows`/`--override-input` to the local
    # checkouts; standalone they build once the dependency repos' default branches
    # carry this code.
    eth_rpc_module = {
      url = "github:logos-co/logos-evm-eth-rpc-module";
      inputs.logos-module-builder.follows = "logos-module-builder";
    };
    fee_module = {
      url = "github:logos-co/logos-evm-fee-module";
      inputs.logos-module-builder.follows = "logos-module-builder";
      inputs.eth_rpc_module.follows = "eth_rpc_module";
    };
    keystore_module = {
      url = "github:logos-co/logos-evm-keystore-module";
      inputs.logos-module-builder.follows = "logos-module-builder";
    };
    token_list_module = {
      url = "github:logos-co/logos-evm-token-list-module";
      inputs.logos-module-builder.follows = "logos-module-builder";
    };
    uniswap_module = {
      url = "github:logos-co/logos-evm-uniswap-module";
      inputs.logos-module-builder.follows = "logos-module-builder";
      inputs.eth_rpc_module.follows = "eth_rpc_module";
    };
  };

  outputs = inputs@{ self, logos-module-builder, ... }:
    let
      nixpkgs = logos-module-builder.inputs.nixpkgs;
      systems = [ "aarch64-darwin" "x86_64-darwin" "aarch64-linux" "x86_64-linux" ];

      # ONE module, answered for every target at once — mkLogosModule already
      # keys its own outputs by system, so calling it inside a genAttrs would
      # evaluate the same module once per target and throw all but one away.
      module = logos-module-builder.lib.mkLogosModule {
        src = ./.;
        configFile = ./metadata.json;
        flakeInputs = inputs;
      };

      # The mobile pseudo-systems logos-nix keys its cross package sets by. Kept
      # out of `systems` above for the reason the builder keeps them out of its
      # own: a phone gets the Bare image and none of the other outputs.
      #
      # THE PREREQUISITE FOR BUNDLING wallet_backend (#148). A phone's Bundled
      # set is resolved out of a catalog whose every entry is a module's own
      # `mobile.<target>.bare`, so a module with no mobile output cannot be in
      # that set however well it builds on a desktop — which is why the wallet
      # UI's `web` variant had nothing to ask for a fee, a send or a history,
      # and its Send and History tabs printed "wallet_backend_module ... has no
      # mobile build".
      #
      # IT IS NOT THE WHOLE OF IT, and this flake cannot be. `--bundle
      # wallet_backend_module` resolves a CLOSURE, and two of the five
      # dependencies in it do not cross yet: keystore_module publishes no mobile
      # targets at all, and fee_module is not a workspace repo. So
      # logos-basecamp's mobile catalog carries no wallet_backend_module entry
      # — eth_rpc_module, token_list_module and uniswap_module are in it and
      # this module is deliberately not — and it must not gain one before those
      # two land: a catalog entry whose closure cannot be resolved is a BROKEN
      # Bundled set rather than a missing one. What this flake does is stop
      # being the piece that is missing; the catalog entry is a later one.
      #
      # NOTHING HAD TO CHANGE IN THE MODULE to cross, and that is the point of
      # this being a one-line absence rather than a port. This crate is the
      # OFFLINE half of the wallet: `alloy` with default features off
      # (`std` + `sol-types`) for ABI encoding and 256-bit arithmetic, plus
      # hex/serde/serde_json. It opens no socket and signs nothing itself — every
      # chain read goes out through `modules().eth_rpc_module` and every
      # signature through `modules().keystore_module`, both of which are
      # module-to-module calls the phone's native transport already carries. So
      # there is no C dependency, no TLS stack and no `nix.external_libraries`
      # to cross, on either phone.
      #
      # `? ${t}` rather than a bare index, so a logos-module-builder pin without
      # the mobile cross sets leaves this flake simply WITHOUT mobile keys
      # instead of failing to evaluate.
      mobileTargets = builtins.filter (t: module.packages ? ${t})
        [ "aarch64-ios" "aarch64-ios-simulator" "aarch64-android" ];
    in
    {
      packages = nixpkgs.lib.genAttrs (systems ++ mobileTargets)
        (target: module.packages.${target});

      # An Android cross derivation's `system` is its BUILD platform, so
      # `packages.aarch64-android` is pinned to the builder's canonical one
      # (x86_64-linux) and a Mac cannot realise it. The same artifact, reached
      # from whichever machine is doing the building:
      #   nix build .#legacyPackages.aarch64-darwin.mobile.aarch64-android.bare
      legacyPackages = module.legacyPackages or { };

      # THE MODULE'S OWN ANSWER ABOUT ITSELF, forwarded so a consumer flake can
      # read it without building anything. When logos-basecamp's mobile catalog
      # gains a wallet_backend_module entry it takes this module's `version`
      # and, above all, its `dependencies` from here rather than restating them:
      # a Bundled set resolves a CLOSURE out of the catalog entry, so `--bundle
      # wallet_backend_module` has to pull eth_rpc_module, keystore_module,
      # token_list_module, uniswap_module and fee_module in without naming any
      # of them — and a hand-copied list in a SIGNED manifest is a claim the
      # core would act on after it had drifted.
      # `configFor` is the per-target resolution of the same document; this
      # module has no `platforms` overlay, so the two agree everywhere.
      inherit (module) config configFor;

      # ── THE `web` (wasm) VARIANT, AND THE CHECK THAT DRIVES IT ─────────────
      #
      # `packages.<system>.web` is an emscripten image with the module's crate,
      # logos-protocol's wasm subset and a Wasm host linked into it — the form
      # this module takes in a webview, where there is no dlopen and no host to
      # dlopen into (ADR 0003). It exists for a module with FIVE dependencies
      # only because logos-protocol's wasm subset now implements the outbound
      # half of the C ABI (`lp_client_create` / `lp_invoke_async`), which is what
      # logos-module-builder's gate reads off the pin (`hasOutboundDoor`,
      # ADR 0009 gate 2).
      #
      # WHAT MADE THIS ONE WORK IS NOT A SPELLING CHANGE (#168). Every outbound
      # call site is now a `Flow` — a state machine over `Call` values, driven
      # with the synchronous clients on a native host and with the `_async` ones
      # here. `send_native` is FOUR calls, each issued from the previous one's
      # callback, and `send_status` is another four; a synchronous twin of either
      # is not a spelling a wasm image can have, because a Worker is a single
      # event loop with no ASYNCIFY (ADR 0004) and a call that blocked for its
      # reply would deadlock the loop that delivers it. That is why every waiting
      # method refuses on this target and names its `start_*` twin, and why
      # `start_*` + `take_result` is the shape the platform actually has.
      #
      # Keyed by `systems`: a check is BUILT and RUN here, so a cross target
      # would have nothing to run it on.
      #
      # A SKIP THAT SAYS SO when the builder publishes no `web` output for this
      # module — a pin whose logos-protocol has no wasm outbound door, or one
      # from before the builder could compile a Rust core to wasm32 at all. That
      # is a pin rollout, not a defect, and an absent check would be a green run
      # with a silently missing test.
      checks = nixpkgs.lib.genAttrs systems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          modulePkgs = module.packages.${system};
        in {
          web-variant =
            if modulePkgs ? web
            then import ./nix/web-variant-test.nix { inherit pkgs; webVariant = modulePkgs.web; }
            else pkgs.runCommand "wallet-backend-web-variant-tests-skipped" { } ''
              echo "SKIP: web-variant -- this pin publishes no \`web\` output for"
              echo "      wallet_backend_module. A module with dependencies gets one"
              echo "      only when logos-protocol's wasm subset carries the outbound"
              echo "      door (hasOutboundDoor). Force the workspace flake, whose"
              echo "      pins do carry it -- a bare --auto-local on a clean tree"
              echo "      builds this module's own lock and lands back here:"
              echo "        ws test logos-evm-wallet-backend-module \"
              echo "             --local logos-evm-wallet-backend-module"
              mkdir -p $out
              echo skipped > $out/result
            '';
        });
    };
}

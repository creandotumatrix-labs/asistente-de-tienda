# Session Handoff — Asistente de Tienda (PRD 3)

Context-priming doc to fork a new session. The crate lives in this folder and
persists; a fresh session can `Read` it directly.

## What this is
PRD 3 (Ecommerce/Retail Support + Sales Agent, "Asistente de Tienda") built as a
**self-contained Rust crate** — a grounded es-MX WhatsApp commerce agent. Stands
in for the PRD's unspecified "shared runtime" (the `00-platform-overview.md` /
shared runtime were NOT in the workspace).

## Decisions locked (from the opening clarifying questions)
- **Language:** Rust (chosen over the PRD's TS-flavored signatures).
- **Agent loop:** real Anthropic Messages API, tool-use, **API-only** (needs
  `ANTHROPIC_API_KEY`; no offline fallback by design).
- **Delivery:** one self-contained runnable repo (not a drop-in module).
- **HTTP:** `ureq` + rustls, **blocking** (no tokio/async). Deps: ureq 2.10,
  serde, serde_json, toml 0.8, clap 4.5, anyhow, dotenvy. No chrono (std-only date math).
- **Model default:** `claude-sonnet-4-6` (override: `--model` › `ANTHROPIC_MODEL`
  › `[agente].modelo` in store.toml).

## Layout
```
src/main.rs        CLI WhatsApp simulator (REPL, --debug tool-trace, --once)
src/agent.rs       tool-use loop (max 8 iters; drops unknown blocks before echo)
src/anthropic.rs   Messages API client; Block enum is internally-tagged + #[serde(other)]
src/prompt.rs      es-MX persona + guardrails, config-injected, includes today's date
src/tools/mod.rs   AppState, ToolSpec, registry (all_tools/enabled_tools), dispatch, str_field/u32_field
src/tools/*.rs     the 7 tools
src/config.rs      StoreConfig (white-label toml)
src/model.rs       Product/Variant/Order/EstadoPedido + Catalog loader + resolve_sku
src/date.rs        std-only Hinnant date math (today_utc, parse, days_between)
src/util.rs        normaliza (accent-fold), token search, hash_corto (FNV→base36)
config/store.toml  white-label surface
data/products.json catalog (variant-level SKUs)
data/orders.json   sample orders
tests/guardrails.rs  ~23 invariants over real seed data (no network)
scripts/verify_logic.py  independent Python oracle, same 19 invariants
Makefile           build/run/demo/test/verify/fmt/lint
```

## The 7 tools (guardrails enforced in CODE, prompt is backstop)
- `search_products` — grounded; `total:0` ⇒ doesn't exist (no fabrication).
- `check_inventory` — variant- or base-SKU stock truth.
- `check_shipping` — config table, city/CP match, national fallback.
- `get_order_status` — unknown id ⇒ `encontrado:false` (no invented guía).
- `start_return` — pure `decide_return(order, plazo, hoy)` → Elegible/NoEntregado/FueraDePlazo/PedidoNoEncontrado; store-level 30-day window.
- `create_order_link` — **refuses OOS/oversell**, returns real `alternativas` instead of a pay link; LLM never touches card data.
- `handoff_human` — deterministic ticket + summary.
`[flujos]` toggles gate tools at BOTH the advertised-schema and dispatch layers.

## Seed facts the demo/tests depend on
- `VTX-BLK` Mochila Vortex negro, stock 14, $1290 (the hero). `VTX-GRY` stock 0 (OOS demo). `VTX-AZL` stock 6.
- Fully-OOS product `BOT` (Botella Hidra). Apparel/shoes have tallas. `SON` has a 15-day per-product policy string (informational; eligibility uses store 30d).
- Orders: `10482` en_camino / guía TRACK-99213 / Guadalajara (hero). `10488` delivered 9d ago → return eligible. `10310` delivered 50d ago → out of window. `10500` pagado (not delivered). Fixed test "today" = 2026-06-29.
- Shipping: Guadalajara $99/2-3, CDMX $89/1-2, Monterrey $109/2-3, Sureste $159/4-5, default $149/4-6.

## Verification state
- `python3 scripts/verify_logic.py` → **19/19 PASS** (ran in-session, independent date oracle).
- Static review of the Rust → **LIKELY COMPILES**, no blocking issues; specifically confirmed the `#[serde(other)]` internally-tagged `Block` enum and ureq 2.10 API against upstream source.
- **NOT yet `cargo`-built.** Reason: this session's sandbox allowlist only reaches `api.anthropic.com` — crates.io/rustup/apt all blocked, no root. So no compiler + no crate fetch was possible here.

## First things to do in the forked session
1. `cargo build && cargo test && cargo clippy --all-targets -- -D warnings` on a real machine. Expect green; fix only if the toolchain surfaces something the static review couldn't.
2. Live smoke test: `cp .env.example .env` (add key) → `make demo` and `cargo run --bin tienda -- --debug`.
3. Eyeball the live es-MX tone/length vs. the PRD sample dialog; tune `prompt.rs` / `[persona].tono` if needed.

## Open / phase-2 (not built)
- Real WhatsApp Business API transport (currently a CLI simulator).
- Payment processor behind `create_order_link` (Conekta / Mercado Pago / Stripe) — today it builds a URL from `[pagos].pay_link_base`.
- Shopify/WooCommerce catalog sync (replaces seed JSON loaders).
- Proactive abandoned-cart templates, loyalty, Instagram channel, persistent RMA/handoff ticketing.
- Per-product return windows (model carries policy text; eligibility is store-level only).

## Constraints to remember (from global instructions)
- Don't modify/delete existing files or send messages without explicit per-action confirmation.
- Infra/integration code gets a dry-run/plan step before any apply.
- Never log/print secrets; `.env` is git-ignored.

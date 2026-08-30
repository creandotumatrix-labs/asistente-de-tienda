# 🛍️ Asistente de Tienda — Simulador CLI de Ventas y Soporte (es-MX)

Implementación de referencia del PRD 3. Un simulador de línea de comandos que responde preguntas de producto desde un **catálogo real**, revisa inventario y envío, consulta el estado de un pedido, inicia devoluciones dentro de política, y entrega un link de pago seguro — **sin alucinar specs, stock, precios ni datos de pedidos**.

Lo importante es el *grounding confiable*: si preguntas por algo que no está en el catálogo, lo dice; si preguntas por algo real, da specs, fotos, costo de envío y un link de pago; si preguntas "¿dónde está mi pedido?", hace una consulta en vivo. La alucinación se previene en **código**, no solo en el prompt.

*Reference implementation for PRD 3. A grounded CLI order assistant simulator that answers product questions from a real catalog, checks inventory and shipping, looks up order status, initiates returns within policy, and hands off a secure payment link — without hallucinating specs, stock, prices, or order data.*

> Crate de Rust autocontenido. Sustituye al runtime compartido referenciado en el PRD: catálogo/pedidos, las siete herramientas, la persona es-MX + guardrails, el loop de tool-use de Anthropic, y un simulador CLI para la demo.
>
> *Self-contained Rust crate standing in for the shared runtime referenced in the PRD.*

---

## Demo

![Demo en vivo — Asistente de Tienda](asistente-de-tienda-demo.gif)

- 🔴 **Demo en vivo (Railway):** [asistente-de-tienda-production.up.railway.app](https://asistente-de-tienda-production.up.railway.app/)
- 📄 **Detalles:** [asistente-de-tienda.vercel.app](https://asistente-de-tienda.vercel.app/)

---

## Quickstart

```bash
cp .env.example .env        # agrega tu ANTHROPIC_API_KEY
cargo run --bin tienda -- --debug
```

`--debug` imprime el trace de llamadas a herramientas en vivo (el flujo anotado del PRD). Luego:

```
cliente ▸ tienen la mochila Vortex en negro?
   → search_products({"query":"mochila Vortex","color":"negro"})
   ⇐ {"total":1,"resultados":[{"sku":"VTX",...,"precio_mxn":1290}]}
🛍 Asistente de Tienda ▸ Sí 🎒 La Mochila Vortex en negro está disponible — $1,290 ...
```

¿Sin API key todavía? Toda la capa de datos + guardrails es verificable offline:

```bash
make verify        # oráculo python3 sobre los datos semilla — 19 invariantes, sin red
make test           # cargo test — mismos invariantes en Rust
```

*No API key yet? The whole data + guardrail layer is still verifiable offline via `make verify` / `make test`.*

---

## La demo (el momento wow)

```bash
make demo
```

1. **Fuera de catálogo** → `search_products` devuelve `total:0` → el agente dice que no está disponible (prueba que no fabrica datos).
2. **Producto real** → specs, precio, fotos, cotización de `check_shipping`, y un link de pago seguro de `create_order_link`.
3. **Post-venta** → `get_order_status("10482")` → "en camino" en vivo, guía TRACK-99213.

También prueba: pide la Vortex en **gris** (sin stock) — se niega a venderla y ofrece los colores disponibles.

---

## Herramientas / Tools

| Tool | Retorna | Guardrail (en código) |
|------|---------|------------------------|
| `search_products` | filas de catálogo que hacen match | solo filas reales; `total:0` ⇒ "no existe" |
| `check_inventory` | stock de un SKU | verdad a nivel variante o producto |
| `check_shipping` | costo + ETA | de tabla de config; fallback nacional |
| `get_order_status` | estado del pedido + guía | id desconocido ⇒ `encontrado:false` |
| `start_return` | RMA o rechazo | **solo** si fue entregado y dentro de N días |
| `create_order_link` | link de pago seguro | **se niega** si no hay stock; ofrece alternativas |
| `handoff_human` | ticket + resumen | ruta de escalamiento para casos límite |

Dos capas protegen el grounding: el **prompt** (`src/prompt.rs`) declara las reglas, y la **capa de herramientas** (`src/tools/`) es el respaldo — p.ej. `create_order_link` devuelve `error:"sin_stock"` con alternativas reales en vez de un link de pago, así el modelo físicamente no puede vender lo que no existe. El LLM nunca ve ni maneja datos de tarjeta; el pago ocurre solo a través del link retornado.

---

## Superficie de configuración white-label

Cambia `config/store.toml` + `data/*.json` → nueva tienda en minutos.

- `config/store.toml` — identidad de la tienda, persona/tono, política y ventana de devoluciones, tabla de envíos, base del link de pago, **toggles por flujo** (`[flujos]`), branding, modelo + max_tokens.
- `data/products.json` — catálogo: SKUs a nivel variante, stock, precios, colores, tallas, fotos, política de devolución por producto.
- `data/orders.json` — pedidos de ejemplo que alimentan el flujo de soporte.

Los flujos desactivados en `[flujos]` nunca se anuncian al modelo **y** se rechazan en el dispatch (defensa en profundidad).

Precedencia de modelo: `--model` › `ANTHROPIC_MODEL` › `[agente].modelo` (default `claude-sonnet-4-6`).

---

## CLI

```
tienda [--config PATH] [--catalog PATH] [--orders PATH]
       [--model ID] [--once "msg"] [--debug] [--no-banner]
```

`--once` corre un solo mensaje no interactivo (usado por `make demo`).

---

## Estructura / Layout

```
src/
  main.rs            CLI simulator (REPL + tool-trace)
  agent.rs           Anthropic tool-use loop
  anthropic.rs       Messages API client (ureq/rustls)
  prompt.rs          es-MX persona + guardrails (config-injected)
  tools/             the 7 tools + registry/dispatch (guardrails in code)
  config.rs model.rs date.rs util.rs
config/store.toml    white-label config
data/*.json          catalog + orders
tests/guardrails.rs  grounding/guardrail invariants (no network)
scripts/verify_logic.py  executable logic oracle (no toolchain needed)
```

---

## Verificación / Verification

- `cargo test` — suite de guardrails sobre los datos semilla reales (sin red, sin LLM).
- `make verify` — oráculo Python independiente que reimplementa la misma lógica con una implementación de fechas separada; verifica los mismos 19 invariantes. Útil en CI o donde no esté el toolchain de Rust.

El loop del agente (`anthropic.rs`/`agent.rs`) se ejerce en vivo contra la API; la capa determinística de herramientas + grounding es lo que fijan los tests.

---

## Fuera de alcance (fase 2) / Out of scope (phase 2)

Transporte real por WhatsApp Business API, integración con Shopify/WooCommerce + procesador de pagos (Stripe/Mercado Pago/Conekta) detrás de `create_order_link`, templates proactivos de carrito abandonado, lealtad, canal de Instagram, y ticketing persistente de RMA/handoff. Las siete herramientas son read-mostly sobre JSON semilla hoy; cada una mapea 1:1 a un punto de integración de fase 2.

*WhatsApp Business API transport, Shopify/WooCommerce + payment processor integration, abandoned-cart templates, loyalty, Instagram, and persistent RMA/handoff ticketing are all phase 2 — not built yet.*

---

## Preguntas frecuentes / FAQ

**¿Ya funciona por WhatsApp?** — No todavía. Hoy es un simulador CLI que prueba la capa de grounding y las 7 herramientas; el transporte de WhatsApp Business API es fase 2 (ver arriba). El demo en vivo corre la misma lógica vía web, no WhatsApp.
*Not yet — today this is a CLI simulator proving the grounding layer and the 7 tools; WhatsApp transport is phase 2.*

**¿Cuánto cuesta implementarlo para mi tienda?** — Depende del alcance (catálogo, integraciones de pago/envío, canal). Escríbenos vía [creandotumatrix.com](https://creandotumatrix.com) para una cotización.
*Depends on scope — contact us via creandotumatrix.com for a quote.*

**¿Puede vender algo que no está en el catálogo o inventar un precio?** — No. `search_products` solo devuelve filas reales; `total:0` es una respuesta válida y el agente la usa. El guardrail está en el código de las herramientas, no solo en el prompt.
*No — the guardrail against fabricated products/prices is enforced in the tool layer, not just the prompt.*

**¿Ve el agente los datos de mi tarjeta?** — No. El LLM nunca maneja datos de pago; solo entrega un link seguro generado por `create_order_link`.
*No — the LLM never handles payment data, it only hands off a secure link.*

---

## Asistentes CTM — la familia / the family

Los tres agentes de WhatsApp de **Creando Tu Matrix**, todos sobre el mismo patrón: runtime de tool-use con Claude, guardrails determinísticos en código, y una superficie de configuración white-label por negocio.

| Agente | Qué hace | Repo |
|---|---|---|
| 🌮 **asistente-pedidos** | Pedidos y reservaciones por WhatsApp para restaurantes | [creandotumatrix-labs/asistente-pedidos](https://github.com/creandotumatrix-labs/asistente-pedidos) |
| 🛍️ **asistente-de-tienda** | Soporte y ventas de retail/ecommerce, sobre catálogo real | [creandotumatrix-labs/asistente-de-tienda](https://github.com/creandotumatrix-labs/asistente-de-tienda) |
| 📈 **asistente-comercial** | Calificación y agendado de leads, agnóstico al vertical | [creandotumatrix-labs/asistente-comercial](https://github.com/creandotumatrix-labs/asistente-comercial) |

*The three Creando Tu Matrix WhatsApp agents, all on the same pattern: a Claude tool-use runtime, deterministic guardrails in code, and a per-business white-label config surface.*

🌐 Más sobre CTM: [creandotumatrix.com](https://creandotumatrix.com) · Org: [creandotumatrix-labs](https://github.com/creandotumatrix-labs)

---

Construido por [Marcus Patman](https://github.com/marcuspat) — Principal Agentic Engineer · Parte de **Asistentes CTM** en [creandotumatrix-labs](https://github.com/creandotumatrix-labs)

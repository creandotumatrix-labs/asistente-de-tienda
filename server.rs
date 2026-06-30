//! HTTP service wrapper around the grounded agent, for container/Railway deploys.
//! Synchronous (tiny_http) to match the blocking agent loop — no async runtime.
//!
//! Endpoints:
//!   GET  /health   -> 200 liveness + config/db status (always healthy)
//!   GET  /         -> info page + a minimal WhatsApp-style chat box
//!   GET  /history  -> last chat turns persisted in Postgres
//!   POST /chat     -> { "mensaje": "..." } -> drives the agent, returns { reply, trace }
//!                     and best-effort logs the turn to Postgres
//!
//! Env: PORT (Railway), ANTHROPIC_API_KEY (required for /chat), ANTHROPIC_MODEL (optional),
//!      DATABASE_URL / DATABASE_PRIVATE_URL (Postgres; connection is best-effort so a DB
//!      hiccup never takes the service down).

use std::io::Read;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use asistente_de_tienda::{
    agent::Agent,
    anthropic::Client,
    config::StoreConfig,
    model::{Catalog, Product, Variant},
    tools::{self, AppState},
};

fn main() {
    let _ = dotenvy::dotenv();

    let config = StoreConfig::load(Path::new("config/store.toml"))
        .unwrap_or_else(|e| fatal(&format!("config: {e:#}")));

    // Catalog comes from a real external product API (DummyJSON by default),
    // mapped into the domain model. Falls back to seed JSON if unreachable.
    let (catalog, catalog_source) = load_catalog();

    let model = std::env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| config.agente.modelo.clone());
    let max_tokens = config.agente.max_tokens;
    let state = AppState::new(config, catalog);
    let n_tools = tools::enabled_tools(&state).len();

    // Best-effort Postgres connection (bounded so a bad URL can't hang boot).
    let mut db = connect_db();

    let port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(8080);
    let addr = format!("0.0.0.0:{port}");
    let server = Server::http(&addr).unwrap_or_else(|e| fatal(&format!("bind {addr}: {e}")));
    eprintln!(
        "asistente-de-tienda escuchando en {addr} · modelo {model} · {n_tools} herramientas · db={}",
        db.is_some()
    );

    for req in server.incoming_requests() {
        let method = req.method().clone();
        let path = req.url().split('?').next().unwrap_or("/").to_string();

        match (method, path.as_str()) {
            (Method::Get, "/health") => {
                let body = json!({
                    "status": "ok",
                    "service": state.config.tienda.nombre,
                    "model": model,
                    "tools": n_tools,
                    "catalog_source": catalog_source,
                    "products": state.catalog.productos.len(),
                    "orders": state.catalog.pedidos.len(),
                    "anthropic_key": std::env::var("ANTHROPIC_API_KEY").is_ok(),
                    "database_url": std::env::var("DATABASE_URL").is_ok()
                        || std::env::var("DATABASE_PRIVATE_URL").is_ok(),
                    "db_connected": db.is_some(),
                });
                respond_json(req, 200, &body.to_string());
            }
            (Method::Get, "/") => {
                let _ = req.respond(
                    Response::from_string(index_html(&state.config.tienda.nombre))
                        .with_header(header("Content-Type", "text/html; charset=utf-8")),
                );
            }
            (Method::Get, "/history") => handle_history(&mut db, req),
            (Method::Post, "/chat") => handle_chat(&state, &model, max_tokens, &mut db, req),
            _ => respond_json(req, 404, &json!({ "error": "no encontrado" }).to_string()),
        }
    }
}

// ── Postgres (sync, best-effort) ────────────────────────────────────────────

fn connect_db() -> Option<postgres::Client> {
    let url = std::env::var("DATABASE_PRIVATE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()?;
    let mut cfg: postgres::Config = match url.parse() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("db: URL inválida: {e}");
            return None;
        }
    };
    cfg.connect_timeout(Duration::from_secs(5));
    match cfg.connect(postgres::NoTls) {
        Ok(mut c) => match c.batch_execute(
            "CREATE TABLE IF NOT EXISTS chat_log (\
                 id BIGSERIAL PRIMARY KEY, \
                 ts TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 mensaje TEXT NOT NULL, \
                 reply TEXT NOT NULL, \
                 tools TEXT)",
        ) {
            Ok(()) => {
                eprintln!("db: conectado + migrado (chat_log)");
                Some(c)
            }
            Err(e) => {
                eprintln!("db: migración falló: {e}");
                None
            }
        },
        Err(e) => {
            eprintln!("db: conexión falló (continuo sin persistencia): {e}");
            None
        }
    }
}

fn log_turn(db: &mut Option<postgres::Client>, mensaje: &str, reply: &str, tools: &str) {
    if let Some(c) = db.as_mut() {
        if let Err(e) = c.execute(
            "INSERT INTO chat_log (mensaje, reply, tools) VALUES ($1, $2, $3)",
            &[&mensaje, &reply, &tools],
        ) {
            eprintln!("db: insert falló: {e}");
        }
    }
}

fn handle_history(db: &mut Option<postgres::Client>, req: Request) {
    let client = match db.as_mut() {
        Some(c) => c,
        None => return respond_json(req, 200, &json!({ "db": false, "items": [] }).to_string()),
    };
    match client.query(
        "SELECT mensaje, reply, COALESCE(tools,'') AS tools, \
         to_char(ts,'YYYY-MM-DD HH24:MI:SS') AS ts \
         FROM chat_log ORDER BY id DESC LIMIT 25",
        &[],
    ) {
        Ok(rows) => {
            let items: Vec<Value> = rows
                .iter()
                .map(|r| {
                    json!({
                        "mensaje": r.get::<_, String>("mensaje"),
                        "reply": r.get::<_, String>("reply"),
                        "tools": r.get::<_, String>("tools"),
                        "ts": r.get::<_, String>("ts"),
                    })
                })
                .collect();
            respond_json(
                req,
                200,
                &json!({ "db": true, "count": items.len(), "items": items }).to_string(),
            );
        }
        Err(e) => respond_json(req, 500, &json!({ "error": format!("{e}") }).to_string()),
    }
}

// ── Chat ────────────────────────────────────────────────────────────────────

fn handle_chat(
    state: &AppState,
    model: &str,
    max_tokens: u32,
    db: &mut Option<postgres::Client>,
    mut req: Request,
) {
    let mut body = String::new();
    if req.as_reader().read_to_string(&mut body).is_err() {
        return respond_json(req, 400, &json!({ "error": "cuerpo ilegible" }).to_string());
    }
    let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let mensaje = match parsed.get("mensaje").and_then(Value::as_str) {
        Some(m) if !m.trim().is_empty() => m.to_string(),
        _ => return respond_json(req, 400, &json!({ "error": "falta 'mensaje'" }).to_string()),
    };
    // Optional UI language ("en"/"es"): instruct the model per-turn, but log the
    // original message so /history stays clean. Catalog data stays Spanish-grounded.
    let lang = parsed.get("lang").and_then(Value::as_str).unwrap_or("es");
    let prompt_msg = if lang.eq_ignore_ascii_case("en") {
        format!(
            "[Reply in English. The catalog is in Spanish — keep product names, prices, \
             SKUs and tracking codes exactly as given.]\n\n{mensaje}"
        )
    } else {
        mensaje.clone()
    };

    let client = match Client::from_env() {
        Ok(c) => c,
        Err(_) => {
            return respond_json(
                req,
                503,
                &json!({ "error": "ANTHROPIC_API_KEY no configurada en el servidor" }).to_string(),
            )
        }
    };

    let mut agent = Agent::new(state, client, model.to_string(), max_tokens);
    match agent.send(&prompt_msg) {
        Ok(turn) => {
            let tools_used: Vec<&str> = turn.trace.iter().map(|t| t.name.as_str()).collect();
            let trace: Vec<Value> = turn
                .trace
                .iter()
                .map(|t| json!({ "tool": t.name, "input": t.input, "output": t.output }))
                .collect();
            let body = json!({ "reply": turn.reply, "trace": trace }).to_string();
            log_turn(db, &mensaje, &turn.reply, &tools_used.join(","));
            respond_json(req, 200, &body);
        }
        Err(e) => respond_json(req, 502, &json!({ "error": format!("{e:#}") }).to_string()),
    }
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).expect("cabecera válida")
}

fn respond_json(req: Request, code: u16, body: &str) {
    let _ = req.respond(
        Response::from_string(body)
            .with_status_code(code)
            .with_header(header("Content-Type", "application/json; charset=utf-8")),
    );
}

fn fatal(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(1);
}

// ── Real external catalog (DummyJSON → domain model) ────────────────────────

#[derive(Deserialize)]
struct DjResp {
    products: Vec<DjProduct>,
}

#[derive(Deserialize)]
struct DjProduct {
    id: u64,
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    price: f64,
    #[serde(default)]
    stock: u32,
    #[serde(default)]
    category: String,
    #[serde(default)]
    sku: Option<String>,
    #[serde(default)]
    thumbnail: Option<String>,
    #[serde(default)]
    images: Vec<String>,
}

/// Default real-data source. Override with CATALOG_API_URL, or "seed" for bundled JSON.
const DEFAULT_CATALOG_API: &str = "https://dummyjson.com/products?limit=100";
/// Rough USD→MXN factor so API prices read like a Mexican storefront.
const FX_USD_MXN: f64 = 17.5;

/// Load the catalog from the external product API (mapped to domain types); fall
/// back to seed JSON on any failure so the service always boots. Orders stay seed.
fn load_catalog() -> (Catalog, String) {
    let seed = || {
        Catalog::load(Path::new("data/products.json"), Path::new("data/orders.json"))
            .unwrap_or_else(|e| fatal(&format!("catalog seed: {e:#}")))
    };
    let url = std::env::var("CATALOG_API_URL").unwrap_or_else(|_| DEFAULT_CATALOG_API.to_string());
    if url.eq_ignore_ascii_case("seed") || url.trim().is_empty() {
        return (seed(), "seed".to_string());
    }
    match fetch_catalog_from_api(&url) {
        Ok(productos) if !productos.is_empty() => {
            let pedidos = Catalog::load(
                Path::new("data/products.json"),
                Path::new("data/orders.json"),
            )
            .map(|c| c.pedidos)
            .unwrap_or_default();
            eprintln!("catálogo: {} productos desde {url}", productos.len());
            (Catalog { productos, pedidos }, format!("api:{url}"))
        }
        Ok(_) => {
            eprintln!("catálogo API vacío; usando seed");
            (seed(), "seed (api vacío)".to_string())
        }
        Err(e) => {
            eprintln!("catálogo API falló ({e}); usando seed");
            (seed(), "seed (api falló)".to_string())
        }
    }
}

fn fetch_catalog_from_api(url: &str) -> Result<Vec<Product>, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(15))
        .build();
    let resp: DjResp = agent
        .get(url)
        .call()
        .map_err(|e| format!("GET: {e}"))?
        .into_json()
        .map_err(|e| format!("parse: {e}"))?;

    let productos = resp
        .products
        .into_iter()
        .map(|p| {
            let sku = p
                .sku
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| format!("DJ-{}", p.id));
            let mut fotos = p.images;
            if fotos.is_empty() {
                if let Some(t) = p.thumbnail {
                    fotos.push(t);
                }
            }
            let precio = (p.price * FX_USD_MXN).round().max(1.0) as u32;
            Product {
                sku: sku.clone(),
                nombre_es: p.title,
                categoria: p.category,
                precio_mxn: precio,
                descripcion_es: p.description,
                foto_url: fotos,
                politica_devolucion: None,
                variantes: vec![Variant {
                    sku,
                    color: "estándar".to_string(),
                    talla: None,
                    stock: p.stock,
                    precio_mxn: None,
                    foto_url: vec![],
                }],
            }
        })
        .collect();
    Ok(productos)
}

fn index_html(nombre: &str) -> String {
    const HTML: &str = r##"<!doctype html><html lang="es" id="html"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>__NOMBRE__ · Asistente</title>
<style>
body{font-family:system-ui,-apple-system,sans-serif;max-width:640px;margin:24px auto;padding:0 16px;background:#0b141a;color:#e9edef}
header{display:flex;justify-content:space-between;align-items:center;gap:12px}
h1{font-size:1.15rem;margin:.4rem 0}
.sub{color:#8696a0;margin:.2rem 0 .6rem}
.seg{display:inline-flex;border:1px solid #2a3942;border-radius:999px;overflow:hidden}
.seg button{background:transparent;color:#8696a0;border:0;padding:6px 13px;font-size:.85rem;cursor:pointer}
.seg button.on{background:#00a884;color:#fff}
#log .b{background:#202c33;border-radius:10px;padding:9px 13px;margin:7px 0;white-space:pre-wrap;display:inline-block;max-width:85%}
#log .me{text-align:right} #log .me .b{background:#005c4b}
form{display:flex;gap:8px;margin-top:12px} input{flex:1;font-size:1rem;padding:11px;border-radius:8px;border:0}
button.send{font-size:1rem;padding:11px 16px;border-radius:8px;border:0;background:#00a884;color:#fff;cursor:pointer}
</style></head><body>
<header>
  <h1>🛍 __NOMBRE__</h1>
  <div class="seg"><button id="bes" class="on">ES</button><button id="ben">EN</button></div>
</header>
<p class="sub" id="sub"></p>
<div id="log"></div>
<form id="f"><input id="m" autocomplete="off" autofocus><button class="send" id="send" type="submit"></button></form>
<script>
const I18N={
  es:{lang:'es',sub:'Asistente de la tienda. Pregunta por productos, envíos o tu pedido.',ph:'tienen la mochila Vortex en negro?',send:'Enviar',wait:'…',none:'(sin respuesta)'},
  en:{lang:'en',sub:'Store assistant. Ask about products, shipping or your order.',ph:'do you have the Vortex backpack in black?',send:'Send',wait:'…',none:'(no reply)'}
};
const $=id=>document.getElementById(id);
let lang=localStorage.getItem('lang')||'es';
function apply(){const t=I18N[lang];$('html').lang=t.lang;$('sub').textContent=t.sub;$('m').placeholder=t.ph;$('send').textContent=t.send;$('bes').classList.toggle('on',lang==='es');$('ben').classList.toggle('on',lang==='en');}
$('bes').onclick=()=>{lang='es';localStorage.setItem('lang',lang);apply();$('m').focus();};
$('ben').onclick=()=>{lang='en';localStorage.setItem('lang',lang);apply();$('m').focus();};
apply();
const log=$('log');
function add(t,who){const d=document.createElement('div');d.className=who;const b=document.createElement('div');b.className='b';b.textContent=t;d.appendChild(b);log.appendChild(d);window.scrollTo(0,9e9);return b;}
$('f').onsubmit=async e=>{e.preventDefault();const t=$('m').value.trim();if(!t)return;add(t,'me');$('m').value='';const b=add(I18N[lang].wait,'bot');
try{const r=await fetch('/chat',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({mensaje:t,lang:lang})});const j=await r.json();b.textContent=j.reply||j.error||I18N[lang].none;}
catch(err){b.textContent='error: '+err;}};
</script></body></html>"##;
    HTML.replace("__NOMBRE__", nombre)
}

//! HTTP service wrapper around the grounded agent, for container/Railway deploys.
//! Synchronous (tiny_http) to match the blocking agent loop — no async runtime.
//!
//! Data lives in **Postgres** (products / orders / order_items), seeded once from a
//! real external product API. Every /chat loads the catalog + orders live from the DB
//! (real queries per request). Seed JSON is only a last-resort fallback if the DB is
//! unreachable, so the service always boots.
//!
//! Endpoints:
//!   GET  /health   -> liveness + catalog source/counts + db status
//!   GET  /         -> bilingual (ES/EN) WhatsApp-style chat UI
//!   GET  /history  -> last chat turns persisted in Postgres
//!   POST /chat     -> { "mensaje": "...", "lang": "es"|"en" } -> drives the agent

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
    model::{Catalog, EstadoPedido, Order, OrderItem, Product, Variant},
    tools::{self, AppState},
};

fn main() {
    let _ = dotenvy::dotenv();

    let config = StoreConfig::load(Path::new("config/store.toml"))
        .unwrap_or_else(|e| fatal(&format!("config: {e:#}")));
    let model = std::env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| config.agente.modelo.clone());
    let max_tokens = config.agente.max_tokens;

    let mut db = connect_db();

    // Source of truth = Postgres. Migrate + (first-boot) seed from the product API,
    // then load the catalog from the DB. Fall back to API/seed JSON only if the DB
    // is unreachable so the service still boots.
    let (boot_catalog, catalog_source) = match db.as_mut() {
        Some(c) => match migrate_and_seed(c).and_then(|_| load_catalog_db(c)) {
            Ok(cat) if !cat.productos.is_empty() => (cat, "postgres".to_string()),
            Ok(_) => {
                eprintln!("db catálogo vacío; usando API/seed");
                (load_catalog().0, "api/seed (db vacío)".to_string())
            }
            Err(e) => {
                eprintln!("db catálogo falló ({e}); usando API/seed");
                (load_catalog().0, "api/seed (db error)".to_string())
            }
        },
        None => (load_catalog().0, "api/seed (sin db)".to_string()),
    };

    let state = AppState::new(config, boot_catalog);
    let n_tools = tools::enabled_tools(&state).len();

    let port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(8080);
    let addr = format!("0.0.0.0:{port}");
    let server = Server::http(&addr).unwrap_or_else(|e| fatal(&format!("bind {addr}: {e}")));
    eprintln!(
        "asistente-de-tienda :{port} · modelo {model} · {n_tools} tools · catálogo {} ({} productos) · db={}",
        catalog_source,
        state.catalog.productos.len(),
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
            // Real WhatsApp inbound (Twilio Sandbox): form-encoded in, TwiML out.
            (Method::Post, "/whatsapp") => handle_whatsapp(&state, &model, max_tokens, &mut db, req),
            _ => respond_json(req, 404, &json!({ "error": "no encontrado" }).to_string()),
        }
    }
}

// ── Postgres connection ─────────────────────────────────────────────────────

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
        Ok(c) => {
            eprintln!("db: conectado");
            Some(c)
        }
        Err(e) => {
            eprintln!("db: conexión falló (continuo sin persistencia): {e}");
            None
        }
    }
}

// ── Schema + seed (Postgres is the source of truth) ─────────────────────────

fn migrate_and_seed(c: &mut postgres::Client) -> Result<(), String> {
    c.batch_execute(
        "CREATE TABLE IF NOT EXISTS products (\
            sku TEXT PRIMARY KEY, nombre TEXT NOT NULL, categoria TEXT NOT NULL, \
            precio_mxn INTEGER NOT NULL, descripcion TEXT NOT NULL DEFAULT '', \
            stock INTEGER NOT NULL DEFAULT 0, color TEXT NOT NULL DEFAULT 'estándar', \
            talla TEXT, foto_url TEXT);\
         CREATE TABLE IF NOT EXISTS orders (\
            order_id TEXT PRIMARY KEY, cliente TEXT NOT NULL, estado TEXT NOT NULL, \
            fecha_pedido TEXT NOT NULL, fecha_envio TEXT, entrega_estimada TEXT, \
            fecha_entrega TEXT, guia TEXT, total_mxn INTEGER NOT NULL, ciudad_envio TEXT NOT NULL);\
         CREATE TABLE IF NOT EXISTS order_items (\
            id BIGSERIAL PRIMARY KEY, order_id TEXT NOT NULL, sku TEXT NOT NULL, \
            nombre TEXT NOT NULL, qty INTEGER NOT NULL);\
         CREATE TABLE IF NOT EXISTS chat_log (\
            id BIGSERIAL PRIMARY KEY, ts TIMESTAMPTZ NOT NULL DEFAULT now(), \
            mensaje TEXT NOT NULL, reply TEXT NOT NULL, tools TEXT);",
    )
    .map_err(|e| format!("migrate: {e}"))?;

    // Seed products from the real product API on first boot only.
    let pcount: i64 = c
        .query_one("SELECT count(*) FROM products", &[])
        .map_err(|e| e.to_string())?
        .get(0);
    if pcount == 0 {
        let productos = fetch_catalog_from_api(DEFAULT_CATALOG_API)?;
        for p in &productos {
            let v = &p.variantes[0];
            let foto: Option<&str> = p.foto_url.first().map(String::as_str);
            let talla: Option<&str> = v.talla.as_deref();
            let precio = p.precio_mxn as i32;
            let stock = v.stock as i32;
            c.execute(
                "INSERT INTO products (sku,nombre,categoria,precio_mxn,descripcion,stock,color,talla,foto_url) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT (sku) DO NOTHING",
                &[
                    &p.sku, &p.nombre_es, &p.categoria, &precio, &p.descripcion_es, &stock,
                    &v.color, &talla, &foto,
                ],
            )
            .map_err(|e| format!("insert product: {e}"))?;
        }
        eprintln!("db: sembrados {} productos desde el API", productos.len());
    }

    // Seed a few sample orders on first boot (defined in code, not JSON).
    let ocount: i64 = c
        .query_one("SELECT count(*) FROM orders", &[])
        .map_err(|e| e.to_string())?
        .get(0);
    if ocount == 0 {
        c.batch_execute(
            "INSERT INTO orders VALUES ('10482','Marcus P.','en_camino','2026-06-27','2026-06-28','2026-06-30',NULL,'TRACK-99213',35000,'Guadalajara') ON CONFLICT DO NOTHING;\
             INSERT INTO order_items (order_id,sku,nombre,qty) VALUES ('10482','DJ-MBP','Apple MacBook Pro 14 Inch Space Grey',1);\
             INSERT INTO orders VALUES ('10488','Ana López','entregado','2026-06-16','2026-06-17','2026-06-19','2026-06-20','TRACK-88120',3500,'Ciudad de México') ON CONFLICT DO NOTHING;\
             INSERT INTO order_items (order_id,sku,nombre,qty) VALUES ('10488','DJ-NIKE','Nike Air Jordan 1 Red And Black',1);\
             INSERT INTO orders VALUES ('10455','Valeria Soto','cancelado','2026-06-10',NULL,NULL,NULL,NULL,1890,'Mérida') ON CONFLICT DO NOTHING;\
             INSERT INTO order_items (order_id,sku,nombre,qty) VALUES ('10455','DJ-PRADA','Prada Women Bag',1);",
        )
        .map_err(|e| format!("seed orders: {e}"))?;
        eprintln!("db: sembrados pedidos de ejemplo");
    }
    Ok(())
}

// ── Live catalog load from Postgres (real queries per request) ──────────────

fn load_catalog_db(c: &mut postgres::Client) -> Result<Catalog, String> {
    let prows = c
        .query(
            "SELECT sku,nombre,categoria,precio_mxn,descripcion,stock,color,talla,foto_url \
             FROM products ORDER BY sku",
            &[],
        )
        .map_err(|e| format!("query products: {e}"))?;
    let productos = prows
        .iter()
        .map(|r| {
            let sku: String = r.get("sku");
            let foto: Option<String> = r.get("foto_url");
            Product {
                sku: sku.clone(),
                nombre_es: r.get("nombre"),
                categoria: r.get("categoria"),
                precio_mxn: r.get::<_, i32>("precio_mxn") as u32,
                descripcion_es: r.get("descripcion"),
                foto_url: foto.clone().into_iter().collect(),
                politica_devolucion: None,
                variantes: vec![Variant {
                    sku,
                    color: r.get("color"),
                    talla: r.get("talla"),
                    stock: r.get::<_, i32>("stock") as u32,
                    precio_mxn: None,
                    foto_url: foto.into_iter().collect(),
                }],
            }
        })
        .collect();

    let orows = c
        .query(
            "SELECT order_id,cliente,estado,fecha_pedido,fecha_envio,entrega_estimada,\
             fecha_entrega,guia,total_mxn,ciudad_envio FROM orders ORDER BY order_id",
            &[],
        )
        .map_err(|e| format!("query orders: {e}"))?;
    let irows = c
        .query("SELECT order_id,sku,nombre,qty FROM order_items", &[])
        .map_err(|e| format!("query items: {e}"))?;
    let pedidos = orows
        .iter()
        .map(|r| {
            let oid: String = r.get("order_id");
            let items = irows
                .iter()
                .filter(|ir| ir.get::<_, String>("order_id") == oid)
                .map(|ir| OrderItem {
                    sku: ir.get("sku"),
                    nombre_es: ir.get("nombre"),
                    qty: ir.get::<_, i32>("qty") as u32,
                })
                .collect();
            let estado_s: String = r.get("estado");
            let estado = serde_json::from_value::<EstadoPedido>(Value::String(estado_s))
                .unwrap_or(EstadoPedido::Pendiente);
            Order {
                order_id: oid,
                cliente: r.get("cliente"),
                estado,
                items,
                fecha_pedido: r.get("fecha_pedido"),
                fecha_envio: r.get("fecha_envio"),
                entrega_estimada: r.get("entrega_estimada"),
                fecha_entrega: r.get("fecha_entrega"),
                guia: r.get("guia"),
                total_mxn: r.get::<_, i32>("total_mxn") as u32,
                ciudad_envio: r.get("ciudad_envio"),
            }
        })
        .collect();

    Ok(Catalog { productos, pedidos })
}

// ── External product API (DummyJSON → domain) — used to seed Postgres ───────

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

const DEFAULT_CATALOG_API: &str = "https://dummyjson.com/products?limit=100";
const FX_USD_MXN: f64 = 17.5;

fn fetch_catalog_from_api(url: &str) -> Result<Vec<Product>, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(20))
        .build();
    let resp: DjResp = agent
        .get(url)
        .call()
        .map_err(|e| format!("GET: {e}"))?
        .into_json()
        .map_err(|e| format!("parse: {e}"))?;

    Ok(resp
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
        .collect())
}

/// Last-resort fallback when the DB is unreachable: API, else bundled seed JSON.
fn load_catalog() -> (Catalog, String) {
    let seed = || {
        Catalog::load(Path::new("data/products.json"), Path::new("data/orders.json"))
            .unwrap_or_else(|e| fatal(&format!("catalog seed: {e:#}")))
    };
    match fetch_catalog_from_api(DEFAULT_CATALOG_API) {
        Ok(p) if !p.is_empty() => {
            let pedidos = Catalog::load(
                Path::new("data/products.json"),
                Path::new("data/orders.json"),
            )
            .map(|c| c.pedidos)
            .unwrap_or_default();
            (Catalog { productos: p, pedidos }, "api".to_string())
        }
        _ => (seed(), "seed".to_string()),
    }
}

// ── chat persistence ────────────────────────────────────────────────────────

fn log_turn(db: &mut Option<postgres::Client>, mensaje: &str, reply: &str, tools: &str) {
    if let Some(c) = db.as_mut() {
        if let Err(e) = c.execute(
            "INSERT INTO chat_log (mensaje, reply, tools) VALUES ($1, $2, $3)",
            &[&mensaje, &reply, &tools],
        ) {
            eprintln!("db: insert chat_log falló: {e}");
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

// ── Chat (loads catalog live from Postgres per request) ─────────────────────

fn handle_chat(
    fallback: &AppState,
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

    // Real per-request data call to the backend: load catalog + orders from Postgres.
    let live = db
        .as_mut()
        .and_then(|c| load_catalog_db(c).ok())
        .filter(|cat| !cat.productos.is_empty())
        .map(|cat| AppState::new(fallback.config.clone(), cat));
    let state: &AppState = live.as_ref().unwrap_or(fallback);

    let mut agent = Agent::new(state, client, model.to_string(), max_tokens);
    match agent.send(&prompt_msg) {
        Ok(turn) => {
            let tools_used: Vec<&str> = turn.trace.iter().map(|t| t.name.as_str()).collect();
            let trace: Vec<Value> = turn
                .trace
                .iter()
                .map(|t| json!({ "tool": t.name, "input": t.input, "output": t.output }))
                .collect();
            let resp_body = json!({ "reply": turn.reply, "trace": trace }).to_string();
            log_turn(db, &mensaje, &turn.reply, &tools_used.join(","));
            respond_json(req, 200, &resp_body);
        }
        Err(e) => respond_json(req, 502, &json!({ "error": format!("{e:#}") }).to_string()),
    }
}

// ── WhatsApp (Twilio Sandbox: inbound form-encoded → TwiML reply) ───────────

fn handle_whatsapp(
    fallback: &AppState,
    model: &str,
    max_tokens: u32,
    db: &mut Option<postgres::Client>,
    mut req: Request,
) {
    let mut body = String::new();
    let _ = req.as_reader().read_to_string(&mut body);
    let mensaje = form_field(&body, "Body").unwrap_or_default();

    let reply = if mensaje.trim().is_empty() {
        "¡Hola! 👋 Soy el asistente de Tienda Vortex. Pregúntame por productos, envíos o tu pedido."
            .to_string()
    } else {
        match Client::from_env() {
            Ok(client) => {
                let live = db
                    .as_mut()
                    .and_then(|c| load_catalog_db(c).ok())
                    .filter(|c| !c.productos.is_empty())
                    .map(|c| AppState::new(fallback.config.clone(), c));
                let state: &AppState = live.as_ref().unwrap_or(fallback);
                let mut agent = Agent::new(state, client, model.to_string(), max_tokens);
                match agent.send(&mensaje) {
                    Ok(turn) => {
                        let tools = turn
                            .trace
                            .iter()
                            .map(|t| t.name.as_str())
                            .collect::<Vec<_>>()
                            .join(",");
                        log_turn(db, &mensaje, &turn.reply, &tools);
                        turn.reply
                    }
                    Err(e) => format!("Lo siento, hubo un error: {e}"),
                }
            }
            Err(_) => "El servidor no tiene configurada la API key todavía.".to_string(),
        }
    };

    let twiml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Response><Message>{}</Message></Response>",
        xml_escape(&truncate(&reply, 1500))
    );
    let _ = req.respond(
        Response::from_string(twiml).with_header(header("Content-Type", "text/xml; charset=utf-8")),
    );
}

fn form_field(body: &str, name: &str) -> Option<String> {
    for pair in body.split('&') {
        let mut it = pair.splitn(2, '=');
        if it.next() == Some(name) {
            return Some(percent_decode(it.next().unwrap_or("")));
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                match (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                    (Some(h), Some(l)) => {
                        out.push(h * 16 + l);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
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
  es:{lang:'es',sub:'Asistente de la tienda. Pregunta por productos, envíos o tu pedido.',ph:'tienen laptops? a qué precio?',send:'Enviar',wait:'…',none:'(sin respuesta)'},
  en:{lang:'en',sub:'Store assistant. Ask about products, shipping or your order.',ph:'do you have any laptops? what price?',send:'Send',wait:'…',none:'(no reply)'}
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

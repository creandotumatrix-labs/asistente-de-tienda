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

use std::collections::HashSet;
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

    let mut wa_seen: HashSet<String> = HashSet::new();
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
            // On-demand live catalog refresh from the product API into Postgres.
            (Method::Get, "/admin/resync") => {
                let out = match db.as_mut() {
                    Some(c) => match sync_products(c) {
                        Ok(n) => json!({ "synced": n, "source": std::env::var("CATALOG_API_URL")
                            .unwrap_or_else(|_| DEFAULT_CATALOG_API.to_string()) }),
                        Err(e) => json!({ "error": e }),
                    },
                    None => json!({ "error": "sin base de datos" }),
                };
                respond_json(req, 200, &out.to_string());
            }
            (Method::Get, "/") => {
                let _ = req.respond(
                    Response::from_string(index_html(&state.config.tienda.nombre))
                        .with_header(header("Content-Type", "text/html; charset=utf-8")),
                );
            }
            (Method::Get, "/history") => handle_history(&mut db, req),
            (Method::Post, "/chat") => handle_chat(&state, &model, max_tokens, &mut db, req),
            // Real WhatsApp (Meta Cloud API): GET verifies the webhook, POST handles inbound.
            (Method::Get, "/whatsapp") => wa_verify(req),
            (Method::Post, "/whatsapp") => {
                handle_whatsapp(&state, &model, max_tokens, &mut db, &mut wa_seen, req)
            }
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

    // Sync the catalog from the LIVE product API (upsert) — refreshes price/stock.
    match sync_products(c) {
        Ok(n) => eprintln!("db: catálogo sincronizado en vivo ({n} productos)"),
        Err(e) => eprintln!("db: sync catálogo falló ({e}); uso lo que ya hay en la DB"),
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

/// Pull the catalog from the live product API (CATALOG_API_URL) and upsert into
/// Postgres — refreshes price/stock so the DB mirrors the live source.
fn sync_products(c: &mut postgres::Client) -> Result<usize, String> {
    let url = std::env::var("CATALOG_API_URL").unwrap_or_else(|_| DEFAULT_CATALOG_API.to_string());
    let productos = fetch_catalog(&url)?;
    for p in &productos {
        let v = &p.variantes[0];
        let foto: Option<&str> = p.foto_url.first().map(String::as_str);
        let talla: Option<&str> = v.talla.as_deref();
        let precio = p.precio_mxn as i32;
        let stock = v.stock as i32;
        c.execute(
            "INSERT INTO products (sku,nombre,categoria,precio_mxn,descripcion,stock,color,talla,foto_url) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) \
             ON CONFLICT (sku) DO UPDATE SET \
                nombre=EXCLUDED.nombre, categoria=EXCLUDED.categoria, precio_mxn=EXCLUDED.precio_mxn, \
                descripcion=EXCLUDED.descripcion, stock=EXCLUDED.stock, color=EXCLUDED.color, \
                talla=EXCLUDED.talla, foto_url=EXCLUDED.foto_url",
            &[
                &p.sku, &p.nombre_es, &p.categoria, &precio, &p.descripcion_es, &stock,
                &v.color, &talla, &foto,
            ],
        )
        .map_err(|e| format!("upsert product: {e}"))?;
    }
    Ok(productos.len())
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

/// Route the configured source to the right fetcher.
fn fetch_catalog(source: &str) -> Result<Vec<Product>, String> {
    if source.eq_ignore_ascii_case("itunes") {
        fetch_catalog_itunes()
    } else {
        fetch_catalog_from_api(source)
    }
}

// ── Apple iTunes Search API (free, no key, real products + real MXN prices) ──

#[derive(Deserialize)]
struct ItunesResp {
    #[serde(default)]
    results: Vec<ItunesItem>,
}

#[derive(Deserialize)]
struct ItunesItem {
    #[serde(rename = "trackId", default)]
    track_id: Option<i64>,
    #[serde(rename = "collectionId", default)]
    collection_id: Option<i64>,
    #[serde(rename = "trackName", default)]
    track_name: Option<String>,
    #[serde(rename = "collectionName", default)]
    collection_name: Option<String>,
    #[serde(rename = "artistName", default)]
    artist_name: Option<String>,
    #[serde(rename = "primaryGenreName", default)]
    primary_genre_name: Option<String>,
    #[serde(rename = "trackPrice", default)]
    track_price: Option<f64>,
    #[serde(rename = "collectionPrice", default)]
    collection_price: Option<f64>,
    #[serde(rename = "artworkUrl100", default)]
    artwork_url100: Option<String>,
    #[serde(rename = "longDescription", default)]
    long_description: Option<String>,
    #[serde(rename = "description", default)]
    description: Option<String>,
    #[serde(rename = "wrapperType", default)]
    wrapper_type: Option<String>,
}

/// Build a real catalog from the Mexican Apple Store (prices already in MXN).
fn fetch_catalog_itunes() -> Result<Vec<Product>, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(20))
        .build();
    let queries: [(&str, &str, &str); 8] = [
        ("top hits", "music", "Pop"),
        ("rock", "music", "Rock"),
        ("hip hop", "music", "Hip-Hop/Rap"),
        ("regional mexicano", "music", "Regional Mexicano"),
        ("pop latino", "music", "Pop Latino"),
        ("electronica", "music", "Electrónica"),
        ("jazz", "music", "Jazz"),
        ("classical", "music", "Clásica"),
    ];
    let mut productos: Vec<Product> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (term, media, cat) in queries {
        let q = term.split_whitespace().collect::<Vec<_>>().join("+");
        let url =
            format!("https://itunes.apple.com/search?term={q}&media={media}&country=MX&limit=25");
        let resp: ItunesResp = match agent
            .get(&url)
            .call()
            .map_err(|e| e.to_string())
            .and_then(|r| r.into_json().map_err(|e| e.to_string()))
        {
            Ok(r) => r,
            Err(_) => continue,
        };
        for it in resp.results {
            let id = match it.track_id.or(it.collection_id) {
                Some(x) if x != 0 => x,
                _ => continue,
            };
            let sku = format!("AP-{id}");
            if !seen.insert(sku.clone()) {
                continue;
            }
            let nombre = it.track_name.or(it.collection_name).unwrap_or_default();
            if nombre.trim().is_empty() {
                continue;
            }
            let precio = it.track_price.or(it.collection_price).unwrap_or(0.0);
            if precio <= 0.0 {
                continue;
            }
            let artista = it.artist_name.unwrap_or_default();
            let nombre_full = if artista.is_empty() {
                nombre
            } else {
                format!("{nombre} — {artista}")
            };
            let genero = it.primary_genre_name.unwrap_or_default();
            let desc = it
                .long_description
                .or(it.description)
                .filter(|d| !d.trim().is_empty())
                .unwrap_or_else(|| {
                    if genero.is_empty() {
                        "Producto digital de Apple Store".to_string()
                    } else {
                        genero.clone()
                    }
                });
            let foto = it.artwork_url100.map(|u| u.replace("100x100", "600x600"));
            productos.push(Product {
                sku: sku.clone(),
                nombre_es: nombre_full,
                categoria: cat.to_string(),
                precio_mxn: precio.round().max(1.0) as u32,
                descripcion_es: desc,
                foto_url: foto.into_iter().collect(),
                politica_devolucion: None,
                variantes: vec![Variant {
                    sku,
                    color: "digital".to_string(),
                    talla: None,
                    stock: 999,
                    precio_mxn: None,
                    foto_url: vec![],
                }],
            });
        }
    }
    if productos.is_empty() {
        return Err("iTunes: 0 productos".to_string());
    }
    Ok(productos)
}

/// Pull meaningful search keywords out of a user message (drop stopwords).
fn keywords(msg: &str) -> String {
    const STOP: &[&str] = &[
        "the", "a", "an", "some", "any", "you", "do", "does", "did", "have", "has", "had", "got",
        "is", "are", "was", "there", "this", "that", "these", "those", "i", "we", "me", "my", "our",
        "for", "of", "to", "in", "on", "at", "with", "and", "or", "please", "hi", "hello", "hey",
        "it", "its", "need", "want", "looking", "buy", "get", "show", "find", "search", "sell",
        "tienen", "tiene", "tienes", "tenes", "hay", "tengo", "quiero", "busco", "buscar", "vende",
        "venden", "muestra", "dame", "por", "favor", "de", "del", "la", "el", "los", "las", "un",
        "una", "unos", "unas", "que", "cual", "cuales", "con", "sin", "y", "o", "hola", "algun",
        "alguna", "alguno", "algunas", "algo", "producto", "productos", "tienda", "precio", "cuanto",
        "cuesta", "me", "mi", "tu", "su",
    ];
    let mut out: Vec<String> = Vec::new();
    for raw in msg.split(|c: char| !c.is_alphanumeric()) {
        let w = raw.to_lowercase();
        if w.chars().count() < 2 || STOP.contains(&w.as_str()) {
            continue;
        }
        if !out.contains(&w) {
            out.push(w);
        }
        if out.len() >= 6 {
            break;
        }
    }
    out.join(" ")
}

/// Live Apple/iTunes MUSIC search for the user's terms — returns real songs AND
/// albums (with cover art + native MXN prices) straight from Apple. This is the
/// store's only source of product truth; nothing about a title is invented.
fn fetch_itunes_query(term: &str) -> Vec<Product> {
    let mut out: Vec<Product> = Vec::new();
    if term.trim().is_empty() {
        return out;
    }
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(4))
        .timeout_read(Duration::from_secs(14))
        .build();
    let q = term.split_whitespace().collect::<Vec<_>>().join("+");
    // Ask Apple explicitly for MUSIC: albums first, then songs (MX store => MXN).
    let urls = [
        format!("https://itunes.apple.com/search?term={q}&country=MX&media=music&entity=album&limit=12"),
        format!("https://itunes.apple.com/search?term={q}&country=MX&media=music&entity=song&limit=16"),
    ];
    let mut seen: HashSet<String> = HashSet::new();
    for url in urls {
        let resp: ItunesResp = match agent
            .get(&url)
            .call()
            .map_err(|e| e.to_string())
            .and_then(|r| r.into_json().map_err(|e| e.to_string()))
        {
            Ok(r) => r,
            Err(_) => continue,
        };
        for it in resp.results {
            let es_album = it.wrapper_type.as_deref() == Some("collection");
            let id = match (if es_album { it.collection_id } else { it.track_id }).or(it.collection_id)
            {
                Some(x) if x != 0 => x,
                _ => continue,
            };
            let sku = format!("AP-{id}");
            if !seen.insert(sku.clone()) {
                continue;
            }
            let nombre = if es_album {
                it.collection_name.clone()
            } else {
                it.track_name.clone()
            };
            let nombre = match nombre {
                Some(n) if !n.trim().is_empty() => n,
                _ => continue,
            };
            // Real Apple price: album => collectionPrice, song => trackPrice.
            // Album-only songs (trackPrice <= 0) are skipped; the album carries them.
            let precio = match if es_album { it.collection_price } else { it.track_price } {
                Some(p) if p > 0.0 => p,
                _ => continue,
            };
            let artista = it.artist_name.clone().unwrap_or_default();
            let genero = it.primary_genre_name.clone().unwrap_or_default();
            let tipo = if es_album { "Álbum" } else { "Canción" };
            let nombre_full = if artista.is_empty() {
                nombre
            } else {
                format!("{nombre} — {artista}")
            };
            let desc = if genero.is_empty() {
                format!("{tipo} · Apple Music")
            } else {
                format!("{tipo} · {genero} · Apple Music")
            };
            let categoria = if genero.is_empty() {
                tipo.to_string()
            } else {
                format!("{tipo} · {genero}")
            };
            let foto = it
                .artwork_url100
                .clone()
                .map(|u| u.replace("100x100", "600x600"));
            out.push(Product {
                sku: sku.clone(),
                nombre_es: nombre_full,
                categoria,
                precio_mxn: precio.round().max(1.0) as u32,
                descripcion_es: desc,
                foto_url: foto.into_iter().collect(),
                politica_devolucion: None,
                variantes: vec![Variant {
                    sku,
                    color: "digital".to_string(),
                    talla: None,
                    stock: 999,
                    precio_mxn: None,
                    foto_url: vec![],
                }],
            });
        }
    }
    out
}

/// Last-resort fallback when the DB is unreachable: API, else bundled seed JSON.
fn load_catalog() -> (Catalog, String) {
    let seed = || {
        Catalog::load(Path::new("data/products.json"), Path::new("data/orders.json"))
            .unwrap_or_else(|e| fatal(&format!("catalog seed: {e:#}")))
    };
    let src = std::env::var("CATALOG_API_URL").unwrap_or_else(|_| DEFAULT_CATALOG_API.to_string());
    match fetch_catalog(&src) {
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

    // Real per-request data call: load catalog + orders from Postgres, then
    // augment with a LIVE iTunes search on the user's terms so any real Apple
    // product is findable — not just the pre-synced subset.
    let kw = keywords(&mensaje);
    let live = db
        .as_mut()
        .and_then(|c| load_catalog_db(c).ok())
        .filter(|cat| !cat.productos.is_empty())
        .map(|mut cat| {
            if kw.len() >= 3 {
                let existentes: HashSet<String> =
                    cat.productos.iter().map(|p| p.sku.clone()).collect();
                for p in fetch_itunes_query(&kw) {
                    if !existentes.contains(&p.sku) {
                        cat.productos.push(p);
                    }
                }
            }
            AppState::new(fallback.config.clone(), cat)
        });
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

// ── WhatsApp (Meta Cloud API) ───────────────────────────────────────────────

/// GET webhook verification — echo Meta's hub.challenge when the token matches.
fn wa_verify(req: Request) {
    let url = req.url().to_string();
    let mode = query_param(&url, "hub.mode");
    let token = query_param(&url, "hub.verify_token");
    let challenge = query_param(&url, "hub.challenge").unwrap_or_default();
    let expected = std::env::var("WHATSAPP_VERIFY_TOKEN").ok();
    if mode.as_deref() == Some("subscribe") && token.is_some() && token == expected {
        let _ = req.respond(Response::from_string(challenge));
    } else {
        respond_json(req, 403, &json!({ "error": "verify_token inválido" }).to_string());
    }
}

/// POST inbound message → run the agent → send the reply via the Graph API.
/// De-dups Meta retries by message id, and always returns 200.
fn handle_whatsapp(
    fallback: &AppState,
    model: &str,
    max_tokens: u32,
    db: &mut Option<postgres::Client>,
    seen: &mut HashSet<String>,
    mut req: Request,
) {
    let mut body = String::new();
    let _ = req.as_reader().read_to_string(&mut body);
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);

    if let Some(m) = v.pointer("/entry/0/changes/0/value/messages/0") {
        let id = m.get("id").and_then(Value::as_str).unwrap_or("").to_string();
        let from = m.get("from").and_then(Value::as_str).unwrap_or("").to_string();
        let text = m
            .get("text")
            .and_then(|t| t.get("body"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        if !id.is_empty() && !seen.insert(id) {
            return respond_json(req, 200, "{\"status\":\"dup\"}");
        }

        if !from.is_empty() && !text.trim().is_empty() {
            let reply = match Client::from_env() {
                Ok(client) => {
                    let live = db
                        .as_mut()
                        .and_then(|c| load_catalog_db(c).ok())
                        .filter(|c| !c.productos.is_empty())
                        .map(|c| AppState::new(fallback.config.clone(), c));
                    let state: &AppState = live.as_ref().unwrap_or(fallback);
                    let mut agent = Agent::new(state, client, model.to_string(), max_tokens);
                    match agent.send(&text) {
                        Ok(turn) => {
                            let tools = turn
                                .trace
                                .iter()
                                .map(|t| t.name.as_str())
                                .collect::<Vec<_>>()
                                .join(",");
                            log_turn(db, &text, &turn.reply, &tools);
                            turn.reply
                        }
                        Err(e) => format!("Lo siento, hubo un error: {e}"),
                    }
                }
                Err(_) => "El servidor no tiene la API key configurada.".to_string(),
            };
            wa_send(&from, &truncate(&reply, 3000));
        }
    }
    respond_json(req, 200, "{\"status\":\"ok\"}");
}

/// Send a WhatsApp text via the Meta Graph API (token + phone id from env).
fn wa_send(to: &str, text: &str) {
    let (token, phone_id) = match (
        std::env::var("WHATSAPP_ACCESS_TOKEN"),
        std::env::var("WHATSAPP_PHONE_NUMBER_ID"),
    ) {
        (Ok(t), Ok(p)) if !t.is_empty() && !p.is_empty() => (t, p),
        _ => {
            eprintln!("wa_send: faltan WHATSAPP_ACCESS_TOKEN / WHATSAPP_PHONE_NUMBER_ID");
            return;
        }
    };
    let url = format!("https://graph.facebook.com/v20.0/{phone_id}/messages");
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(15))
        .build();
    let payload = json!({
        "messaging_product": "whatsapp",
        "to": to,
        "type": "text",
        "text": { "body": text },
    });
    if let Err(e) = agent
        .post(&url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .send_json(payload)
    {
        eprintln!("wa_send falló: {e}");
    }
}

fn query_param(url: &str, key: &str) -> Option<String> {
    let q = url.split('?').nth(1)?;
    for pair in q.split('&') {
        let mut it = pair.splitn(2, '=');
        if it.next() == Some(key) {
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
<meta name="viewport" content="width=device-width, initial-scale=1, maximum-scale=1">
<title>__NOMBRE__ · Música con IA</title>
<style>
:root{--am1:#fb2c6b;--am2:#9b5cff;--am3:#ff6a8b;--bg:#08070d;--ink:#f4f0f7;--muted:#a99fb6;--line:rgba(255,255,255,.09);--glass:rgba(255,255,255,.05)}
*{box-sizing:border-box}
html,body{height:100%}
body{margin:0;font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Inter,Roboto,sans-serif;background:var(--bg);color:var(--ink);-webkit-font-smoothing:antialiased;display:flex;flex-direction:column;overflow:hidden}
body::before{content:"";position:fixed;inset:-25%;z-index:-1;background:radial-gradient(45% 35% at 12% 6%,rgba(251,44,107,.24),transparent 60%),radial-gradient(45% 40% at 90% 3%,rgba(155,92,255,.22),transparent 60%),radial-gradient(60% 45% at 60% 100%,rgba(155,92,255,.10),transparent 60%);filter:blur(24px)}
header{display:flex;align-items:center;justify-content:space-between;gap:12px;padding:13px 18px;border-bottom:1px solid var(--line);backdrop-filter:blur(12px);background:rgba(8,7,13,.62)}
.brand{display:flex;align-items:center;gap:11px;min-width:0}
.logo{width:40px;height:40px;border-radius:12px;background:linear-gradient(140deg,var(--am1),var(--am2));display:grid;place-items:center;font-size:20px;flex:none;box-shadow:0 8px 22px -8px rgba(251,44,107,.65)}
.brand h1{font-size:1.02rem;margin:0;font-weight:800;letter-spacing:-.2px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.brand .sub{font-size:.72rem;color:var(--muted);margin-top:1px}
.seg{display:inline-flex;border:1px solid var(--line);border-radius:999px;overflow:hidden;flex:none}
.seg button{background:transparent;color:var(--muted);border:0;padding:6px 13px;font-size:.78rem;font-weight:700;cursor:pointer}
.seg button.on{background:linear-gradient(120deg,var(--am1),var(--am2));color:#fff}
main{flex:1;overflow-y:auto;padding:18px 16px 10px}
#log{max-width:680px;margin:0 auto}
.empty{max-width:560px;margin:9vh auto 0;text-align:center;padding:0 8px}
.empty .big{font-size:1.5rem;font-weight:800;letter-spacing:-.5px;line-height:1.2}
.empty .big span{background:linear-gradient(100deg,var(--am3),var(--am1) 45%,var(--am2));-webkit-background-clip:text;background-clip:text;color:transparent}
.empty p{color:var(--muted);margin:.55rem 0 1.3rem}
.chips{display:flex;flex-wrap:wrap;gap:9px;justify-content:center}
.chip{border:1px solid var(--line);background:var(--glass);color:var(--ink);border-radius:999px;padding:9px 15px;font-size:.86rem;cursor:pointer;transition:transform .12s,border-color .12s}
.chip:hover{transform:translateY(-2px);border-color:var(--am1)}
.row{display:flex;margin:11px 0}
.row.me{justify-content:flex-end}
.b{max-width:88%;padding:11px 15px;border-radius:18px;font-size:.95rem;line-height:1.5;overflow-wrap:anywhere}
.me .b{background:linear-gradient(120deg,var(--am1),var(--am2));color:#fff;border-bottom-right-radius:6px}
.ai .b{background:var(--glass);border:1px solid var(--line);color:#ece7f2;border-bottom-left-radius:6px}
.ai .b img.cov{display:block;width:136px;height:136px;object-fit:cover;border-radius:12px;margin:10px 0 4px;border:1px solid var(--line);background:#15121c}
.ai .b strong{color:#fff}
.ai .b a{color:var(--am3)}
.typing span{display:inline-block;width:7px;height:7px;margin:0 2px;border-radius:50%;background:var(--muted);animation:bounce 1.2s infinite}
.typing span:nth-child(2){animation-delay:.15s}.typing span:nth-child(3){animation-delay:.3s}
@keyframes bounce{0%,80%,100%{opacity:.3;transform:translateY(0)}40%{opacity:1;transform:translateY(-4px)}}
footer{padding:12px 16px calc(12px + env(safe-area-inset-bottom));border-top:1px solid var(--line);backdrop-filter:blur(12px);background:rgba(8,7,13,.62)}
form{display:flex;gap:10px;max-width:680px;margin:0 auto;align-items:center}
input{flex:1;font-size:1rem;padding:13px 17px;border-radius:999px;border:1px solid var(--line);background:rgba(255,255,255,.06);color:var(--ink);outline:none}
input:focus{border-color:var(--am1)}
input::placeholder{color:var(--muted)}
button.send{width:47px;height:47px;flex:none;border-radius:50%;border:0;background:linear-gradient(120deg,var(--am1),var(--am2));color:#fff;cursor:pointer;display:grid;place-items:center;box-shadow:0 8px 22px -8px rgba(251,44,107,.65)}
button.send svg{width:20px;height:20px}
button.send:disabled{opacity:.5}
</style></head><body>
<header>
  <div class="brand"><span class="logo">♪</span><div style="min-width:0"><h1>__NOMBRE__</h1><div class="sub" id="sub"></div></div></div>
  <div class="seg"><button id="bes" class="on">ES</button><button id="ben">EN</button></div>
</header>
<main><div id="log"><div class="empty" id="empty"></div></div></main>
<footer><form id="f"><input id="m" autocomplete="off" autofocus><button class="send" id="send" type="submit" aria-label="send"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4"><path d="M5 12h14M13 6l6 6-6 6"/></svg></button></form></footer>
<script>
const I18N={
  es:{lang:'es',sub:'Música de Apple · con IA',ph:'busca un artista, canción o álbum…',none:'(sin respuesta)',hi:'Tu música de Apple, con IA.',lead:'Pídeme cualquier artista, canción o álbum. Lo busco en vivo en Apple con su portada y precio.',chips:['The Beatles','Bad Bunny','Café Tacvba','Coldplay']},
  en:{lang:'en',sub:'Apple Music · AI-powered',ph:'search an artist, song or album…',none:'(no reply)',hi:'Your Apple Music, powered by AI.',lead:'Ask me for any artist, song or album. I search Apple live and show real cover art and price.',chips:['The Beatles','Taylor Swift','Daft Punk','Coldplay']}
};
const $=id=>document.getElementById(id);
const main=document.querySelector('main');
function down(){main.scrollTop=main.scrollHeight;}
let lang=localStorage.getItem('lang')||'es';
function drawEmpty(){const t=I18N[lang];const e=$('empty');if(!e)return;const hi=t.hi.replace(/(IA|AI)\./,'<span>$1</span>.');e.innerHTML='<div class="big">'+hi+'</div><p>'+t.lead+'</p><div class="chips">'+t.chips.map(c=>'<button class="chip">'+c+'</button>').join('')+'</div>';e.querySelectorAll('.chip').forEach(c=>c.onclick=()=>{$('m').value=c.textContent;send();});}
function apply(){const t=I18N[lang];$('html').lang=t.lang;$('sub').textContent=t.sub;$('m').placeholder=t.ph;$('bes').classList.toggle('on',lang==='es');$('ben').classList.toggle('on',lang==='en');drawEmpty();}
$('bes').onclick=()=>{lang='es';localStorage.setItem('lang',lang);apply();$('m').focus();};
$('ben').onclick=()=>{lang='en';localStorage.setItem('lang',lang);apply();$('m').focus();};
const log=$('log');
function esc(s){return s.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');}
function render(t){let s=esc(t);
  s=s.replace(/!\[[^\]]*\]\((https?:\/\/[^)\s]+)\)/g,'<img class="cov" src="$1" loading="lazy">');
  s=s.replace(/(^|[\s>])(https?:\/\/[^\s)<\]"]+\.(?:jpg|jpeg|png))/gi,'$1<img class="cov" src="$2" loading="lazy">');
  s=s.replace(/\[([^\]]+)\]\((https?:\/\/[^)\s]+)\)/g,'<a href="$2" target="_blank" rel="noopener">$1</a>');
  s=s.replace(/\*\*([^*]+)\*\*/g,'<strong>$1</strong>');
  s=s.replace(/^[ \t]*\|?[ \t:|-]{3,}\|?[ \t]*$/gm,'');
  s=s.replace(/[ \t]*\|[ \t]*/g,'  ');
  s=s.replace(/\n{2,}/g,'\n').replace(/\n/g,'<br>');
  return s;}
function bubble(html,who,raw){const r=document.createElement('div');r.className='row '+who;const b=document.createElement('div');b.className='b';if(raw){b.innerHTML=html;}else{b.textContent=html;}r.appendChild(b);log.appendChild(r);down();return b;}
let busy=false;
async function send(){const t=$('m').value.trim();if(!t||busy)return;const e=$('empty');if(e)e.remove();busy=true;$('send').disabled=true;bubble(t,'me',false);$('m').value='';const b=bubble('<div class="typing"><span></span><span></span><span></span></div>','ai',true);
  try{const r=await fetch('/chat',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({mensaje:t,lang:lang})});const j=await r.json();b.innerHTML=render(j.reply||j.error||I18N[lang].none);}
  catch(err){b.textContent='⚠️ '+err;}
  busy=false;$('send').disabled=false;down();$('m').focus();}
$('f').onsubmit=e=>{e.preventDefault();send();};
apply();$('m').focus();
</script></body></html>"##;
    HTML.replace("__NOMBRE__", nombre)
}

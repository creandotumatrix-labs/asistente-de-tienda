//! HTTP service wrapper around the grounded agent, for container/Railway deploys.
//! Synchronous (tiny_http) to match the blocking agent loop — no async runtime.
//!
//! Endpoints:
//!   GET  /health  -> 200 liveness + config status (stays healthy even with no API key)
//!   GET  /        -> info page + a minimal WhatsApp-style chat box
//!   POST /chat    -> { "mensaje": "..." } -> drives the agent, returns { reply, trace }
//!
//! Env: PORT (Railway-provided), ANTHROPIC_API_KEY (required for /chat),
//!      ANTHROPIC_MODEL (optional), DATABASE_URL (surfaced in /health; Postgres reserved
//!      for order/RMA persistence in a later increment).

use std::io::Read;

use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use asistente_de_tienda::{
    agent::Agent,
    anthropic::Client,
    config::StoreConfig,
    model::Catalog,
    tools::{self, AppState},
};

fn main() {
    let _ = dotenvy::dotenv();

    let config = StoreConfig::load(std::path::Path::new("config/store.toml"))
        .unwrap_or_else(|e| fatal(&format!("config: {e:#}")));
    let catalog = Catalog::load(
        std::path::Path::new("data/products.json"),
        std::path::Path::new("data/orders.json"),
    )
    .unwrap_or_else(|e| fatal(&format!("catalog: {e:#}")));

    let model = std::env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| config.agente.modelo.clone());
    let max_tokens = config.agente.max_tokens;
    let state = AppState::new(config, catalog);
    let n_tools = tools::enabled_tools(&state).len();

    let port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(8080);
    let addr = format!("0.0.0.0:{port}");
    let server = Server::http(&addr).unwrap_or_else(|e| fatal(&format!("bind {addr}: {e}")));
    eprintln!("asistente-de-tienda escuchando en {addr} · modelo {model} · {n_tools} herramientas");

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
                    "anthropic_key": std::env::var("ANTHROPIC_API_KEY").is_ok(),
                    "database_url": std::env::var("DATABASE_URL").is_ok(),
                });
                respond_json(req, 200, &body.to_string());
            }
            (Method::Get, "/") => {
                let _ = req.respond(
                    Response::from_string(index_html(&state.config.tienda.nombre))
                        .with_header(header("Content-Type", "text/html; charset=utf-8")),
                );
            }
            (Method::Post, "/chat") => handle_chat(&state, &model, max_tokens, req),
            _ => respond_json(req, 404, &json!({ "error": "no encontrado" }).to_string()),
        }
    }
}

fn handle_chat(state: &AppState, model: &str, max_tokens: u32, mut req: Request) {
    let mut body = String::new();
    if req.as_reader().read_to_string(&mut body).is_err() {
        return respond_json(req, 400, &json!({ "error": "cuerpo ilegible" }).to_string());
    }
    let mensaje = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| v.get("mensaje").and_then(Value::as_str).map(str::to_string));
    let mensaje = match mensaje {
        Some(m) if !m.trim().is_empty() => m,
        _ => return respond_json(req, 400, &json!({ "error": "falta 'mensaje'" }).to_string()),
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
    match agent.send(&mensaje) {
        Ok(turn) => {
            let trace: Vec<Value> = turn
                .trace
                .iter()
                .map(|t| json!({ "tool": t.name, "input": t.input, "output": t.output }))
                .collect();
            respond_json(
                req,
                200,
                &json!({ "reply": turn.reply, "trace": trace }).to_string(),
            );
        }
        Err(e) => respond_json(req, 502, &json!({ "error": format!("{e:#}") }).to_string()),
    }
}

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
    const HTML: &str = r##"<!doctype html><html lang="es"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>__NOMBRE__ · Asistente</title>
<style>
body{font-family:system-ui,-apple-system,sans-serif;max-width:640px;margin:32px auto;padding:0 16px;background:#0b141a;color:#e9edef}
h1{font-size:1.2rem} code{background:#202c33;padding:2px 6px;border-radius:6px}
#log .b{background:#202c33;border-radius:10px;padding:9px 13px;margin:7px 0;white-space:pre-wrap;display:inline-block;max-width:85%}
#log .me{text-align:right} #log .me .b{background:#005c4b}
form{display:flex;gap:8px;margin-top:12px} input{flex:1;font-size:1rem;padding:11px;border-radius:8px;border:0}
button{font-size:1rem;padding:11px 16px;border-radius:8px;border:0;background:#00a884;color:#fff;cursor:pointer}
</style></head><body>
<h1>🛍 __NOMBRE__ — Asistente (es-MX)</h1>
<p>Demo. API: <code>GET /health</code> · <code>POST /chat</code> con <code>{"mensaje":"..."}</code></p>
<div id="log"></div>
<form id="f"><input id="m" placeholder="tienen la mochila Vortex en negro?" autocomplete="off" autofocus><button>Enviar</button></form>
<script>
const log=document.getElementById('log'),f=document.getElementById('f'),m=document.getElementById('m');
function add(t,who){const d=document.createElement('div');d.className=who;const b=document.createElement('div');b.className='b';b.textContent=t;d.appendChild(b);log.appendChild(d);window.scrollTo(0,9e9);return b;}
f.onsubmit=async e=>{e.preventDefault();const t=m.value.trim();if(!t)return;add(t,'me');m.value='';const b=add('…','bot');
try{const r=await fetch('/chat',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({mensaje:t})});const j=await r.json();b.textContent=j.reply||j.error||'(sin respuesta)';}
catch(err){b.textContent='error: '+err;}};
</script></body></html>"##;
    HTML.replace("__NOMBRE__", nombre)
}

//! `tienda` — CLI WhatsApp simulator for the Asistente de Tienda agent.
//! Reads catalog/orders/config, drives the Anthropic tool-use loop, and renders
//! a chat-style conversation. `--debug` prints the live tool-call trace.

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

use asistente_de_tienda::agent::{Agent, TraceEntry, Turn};
use asistente_de_tienda::{anthropic::Client, config::StoreConfig, model::Catalog, tools::AppState};

#[derive(Parser, Debug)]
#[command(
    name = "tienda",
    version,
    about = "Asistente de Tienda — agente de soporte + ventas para WhatsApp (es-MX)"
)]
struct Args {
    /// Ruta al config white-label (TOML).
    #[arg(long, default_value = "config/store.toml")]
    config: PathBuf,
    /// Ruta al catálogo (JSON).
    #[arg(long, default_value = "data/products.json")]
    catalog: PathBuf,
    /// Ruta a los pedidos (JSON).
    #[arg(long, default_value = "data/orders.json")]
    orders: PathBuf,
    /// Modelo a usar (override). Precedencia: --model > ANTHROPIC_MODEL > config.
    #[arg(long)]
    model: Option<String>,
    /// Envía un solo mensaje y termina (modo no interactivo).
    #[arg(long, value_name = "MENSAJE")]
    once: Option<String>,
    /// Muestra la traza de llamadas a herramientas.
    #[arg(long)]
    debug: bool,
    /// Omite el banner inicial.
    #[arg(long)]
    no_banner: bool,
}

struct Style {
    on: bool,
}
impl Style {
    fn new() -> Self {
        Style {
            on: io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
        }
    }
    fn paint(&self, code: &str, s: &str) -> String {
        if self.on {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
    fn dim(&self, s: &str) -> String {
        self.paint("2", s)
    }
    fn agent(&self, s: &str) -> String {
        self.paint("32", s)
    }
    fn bold(&self, s: &str) -> String {
        self.paint("1", s)
    }
}

fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let args = Args::parse();
    let style = Style::new();

    let config = StoreConfig::load(&args.config)?;
    let catalog = Catalog::load(&args.catalog, &args.orders)?;

    let model = args
        .model
        .clone()
        .or_else(|| std::env::var("ANTHROPIC_MODEL").ok())
        .unwrap_or_else(|| config.agente.modelo.clone());
    let max_tokens = config.agente.max_tokens;
    let agente_nombre = config.persona.nombre_agente.clone();
    let saludo = config.branding.saludo.clone();
    let despedida = config.branding.despedida.clone();

    let state = AppState::new(config, catalog);
    let client = Client::from_env()
        .context("No se pudo inicializar el cliente de Anthropic (revisa ANTHROPIC_API_KEY)")?;
    let mut agent = Agent::new(&state, client, model, max_tokens);

    if !args.no_banner {
        print_banner(&style, &state, &agent, args.debug);
    }
    // Opening greeting from the store.
    println!("{}", agent_bubble(&style, &agente_nombre, &saludo));

    if let Some(msg) = args.once {
        println!("{} {}", style.dim("cliente ▸"), msg);
        run_turn(&style, &mut agent, &agente_nombre, &msg, args.debug);
        return Ok(());
    }

    repl(&style, &mut agent, &agente_nombre, &despedida, args.debug)
}

fn repl(
    style: &Style,
    agent: &mut Agent,
    agente_nombre: &str,
    despedida: &str,
    debug: bool,
) -> Result<()> {
    let stdin = io::stdin();
    loop {
        print!("{} ", style.dim("cliente ▸"));
        io::stdout().flush().ok();

        let mut line = String::new();
        if stdin.read_line(&mut line)? == 0 {
            println!();
            break; // EOF (Ctrl-D)
        }
        let msg = line.trim();
        if msg.is_empty() {
            continue;
        }
        if matches!(msg.to_lowercase().as_str(), "salir" | "exit" | "quit" | ":q") {
            break;
        }
        run_turn(style, agent, agente_nombre, msg, debug);
    }
    if !despedida.is_empty() {
        println!("{}", agent_bubble(style, agente_nombre, despedida));
    }
    Ok(())
}

fn run_turn(style: &Style, agent: &mut Agent, agente_nombre: &str, msg: &str, debug: bool) {
    match agent.send(msg) {
        Ok(Turn { reply, trace }) => {
            if debug {
                print_trace(style, &trace);
            }
            if !reply.is_empty() {
                println!("{}", agent_bubble(style, agente_nombre, &reply));
            }
        }
        Err(e) => {
            eprintln!("{}", style.paint("31", &format!("⚠ error: {e:#}")));
        }
    }
}

fn agent_bubble(style: &Style, agente_nombre: &str, text: &str) -> String {
    let head = style.agent(&format!("🛍  {agente_nombre} ▸"));
    // Indent multi-line replies under the header.
    let body = text.replace('\n', "\n   ");
    format!("{head} {body}")
}

fn print_trace(style: &Style, trace: &[TraceEntry]) {
    for t in trace {
        let input = compact(&t.input, 200);
        let output = compact(&t.output, 320);
        println!("{}", style.dim(&format!("   → {}({})", t.name, input)));
        println!("{}", style.dim(&format!("   ⇐ {output}")));
    }
}

fn compact(v: &serde_json::Value, max: usize) -> String {
    let s = serde_json::to_string(v).unwrap_or_else(|_| "<json>".into());
    if s.chars().count() > max {
        let truncated: String = s.chars().take(max).collect();
        format!("{truncated}…")
    } else {
        s
    }
}

fn print_banner(style: &Style, state: &AppState, agent: &Agent, debug: bool) {
    let line = "─".repeat(60);
    println!("{}", style.dim(&line));
    println!(
        "  {}  ·  {} productos  ·  {} pedidos",
        style.bold(&state.config.tienda.nombre),
        state.catalog.productos.len(),
        state.catalog.pedidos.len(),
    );
    println!(
        "  modelo: {}  ·  herramientas: {}  ·  debug: {}",
        agent.model(),
        agent.tool_count(),
        if debug { "on" } else { "off" },
    );
    println!(
        "  {}",
        style.dim("escribe tu mensaje · 'salir' para terminar")
    );
    println!("{}", style.dim(&line));
}

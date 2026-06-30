//! Tool layer. Each tool is a pure-ish function over the in-memory catalog +
//! config (the only side effect is reading the system clock for date math).
//!
//! Guardrails are enforced HERE, in code — not just asked for in the prompt:
//!   * `search_products` returns only real catalog rows (empty ⇒ "no existe").
//!   * `create_order_link` refuses out-of-stock / insufficient-stock SKUs.
//!   * `start_return` enforces the return window from config.
//!
//! The prompt restates these, but the code is the backstop.

use serde_json::{json, Value};

use crate::config::StoreConfig;
use crate::model::Catalog;

pub mod check_inventory;
pub mod check_shipping;
pub mod create_order_link;
pub mod get_order_status;
pub mod handoff_human;
pub mod search_products;
pub mod start_return;

/// Everything a tool needs at call time.
pub struct AppState {
    pub config: StoreConfig,
    pub catalog: Catalog,
}

impl AppState {
    pub fn new(config: StoreConfig, catalog: Catalog) -> Self {
        AppState { config, catalog }
    }
}

pub type Handler = fn(&AppState, &Value) -> Value;

pub struct ToolSpec {
    pub name: &'static str,
    pub description: String,
    pub input_schema: Value,
    pub enabled: bool,
    pub handler: Handler,
}

/// Build every tool spec, honoring the per-deployment `[flujos]` toggles.
pub fn all_tools(state: &AppState) -> Vec<ToolSpec> {
    let f = &state.config.flujos;
    vec![
        search_products::spec(state, f.search_products),
        check_inventory::spec(f.check_inventory),
        check_shipping::spec(f.check_shipping),
        get_order_status::spec(f.get_order_status),
        start_return::spec(state, f.start_return),
        create_order_link::spec(f.create_order_link),
        handoff_human::spec(f.handoff_human),
    ]
}

/// Only the tools enabled for this deployment (exposed to the model).
pub fn enabled_tools(state: &AppState) -> Vec<ToolSpec> {
    all_tools(state).into_iter().filter(|t| t.enabled).collect()
}

/// Route a tool call by name. Unknown or disabled ⇒ structured error
/// (defense in depth: the model can never invoke a gated capability).
pub fn dispatch(state: &AppState, name: &str, input: &Value) -> Value {
    for t in all_tools(state) {
        if t.name == name {
            if !t.enabled {
                return json!({ "error": "herramienta_deshabilitada", "tool": name });
            }
            return (t.handler)(state, input);
        }
    }
    json!({ "error": "herramienta_desconocida", "tool": name })
}

/// Helper: read an optional string field from tool input, trimmed & non-empty.
pub(crate) fn str_field(input: &Value, key: &str) -> Option<String> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Helper: read an optional u32 field (accepts number or numeric string).
pub(crate) fn u32_field(input: &Value, key: &str) -> Option<u32> {
    match input.get(key) {
        Some(Value::Number(n)) => n.as_u64().map(|x| x as u32),
        Some(Value::String(s)) => s.trim().parse::<u32>().ok(),
        _ => None,
    }
}

//! Guardrail suite — proves the grounding/safety properties over the REAL seed
//! data (no network, no LLM). These are the invariants the demo's "wow" depends
//! on: zero hallucination, stock-aware selling, returns within policy.

use std::path::PathBuf;

use serde_json::json;

use asistente_de_tienda::config::StoreConfig;
use asistente_de_tienda::date::Date;
use asistente_de_tienda::model::Catalog;
use asistente_de_tienda::tools::start_return::{decide_return, ReturnDecision};
use asistente_de_tienda::tools::{dispatch, enabled_tools, AppState};

fn root(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn state() -> AppState {
    let config = StoreConfig::load(&root("config/store.toml")).expect("config");
    let catalog = Catalog::load(&root("data/products.json"), &root("data/orders.json")).expect("data");
    AppState::new(config, catalog)
}

// ── Grounding: real hit vs. honest miss ─────────────────────────────────────

#[test]
fn search_returns_real_product() {
    let st = state();
    let r = dispatch(&st, "search_products", &json!({ "query": "mochila vortex", "color": "negro" }));
    assert_eq!(r["total"].as_u64(), Some(1));
    let p = &r["resultados"][0];
    assert_eq!(p["sku"].as_str(), Some("VTX"));
    assert_eq!(p["precio_mxn"].as_u64(), Some(1290));
    // color filter narrows variants to the black one only
    assert_eq!(p["variantes"].as_array().unwrap().len(), 1);
    assert_eq!(p["variantes"][0]["sku"].as_str(), Some("VTX-BLK"));
}

#[test]
fn search_unknown_product_is_empty_not_fabricated() {
    let st = state();
    let r = dispatch(&st, "search_products", &json!({ "query": "dron submarino nuclear" }));
    assert_eq!(r["total"].as_u64(), Some(0));
    assert!(r["resultados"].as_array().unwrap().is_empty());
}

// ── Inventory ───────────────────────────────────────────────────────────────

#[test]
fn inventory_variant_in_stock() {
    let st = state();
    let r = dispatch(&st, "check_inventory", &json!({ "sku": "VTX-BLK" }));
    assert_eq!(r["encontrado"].as_bool(), Some(true));
    assert_eq!(r["disponible"].as_bool(), Some(true));
    assert_eq!(r["stock"].as_u64(), Some(14));
}

#[test]
fn inventory_variant_out_of_stock() {
    let st = state();
    let r = dispatch(&st, "check_inventory", &json!({ "sku": "VTX-GRY" }));
    assert_eq!(r["disponible"].as_bool(), Some(false));
    assert_eq!(r["stock"].as_u64(), Some(0));
}

#[test]
fn inventory_unknown_sku() {
    let st = state();
    let r = dispatch(&st, "check_inventory", &json!({ "sku": "ZZZ-000" }));
    assert_eq!(r["encontrado"].as_bool(), Some(false));
}

// ── Stock-aware selling (the hard guardrail) ────────────────────────────────

#[test]
fn cannot_sell_out_of_stock() {
    let st = state();
    let r = dispatch(&st, "create_order_link", &json!({ "sku": "VTX-GRY", "qty": 1 }));
    assert_eq!(r["creado"].as_bool(), Some(false));
    assert_eq!(r["error"].as_str(), Some("sin_stock"));
    assert!(r.get("pay_link").is_none(), "no debe existir link de pago para OOS");
    // offers real, in-stock alternatives of the same product
    let alts = r["alternativas"].as_array().unwrap();
    assert!(!alts.is_empty());
    assert!(alts.iter().all(|a| a["stock"].as_u64().unwrap() > 0));
}

#[test]
fn cannot_oversell_beyond_stock() {
    let st = state();
    let r = dispatch(&st, "create_order_link", &json!({ "sku": "VTX-AZL", "qty": 999 }));
    assert_eq!(r["creado"].as_bool(), Some(false));
    assert_eq!(r["error"].as_str(), Some("stock_insuficiente"));
    assert!(r.get("pay_link").is_none());
}

#[test]
fn pay_link_for_in_stock_sku() {
    let st = state();
    let r = dispatch(&st, "create_order_link", &json!({ "sku": "VTX-BLK", "qty": 1 }));
    assert_eq!(r["creado"].as_bool(), Some(true));
    assert_eq!(r["total_mxn"].as_u64(), Some(1290));
    let link = r["pay_link"].as_str().unwrap();
    assert!(link.contains("sku=VTX-BLK"));
    assert!(link.contains("qty=1"));
}

// ── Shipping ────────────────────────────────────────────────────────────────

#[test]
fn shipping_known_city() {
    let st = state();
    let r = dispatch(&st, "check_shipping", &json!({ "cp_or_ciudad": "Guadalajara" }));
    assert_eq!(r["costo_mxn"].as_u64(), Some(99));
    assert_eq!(r["dias_habiles"].as_str(), Some("2-3"));
}

#[test]
fn shipping_by_cp_prefix() {
    let st = state();
    let r = dispatch(&st, "check_shipping", &json!({ "cp_or_ciudad": "44100" }));
    assert_eq!(r["costo_mxn"].as_u64(), Some(99));
}

#[test]
fn shipping_unlisted_falls_back() {
    let st = state();
    let r = dispatch(&st, "check_shipping", &json!({ "cp_or_ciudad": "Tijuana" }));
    assert_eq!(r["zona"].as_str(), Some("Nacional (general)"));
    assert_eq!(r["costo_mxn"].as_u64(), Some(149));
}

// ── Order status ────────────────────────────────────────────────────────────

#[test]
fn order_status_found() {
    let st = state();
    let r = dispatch(&st, "get_order_status", &json!({ "order_id": "10482" }));
    assert_eq!(r["encontrado"].as_bool(), Some(true));
    assert_eq!(r["estado"].as_str(), Some("en_camino"));
    assert_eq!(r["guia"].as_str(), Some("TRACK-99213"));
}

#[test]
fn order_status_strips_hash() {
    let st = state();
    let r = dispatch(&st, "get_order_status", &json!({ "order_id": "#10482" }));
    assert_eq!(r["encontrado"].as_bool(), Some(true));
}

#[test]
fn order_status_unknown_not_fabricated() {
    let st = state();
    let r = dispatch(&st, "get_order_status", &json!({ "order_id": "99999" }));
    assert_eq!(r["encontrado"].as_bool(), Some(false));
    assert!(r.get("guia").is_none());
}

// ── Returns within policy (pure, deterministic "today") ─────────────────────

#[test]
fn return_eligible_within_window() {
    let st = state();
    let hoy = Date::new(2026, 6, 29);
    match decide_return(st.catalog.order("10488"), st.config.devoluciones.dias, hoy) {
        ReturnDecision::Elegible { rma, dias_transcurridos, .. } => {
            assert_eq!(rma, "RMA-10488");
            assert_eq!(dias_transcurridos, 9); // delivered 2026-06-20
        }
        other => panic!("esperaba Elegible, fue {other:?}"),
    }
}

#[test]
fn return_rejected_outside_window() {
    let st = state();
    let hoy = Date::new(2026, 6, 29);
    match decide_return(st.catalog.order("10310"), st.config.devoluciones.dias, hoy) {
        ReturnDecision::FueraDePlazo { dias_transcurridos, plazo_dias } => {
            assert_eq!(plazo_dias, 30);
            assert_eq!(dias_transcurridos, 50); // delivered 2026-05-10
        }
        other => panic!("esperaba FueraDePlazo, fue {other:?}"),
    }
}

#[test]
fn return_rejected_not_delivered() {
    let st = state();
    let hoy = Date::new(2026, 6, 29);
    match decide_return(st.catalog.order("10500"), st.config.devoluciones.dias, hoy) {
        ReturnDecision::NoEntregado { .. } => {}
        other => panic!("esperaba NoEntregado, fue {other:?}"),
    }
}

#[test]
fn return_unknown_order() {
    assert_eq!(
        decide_return(None, 30, Date::new(2026, 6, 29)),
        ReturnDecision::PedidoNoEncontrado
    );
}

#[test]
fn start_return_dispatch_not_delivered_is_deterministic() {
    let st = state();
    let r = dispatch(&st, "start_return", &json!({ "order_id": "10500", "motivo": "ya no la quiero" }));
    assert_eq!(r["iniciado"].as_bool(), Some(false));
    assert_eq!(r["motivo_rechazo"].as_str(), Some("pedido_no_entregado"));
}

// ── Config gating + dispatch hygiene ────────────────────────────────────────

#[test]
fn disabled_flow_is_not_callable() {
    let mut st = state();
    st.config.flujos.create_order_link = false;
    let r = dispatch(&st, "create_order_link", &json!({ "sku": "VTX-BLK" }));
    assert_eq!(r["error"].as_str(), Some("herramienta_deshabilitada"));
    // and it's not advertised to the model
    assert!(enabled_tools(&st).iter().all(|t| t.name != "create_order_link"));
}

#[test]
fn default_exposes_all_seven_tools() {
    let st = state();
    assert_eq!(enabled_tools(&st).len(), 7);
}

#[test]
fn unknown_tool_is_rejected() {
    let st = state();
    let r = dispatch(&st, "definitely_not_a_tool", &json!({}));
    assert_eq!(r["error"].as_str(), Some("herramienta_desconocida"));
}

#[test]
fn handoff_emits_ticket() {
    let st = state();
    let r = dispatch(&st, "handoff_human", &json!({ "motivo": "fuera de alcance", "resumen": "cliente pide factura especial" }));
    assert_eq!(r["handoff"].as_bool(), Some(true));
    assert!(r["ticket"].as_str().unwrap().starts_with("H-"));
}

//! create_order_link — produce a secure checkout link. HARD guardrail: it
//! refuses out-of-stock or insufficient-stock SKUs (offering real alternatives)
//! so the agent can never sell what isn't there. The LLM never touches payment
//! data — payment happens only through the returned link.

use serde_json::{json, Value};

use super::{str_field, u32_field, AppState, ToolSpec};
use crate::model::{Product, SkuResolucion, Variant};

pub fn spec(enabled: bool) -> ToolSpec {
    ToolSpec {
        name: "create_order_link",
        description: "Genera un link de pago seguro para un SKU disponible. Si no hay stock \
                      suficiente, NO crea el link: devuelve alternativas reales. El asistente \
                      nunca pide ni procesa datos de tarjeta."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "sku": { "type": "string", "description": "SKU de variante (preferido) o base si es único." },
                "qty": { "type": "integer", "description": "Cantidad (por defecto 1).", "minimum": 1 }
            },
            "required": ["sku"],
            "additionalProperties": false
        }),
        enabled,
        handler: run,
    }
}

pub fn run(state: &AppState, input: &Value) -> Value {
    let sku = match str_field(input, "sku") {
        Some(s) => s,
        None => return json!({ "creado": false, "error": "sku_requerido" }),
    };
    let qty = u32_field(input, "qty").filter(|q| *q > 0).unwrap_or(1);

    let (p, v) = match state.catalog.resolve_sku(&sku) {
        SkuResolucion::Variante(p, v) => (p, v),
        SkuResolucion::ProductoUnico(p, v) => (p, v),
        SkuResolucion::ProductoAmbiguo(p) => {
            return json!({
                "creado": false,
                "error": "sku_ambiguo",
                "mensaje": "Especifica la variante (color/talla).",
                "variantes": p.variantes.iter().map(|v| json!({
                    "sku": v.sku, "color": v.color, "talla": v.talla, "disponible": v.disponible(),
                })).collect::<Vec<_>>(),
            });
        }
        SkuResolucion::NoEncontrado => {
            return json!({ "creado": false, "error": "sku_no_encontrado", "sku": sku });
        }
    };

    // ── Stock guardrail (code-enforced) ──────────────────────────────────────
    if v.stock == 0 {
        return json!({
            "creado": false,
            "error": "sin_stock",
            "sku": v.sku,
            "stock_disponible": 0,
            "alternativas": alternativas(p, v),
            "mensaje": "Sin stock. Ofrece una alternativa real o propón aviso de reabastecimiento.",
        });
    }
    if v.stock < qty {
        return json!({
            "creado": false,
            "error": "stock_insuficiente",
            "sku": v.sku,
            "solicitado": qty,
            "stock_disponible": v.stock,
            "alternativas": alternativas(p, v),
            "mensaje": "No hay stock suficiente para esa cantidad.",
        });
    }

    let precio = p.precio_de(v);
    let total = precio * qty;
    let pay_link = format!(
        "{}?sku={}&qty={}&amount={}",
        state.config.pagos.pay_link_base, v.sku, qty, total
    );

    json!({
        "creado": true,
        "pay_link": pay_link,
        "sku": v.sku,
        "nombre_es": p.nombre_es,
        "color": v.color,
        "talla": v.talla,
        "qty": qty,
        "precio_unitario_mxn": precio,
        "total_mxn": total,
        "moneda": state.config.tienda.moneda,
        "nota": "Pago seguro. El asistente nunca solicita ni almacena datos de tarjeta.",
    })
}

/// Other in-stock variants of the same product (color/size swaps).
fn alternativas(p: &Product, excluir: &Variant) -> Vec<Value> {
    p.variantes
        .iter()
        .filter(|v| v.sku != excluir.sku && v.disponible())
        .map(|v| {
            json!({
                "sku": v.sku,
                "color": v.color,
                "talla": v.talla,
                "stock": v.stock,
                "precio_mxn": p.precio_de(v),
            })
        })
        .collect()
}

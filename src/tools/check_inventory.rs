//! check_inventory — real-time stock for a SKU (variant or base product).

use serde_json::{json, Value};

use super::{str_field, AppState, ToolSpec};

pub fn spec(enabled: bool) -> ToolSpec {
    ToolSpec {
        name: "check_inventory",
        description: "Consulta existencias reales de un SKU (variante como 'VTX-BLK' o base como 'VTX'). \
                      Úsala antes de confirmar disponibilidad o de generar un link de pago."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "sku": { "type": "string", "description": "SKU de variante o de producto base." }
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
        None => return json!({ "error": "sku_requerido" }),
    };

    if let Some((p, v)) = state.catalog.variant_by_sku(&sku) {
        return json!({
            "encontrado": true,
            "tipo": "variante",
            "sku": v.sku,
            "nombre_es": p.nombre_es,
            "color": v.color,
            "talla": v.talla,
            "stock": v.stock,
            "disponible": v.disponible(),
            "precio_mxn": p.precio_de(v),
        });
    }

    if let Some(p) = state.catalog.product_by_base_sku(&sku) {
        let variantes: Vec<Value> = p
            .variantes
            .iter()
            .map(|v| {
                json!({
                    "sku": v.sku,
                    "color": v.color,
                    "talla": v.talla,
                    "stock": v.stock,
                    "disponible": v.disponible(),
                })
            })
            .collect();
        return json!({
            "encontrado": true,
            "tipo": "producto",
            "sku": p.sku,
            "nombre_es": p.nombre_es,
            "stock_total": p.stock_total(),
            "disponible": p.stock_total() > 0,
            "variantes": variantes,
        });
    }

    json!({ "encontrado": false, "sku": sku })
}

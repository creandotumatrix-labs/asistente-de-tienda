//! get_order_status — look up a real order. Unknown id ⇒ `encontrado:false`
//! (never fabricate a status or a tracking number).

use serde_json::{json, Value};

use super::{str_field, AppState, ToolSpec};

pub fn spec(enabled: bool) -> ToolSpec {
    ToolSpec {
        name: "get_order_status",
        description: "Consulta el estado real de un pedido por su número. Si no existe, dilo; \
                      no inventes estado ni número de guía."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "order_id": { "type": "string", "description": "Número de pedido (con o sin '#'), p. ej. '10482'." }
            },
            "required": ["order_id"],
            "additionalProperties": false
        }),
        enabled,
        handler: run,
    }
}

pub fn run(state: &AppState, input: &Value) -> Value {
    let order_id = match str_field(input, "order_id") {
        Some(s) => s,
        None => return json!({ "error": "order_id_requerido" }),
    };

    match state.catalog.order(&order_id) {
        None => json!({ "encontrado": false, "order_id": order_id.trim_start_matches('#') }),
        Some(o) => json!({
            "encontrado": true,
            "order_id": o.order_id,
            "cliente": o.cliente,
            "estado": o.estado.slug(),
            "estado_legible": o.estado.legible(),
            "items": o.items,
            "fecha_pedido": o.fecha_pedido,
            "fecha_envio": o.fecha_envio,
            "entrega_estimada": o.entrega_estimada,
            "fecha_entrega": o.fecha_entrega,
            "guia": o.guia,
            "total_mxn": o.total_mxn,
            "ciudad_envio": o.ciudad_envio,
        }),
    }
}

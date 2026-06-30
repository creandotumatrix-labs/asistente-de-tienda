//! handoff_human — escalate to a person with a structured summary. Returns a
//! deterministic ticket id and a customer-facing message.

use serde_json::{json, Value};

use super::{str_field, AppState, ToolSpec};
use crate::util::hash_corto;

pub fn spec(enabled: bool) -> ToolSpec {
    ToolSpec {
        name: "handoff_human",
        description: "Escala la conversación a una persona del equipo. Úsala cuando algo esté \
                      fuera de alcance, fuera de política, o cuando el cliente lo pida."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "motivo":  { "type": "string", "description": "Motivo breve de la escalación." },
                "resumen": { "type": "string", "description": "Resumen del contexto para la persona que recibe el caso." }
            },
            "required": ["motivo", "resumen"],
            "additionalProperties": false
        }),
        enabled,
        handler: run,
    }
}

pub fn run(_state: &AppState, input: &Value) -> Value {
    let motivo = str_field(input, "motivo").unwrap_or_else(|| "sin_especificar".to_string());
    let resumen = str_field(input, "resumen").unwrap_or_default();
    let ticket = format!("H-{}", hash_corto(&format!("{motivo}|{resumen}")));

    json!({
        "handoff": true,
        "ticket": ticket,
        "motivo": motivo,
        "resumen": resumen,
        "mensaje_al_cliente": "Te paso con una persona de nuestro equipo; en breve te contactan por aquí. 🙌",
    })
}

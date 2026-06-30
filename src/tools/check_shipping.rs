//! check_shipping — cost + ETA for a destination, from the config shipping
//! table. Matches by city name (accent/case-insensitive) or CP prefix; always
//! returns a quote (falls back to the configured national default).

use serde_json::{json, Value};

use super::{str_field, AppState, ToolSpec};
use crate::config::EnvioFila;
use crate::util::normaliza;

pub fn spec(enabled: bool) -> ToolSpec {
    ToolSpec {
        name: "check_shipping",
        description: "Calcula costo y tiempo de envío a una ciudad o código postal de México."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "cp_or_ciudad": { "type": "string", "description": "Ciudad (p. ej. 'Guadalajara') o código postal (p. ej. '44100')." }
            },
            "required": ["cp_or_ciudad"],
            "additionalProperties": false
        }),
        enabled,
        handler: run,
    }
}

pub fn run(state: &AppState, input: &Value) -> Value {
    let destino = match str_field(input, "cp_or_ciudad")
        .or_else(|| str_field(input, "destino"))
        .or_else(|| str_field(input, "ciudad"))
        .or_else(|| str_field(input, "cp"))
    {
        Some(d) => d,
        None => return json!({ "error": "destino_requerido" }),
    };

    let envios = &state.config.envios;
    let es_cp = destino.chars().all(|c| c.is_ascii_digit()) && destino.len() >= 2;

    for fila in &envios.tabla {
        if coincide(fila, &destino, es_cp) {
            return json!({
                "encontrado": true,
                "destino": destino,
                "zona": fila.zona,
                "costo_mxn": fila.costo_mxn,
                "dias_habiles": fila.dias,
            });
        }
    }

    json!({
        "encontrado": true,
        "destino": destino,
        "zona": "Nacional (general)",
        "costo_mxn": envios.default_costo_mxn,
        "dias_habiles": envios.default_dias,
        "nota": "Tarifa general; sin zona específica configurada.",
    })
}

fn coincide(fila: &EnvioFila, destino: &str, es_cp: bool) -> bool {
    if es_cp {
        return fila.cp_prefijos.iter().any(|pref| destino.starts_with(pref));
    }
    let d = normaliza(destino);
    fila.ciudades.iter().any(|c| {
        let cn = normaliza(c);
        d.contains(&cn) || cn.contains(&d)
    })
}

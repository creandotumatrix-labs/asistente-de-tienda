//! start_return — initiate an RMA, but ONLY within policy. The eligibility
//! decision is a pure function (`decide_return`) so it can be unit-tested with
//! a fixed "today"; the handler is a thin wrapper that reads the system clock.

use serde_json::{json, Value};

use super::{str_field, AppState, ToolSpec};
use crate::date::{days_between, Date};
use crate::model::{EstadoPedido, Order};

pub fn spec(state: &AppState, enabled: bool) -> ToolSpec {
    let dias = state.config.devoluciones.dias;
    ToolSpec {
        name: "start_return",
        description: format!(
            "Inicia una devolución/cambio (RMA) para un pedido. Solo es elegible si el pedido \
             fue ENTREGADO y está dentro del plazo de {dias} días. Casos fuera de plazo, no \
             entregados o ambiguos → usa handoff_human con el contexto."
        ),
        input_schema: json!({
            "type": "object",
            "properties": {
                "order_id": { "type": "string", "description": "Número de pedido." },
                "motivo":   { "type": "string", "description": "Motivo de la devolución (opcional pero recomendado)." }
            },
            "required": ["order_id"],
            "additionalProperties": false
        }),
        enabled,
        handler: run,
    }
}

/// Pure eligibility decision. `hoy` is injected for deterministic testing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReturnDecision {
    Elegible {
        rma: String,
        plazo_dias: i64,
        dias_transcurridos: i64,
    },
    NoEntregado {
        estado: EstadoPedido,
    },
    FueraDePlazo {
        dias_transcurridos: i64,
        plazo_dias: i64,
    },
    PedidoNoEncontrado,
}

pub fn decide_return(order: Option<&Order>, plazo_dias: i64, hoy: Date) -> ReturnDecision {
    let order = match order {
        Some(o) => o,
        None => return ReturnDecision::PedidoNoEncontrado,
    };

    if order.estado != EstadoPedido::Entregado {
        return ReturnDecision::NoEntregado {
            estado: order.estado,
        };
    }

    // Reference date for the window: delivery date, else ETA, else order date.
    let base = order
        .fecha_entrega
        .as_deref()
        .or(order.entrega_estimada.as_deref())
        .or(Some(order.fecha_pedido.as_str()))
        .and_then(Date::parse)
        .unwrap_or(hoy);

    let dias_transcurridos = days_between(base, hoy);
    if dias_transcurridos > plazo_dias {
        ReturnDecision::FueraDePlazo {
            dias_transcurridos,
            plazo_dias,
        }
    } else {
        ReturnDecision::Elegible {
            rma: format!("RMA-{}", order.order_id),
            plazo_dias,
            dias_transcurridos,
        }
    }
}

pub fn run(state: &AppState, input: &Value) -> Value {
    let order_id = match str_field(input, "order_id") {
        Some(s) => s,
        None => return json!({ "error": "order_id_requerido" }),
    };
    let motivo = str_field(input, "motivo");
    let plazo = state.config.devoluciones.dias;
    let order = state.catalog.order(&order_id);

    match decide_return(order, plazo, Date::today_utc()) {
        ReturnDecision::Elegible {
            rma,
            plazo_dias,
            dias_transcurridos,
        } => json!({
            "iniciado": true,
            "rma": rma,
            "order_id": order.map(|o| o.order_id.clone()),
            "motivo": motivo,
            "plazo_dias": plazo_dias,
            "dias_transcurridos": dias_transcurridos,
            "instrucciones": "Te enviaremos por este medio la guía de devolución prepagada. Empaca el producto sin uso y con sus etiquetas.",
            "condiciones": state.config.devoluciones.condiciones,
        }),
        ReturnDecision::NoEntregado { estado } => json!({
            "iniciado": false,
            "motivo_rechazo": "pedido_no_entregado",
            "estado": estado.slug(),
            "handoff_sugerido": true,
            "mensaje": "El pedido aún no se entrega; no se puede iniciar una devolución.",
        }),
        ReturnDecision::FueraDePlazo {
            dias_transcurridos,
            plazo_dias,
        } => json!({
            "iniciado": false,
            "motivo_rechazo": "fuera_de_plazo",
            "dias_transcurridos": dias_transcurridos,
            "plazo_dias": plazo_dias,
            "handoff_sugerido": true,
            "mensaje": "El pedido está fuera del plazo de devolución; pásalo a un humano con el contexto.",
        }),
        ReturnDecision::PedidoNoEncontrado => json!({
            "iniciado": false,
            "motivo_rechazo": "pedido_no_encontrado",
            "handoff_sugerido": true,
        }),
    }
}

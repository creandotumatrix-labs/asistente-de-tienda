//! search_products — grounded catalog search. Returns ONLY real rows.
//! An empty result is the signal that a product does not exist (no fabrication).

use serde_json::{json, Value};

use super::{str_field, u32_field, AppState, ToolSpec};
use crate::model::{Product, Variant};
use crate::util::{contiene_sinonimo, normaliza, todos_los_tokens};

pub fn spec(state: &AppState, enabled: bool) -> ToolSpec {
    let categorias = {
        let mut c: Vec<String> = Vec::new();
        for p in &state.catalog.productos {
            if !c.contains(&p.categoria) {
                c.push(p.categoria.clone());
            }
        }
        c.join(", ")
    };
    ToolSpec {
        name: "search_products",
        description: format!(
            "Busca productos en el catálogo real de la tienda. Úsala SIEMPRE antes de \
             afirmar que un producto existe, su precio o sus características. Si `total` es 0, \
             el producto NO está en el catálogo: dilo y no inventes. Categorías disponibles: {categorias}."
        ),
        input_schema: json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Texto libre: nombre o palabras clave del producto (p. ej. 'mochila vortex')." },
                "categoria": { "type": "string", "description": "Filtrar por categoría exacta o parcial." },
                "color": { "type": "string", "description": "Filtrar por color (p. ej. 'negro')." },
                "talla": { "type": "string", "description": "Filtrar por talla (p. ej. 'M' o '27')." },
                "max_precio":{ "type": "integer", "description": "Precio máximo en MXN." }
            },
            "additionalProperties": false
        }),
        enabled,
        handler: run,
    }
}

pub fn run(state: &AppState, input: &Value) -> Value {
    let query = str_field(input, "query");
    let categoria = str_field(input, "categoria");
    let color = str_field(input, "color");
    let talla = str_field(input, "talla");
    let max_precio = u32_field(input, "max_precio");

    let mut resultados = Vec::new();

    for p in &state.catalog.productos {
        // Category filter (accent/case-insensitive substring, with cross-language
        // synonym fallback so e.g. "bolsas" matches a "Women's Bags" category
        // that only exists in English in the underlying catalog source).
        if let Some(cat) = &categoria {
            if !contiene_sinonimo(&p.categoria, cat) {
                continue;
            }
        }
        // Free-text query: every token must appear in name+description+category,
        // literally or via a known synonym (see util::todos_los_tokens).
        if let Some(q) = &query {
            let haystack = format!("{} {} {}", p.nombre_es, p.descripcion_es, p.categoria);
            if !todos_los_tokens(&haystack, q) {
                continue;
            }
        }

        // Narrow variants by color/talla/price.
        let matched: Vec<&Variant> = p
            .variantes
            .iter()
            .filter(|v| match &color {
                Some(c) => normaliza(&v.color).contains(&normaliza(c)),
                None => true,
            })
            .filter(|v| match &talla {
                Some(t) => v
                    .talla
                    .as_ref()
                    .map(|vt| normaliza(vt) == normaliza(t))
                    .unwrap_or(false),
                None => true,
            })
            .filter(|v| match max_precio {
                Some(maxp) => p.precio_de(v) <= maxp,
                None => true,
            })
            .collect();

        if matched.is_empty() {
            continue;
        }
        resultados.push(producto_json(p, &matched));
    }

    json!({
        "total": resultados.len(),
        "resultados": resultados,
    })
}

fn producto_json(p: &Product, variantes: &[&Variant]) -> Value {
    let precios: Vec<u32> = variantes.iter().map(|v| p.precio_de(v)).collect();
    let min = precios.iter().copied().min().unwrap_or(p.precio_mxn);
    let max = precios.iter().copied().max().unwrap_or(p.precio_mxn);

    let colores = unicos(variantes.iter().map(|v| v.color.clone()));
    let tallas = unicos(variantes.iter().filter_map(|v| v.talla.clone()));
    let fotos = variantes
        .first()
        .map(|v| p.fotos_de(v))
        .unwrap_or_else(|| p.foto_url.clone());

    let vjson: Vec<Value> = variantes
        .iter()
        .map(|v| {
            json!({
                "sku": v.sku,
                "color": v.color,
                "talla": v.talla,
                "stock": v.stock,
                "disponible": v.disponible(),
                "precio_mxn": p.precio_de(v),
            })
        })
        .collect();

    json!({
        "sku": p.sku,
        "nombre_es": p.nombre_es,
        "categoria": p.categoria,
        "descripcion_es": p.descripcion_es,
        "precio_mxn": min,
        "precio_desde_mxn": min,
        "precio_hasta_mxn": max,
        "colores": colores,
        "tallas": tallas,
        "fotos": fotos,
        "stock_total": variantes.iter().map(|v| v.stock).sum::<u32>(),
        "politica_devolucion": p.politica_devolucion,
        "variantes": vjson,
    })
}

fn unicos<I: IntoIterator<Item = String>>(it: I) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for x in it {
        if !out.iter().any(|e| e == &x) {
            out.push(x);
        }
    }
    out
}

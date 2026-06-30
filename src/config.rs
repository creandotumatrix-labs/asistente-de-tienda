//! White-label store configuration (`config/store.toml`).
//! This is the swap surface: change this file + `data/*.json` → new store.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct StoreConfig {
    pub tienda: Tienda,
    #[serde(default)]
    pub agente: Agente,
    pub persona: Persona,
    pub devoluciones: Devoluciones,
    pub envios: Envios,
    #[serde(default)]
    pub pagos: Pagos,
    #[serde(default)]
    pub flujos: Flujos,
    pub branding: Branding,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Tienda {
    pub nombre: String,
    #[serde(default = "default_moneda")]
    pub moneda: String,
    #[serde(default = "default_idioma")]
    pub idioma: String,
}
fn default_moneda() -> String {
    "MXN".into()
}
fn default_idioma() -> String {
    "es-MX".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct Agente {
    #[serde(default = "default_modelo")]
    pub modelo: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
}
impl Default for Agente {
    fn default() -> Self {
        Agente {
            modelo: default_modelo(),
            max_tokens: default_max_tokens(),
        }
    }
}
fn default_modelo() -> String {
    "claude-sonnet-4-6".into()
}
fn default_max_tokens() -> u32 {
    1024
}

#[derive(Debug, Clone, Deserialize)]
pub struct Persona {
    pub nombre_agente: String,
    #[serde(default)]
    pub descripcion: String,
    #[serde(default)]
    pub tono: Vec<String>,
    #[serde(default)]
    pub reglas_extra: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Devoluciones {
    pub dias: i64,
    pub texto: String,
    #[serde(default)]
    pub condiciones: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Envios {
    pub default_costo_mxn: u32,
    pub default_dias: String,
    #[serde(default)]
    pub tabla: Vec<EnvioFila>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EnvioFila {
    pub zona: String,
    #[serde(default)]
    pub ciudades: Vec<String>,
    #[serde(default)]
    pub cp_prefijos: Vec<String>,
    pub costo_mxn: u32,
    pub dias: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Pagos {
    #[serde(default = "default_pay_base")]
    pub pay_link_base: String,
}
impl Default for Pagos {
    fn default() -> Self {
        Pagos {
            pay_link_base: default_pay_base(),
        }
    }
}
fn default_pay_base() -> String {
    "https://example.test/checkout".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct Flujos {
    #[serde(default = "si")]
    pub search_products: bool,
    #[serde(default = "si")]
    pub check_inventory: bool,
    #[serde(default = "si")]
    pub check_shipping: bool,
    #[serde(default = "si")]
    pub get_order_status: bool,
    #[serde(default = "si")]
    pub start_return: bool,
    #[serde(default = "si")]
    pub create_order_link: bool,
    #[serde(default = "si")]
    pub handoff_human: bool,
}
fn si() -> bool {
    true
}
impl Default for Flujos {
    fn default() -> Self {
        Flujos {
            search_products: true,
            check_inventory: true,
            check_shipping: true,
            get_order_status: true,
            start_return: true,
            create_order_link: true,
            handoff_human: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Branding {
    pub saludo: String,
    #[serde(default)]
    pub despedida: String,
}

impl StoreConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("leyendo config {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("parseando TOML {}", path.display()))
    }
}

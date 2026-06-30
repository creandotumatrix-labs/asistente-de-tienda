//! Asistente de Tienda — WhatsApp ecommerce/retail support + sales agent (es-MX).
//!
//! Grounded by construction: the model can only state what the tools return,
//! and the tools read from a real catalog + orders with guardrails enforced in
//! code (no out-of-stock sales, returns within policy, no fabricated data).

pub mod agent;
pub mod anthropic;
pub mod config;
pub mod date;
pub mod model;
pub mod prompt;
pub mod tools;
pub mod util;

pub use agent::Agent;
pub use anthropic::Client;
pub use config::StoreConfig;
pub use model::{Catalog, EstadoPedido, Order, OrderItem, Product, Variant};
pub use tools::AppState;

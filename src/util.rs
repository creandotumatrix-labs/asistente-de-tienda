//! Small dependency-free helpers shared across tools.

/// Lowercase + strip common Spanish diacritics for accent/case-insensitive
/// matching (e.g. "Mérida" and "merida" compare equal).
pub fn normaliza(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'á' => 'a',
            'é' => 'e',
            'í' => 'i',
            'ó' => 'o',
            'ú' | 'ü' => 'u',
            'ñ' => 'n',
            other => other,
        })
        .collect()
}

/// True if `needle` (normalized) appears in `haystack` (normalized).
pub fn contiene(haystack: &str, needle: &str) -> bool {
    normaliza(haystack).contains(&normaliza(needle))
}

/// Small bilingual/singular-plural synonym groups so a Spanish query still
/// matches product text sourced in English (e.g. the DummyJSON fallback
/// catalog used when Mercado Libre isn't configured), and so plural/singular
/// forms ("bolsas" vs "bolsa") don't silently miss a real, in-catalog hit.
/// Each inner slice is a set of interchangeable terms — matching ANY one of
/// them against the haystack counts as a hit for the whole group.
const SINONIMOS: &[&[&str]] = &[
    &[
        "bolsa", "bolsas", "bag", "bags", "backpack", "backpacks", "mochila", "mochilas",
        "purse", "purses", "handbag", "handbags",
    ],
    &["lente", "lentes", "gafas", "anteojos", "sunglasses", "glasses"],
    &["perfume", "perfumes", "fragancia", "fragancias", "fragrance", "fragrances", "colonia"],
    &["joyeria", "joya", "joyas", "jewellery", "jewelry", "jewel", "jewels"],
    &["collar", "collares", "necklace", "necklaces"],
    &["arete", "aretes", "pendiente", "pendientes", "earring", "earrings"],
    &["anillo", "anillos", "ring", "rings"],
    &["pulsera", "pulseras", "bracelet", "bracelets"],
    &["mueble", "muebles", "furniture"],
    &["cocina", "kitchen"],
    &["belleza", "beauty", "cosmetico", "cosmeticos", "cosmetic", "cosmetics"],
    &["decoracion", "decoration", "decor"],
    &["hogar", "casa", "home"],
    &["vela", "velas", "candle", "candles"],
    &["lampara", "lamparas", "lamp", "lamps"],
    &["maceta", "macetas", "planta", "plantas", "plant", "plants", "pot", "pots", "flowerpot"],
    &["marco", "marcos", "frame", "frames"],
    &["columpio", "columpios", "swing", "swings"],
    &["espejo", "espejos", "mirror", "mirrors"],
    &["cortina", "cortinas", "curtain", "curtains"],
    &[
        "cojin", "cojines", "almohada", "almohadas", "pillow", "pillows", "cushion", "cushions",
    ],
    &["silla", "sillas", "chair", "chairs"],
    &["mesa", "mesas", "table", "tables"],
    &["mujer", "mujeres", "woman", "women", "womens"],
    &["hombre", "hombres", "man", "men", "mens"],
    &["nino", "ninos", "nina", "ninas", "kid", "kids", "child", "children"],
    // "lentes de sol" / "gafas de sol" → sunglasses: "sol" needs its own
    // mapping since "sunglasses" contains "sun" as a literal substring.
    &["sol", "sun"],
    &["regalo", "regalos", "gift", "gifts"],
    &["artesania", "artesanias", "craft", "crafts", "handmade", "handcraft", "handcrafted"],
];

/// Returns the normalized `tok` plus any known cross-language / singular-plural
/// synonyms, so callers can check a haystack against every interchangeable form.
pub fn sinonimos_de(tok: &str) -> Vec<&'static str> {
    for grupo in SINONIMOS {
        if grupo.contains(&tok) {
            return grupo.to_vec();
        }
    }
    Vec::new()
}

/// Like `contiene`, but a miss on the literal term also tries its known
/// synonyms before giving up (e.g. needle "bolsas" also tries "bag", "backpack").
pub fn contiene_sinonimo(haystack: &str, needle: &str) -> bool {
    let hay = normaliza(haystack);
    let n = normaliza(needle);
    if hay.contains(&n) {
        return true;
    }
    sinonimos_de(&n).iter().any(|alt| hay.contains(*alt))
}

/// Filler words that carry no product-matching signal on their own (Spanish
/// articles/prepositions/conjunctions plus a few common verbs from casual
/// questions like "tienen X de Y"). Skipped so a natural phrase like "bolsas
/// de mujer" doesn't fail on the literal, untranslatable "de".
const STOPWORDS: &[&str] = &[
    "de", "del", "la", "el", "los", "las", "un", "una", "unos", "unas", "y", "o", "en", "con",
    "sin", "para", "por", "que", "tienen", "tiene", "tienes", "hay", "algo", "alguna", "algun",
    "algunos", "algunas",
];

/// All non-stopword, whitespace-separated tokens of `query` are present in
/// `haystack`, where a token may match literally OR via a known synonym.
/// Used for grounded product search ("mochila vortex" → both words must hit;
/// "bolsas" also hits a haystack that only says "bolsa" or "bag"; "bolsas de
/// mujer" skips "de" and still requires "bolsas" and "mujer" to resolve).
pub fn todos_los_tokens(haystack: &str, query: &str) -> bool {
    let hay = normaliza(haystack);
    let q = normaliza(query);
    let mut toks: Vec<&str> = q
        .split_whitespace()
        .filter(|t| !STOPWORDS.contains(t))
        .collect();
    // If the query was ONLY stopwords, fall back to the raw tokens rather
    // than vacuously matching every product.
    if toks.is_empty() {
        toks = q.split_whitespace().collect();
    }
    toks.iter().all(|tok| {
        if hay.contains(tok) {
            return true;
        }
        sinonimos_de(tok).iter().any(|alt| hay.contains(*alt))
    })
}

/// Deterministic short id (FNV-1a → base36, 6 chars). Same input → same id,
/// which keeps handoff/RMA references stable and unit-testable.
pub fn hash_corto(s: &str) -> String {
    let mut h: u32 = 2_166_136_261;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16_777_619);
    }
    let mut out = Vec::new();
    let alphabet = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut n = h as u64;
    if n == 0 {
        out.push(b'0');
    }
    while n > 0 {
        out.push(alphabet[(n % 36) as usize]);
        n /= 36;
    }
    while out.len() < 6 {
        out.push(b'0');
    }
    out.truncate(6);
    out.reverse();
    String::from_utf8(out).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normaliza_quita_acentos() {
        assert_eq!(normaliza("Mérida"), "merida");
        assert_eq!(normaliza(" CAFÉ Ñandú "), "cafe nandu");
    }

    #[test]
    fn tokens_de_busqueda() {
        assert!(todos_los_tokens("Mochila Vortex negro 28L", "mochila vortex"));
        assert!(!todos_los_tokens("Mochila Vortex", "mochila escolar"));
    }

    #[test]
    fn hash_estable() {
        assert_eq!(hash_corto("abc"), hash_corto("abc"));
        assert_eq!(hash_corto("abc").len(), 6);
    }

    #[test]
    fn sinonimos_cruzan_idioma() {
        // Spanish query hits an English-only haystack (DummyJSON-style fallback data).
        assert!(todos_los_tokens("Prada Women Bag", "bolsas"));
        assert!(todos_los_tokens("White Faux Leather Backpack", "bolsa"));
        assert!(!todos_los_tokens("Table Lamp", "bolsas"));
    }

    #[test]
    fn sinonimos_singular_plural() {
        // Plural query still hits a singular product name in the real catalog.
        assert!(todos_los_tokens("Bolsa bordada a mano de Chiapas", "bolsas"));
    }

    #[test]
    fn contiene_sinonimo_categoria() {
        assert!(contiene_sinonimo("Women's Bags", "bolsas"));
        assert!(contiene_sinonimo("Sunglasses", "lentes"));
        assert!(!contiene_sinonimo("Kitchen Accessories", "bolsas"));
    }

    #[test]
    fn sinonimos_frase_completa() {
        // Real production repro: "bolsas de mujer" (multi-token, includes a
        // gendered qualifier) must still resolve an English-sourced product.
        assert!(todos_los_tokens("Prada Women Bag", "bolsas de mujer"));
        assert!(todos_los_tokens("Women's Bags", "bolsas mujer"));
    }

    #[test]
    fn sinonimos_lentes_de_sol() {
        // Real production repro: "lentes de sol" missed "Sunglasses" because
        // "sol" had no synonym and isn't a literal substring on its own.
        assert!(todos_los_tokens("Classic Sunglasses", "lentes de sol"));
        assert!(todos_los_tokens("Blue Sunglasses", "gafas de sol"));
    }
}

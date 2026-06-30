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

/// All whitespace-separated tokens of `query` are present in `haystack`.
/// Used for grounded product search ("mochila vortex" → both words must hit).
pub fn todos_los_tokens(haystack: &str, query: &str) -> bool {
    let hay = normaliza(haystack);
    let q = normaliza(query);
    q.split_whitespace().all(|tok| hay.contains(tok))
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
        assert_eq!(normaliza("  CAFÉ Ñandú "), "cafe nandu");
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
}

#!/usr/bin/env python3
"""
Parity / logic oracle for the Asistente de Tienda guardrails.

Why this exists: it independently re-implements the same grounding + guardrail
logic the Rust tool layer enforces, runs it against the *exact same* seed data
(data/*.json) and white-label config (config/store.toml), and asserts the same
outcomes as tests/guardrails.rs. It is an executable cross-check of the data +
algorithm design that runs anywhere Python 3.8+ is available — no Rust toolchain,
no network, no LLM.

Run: python3 scripts/verify_logic.py
Exit: 0 if every invariant holds, 1 otherwise.
"""
from __future__ import annotations

import json
import re
import sys
import unicodedata
from datetime import date
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TODAY = date(2026, 6, 29)  # fixed "today" so the oracle is deterministic

# ── tiny TOML subset reader (store.toml only) ───────────────────────────────
def load_toml(path: Path) -> dict:
    root: dict = {}
    cur = root
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        m = re.match(r"\[\[(.+?)\]\]$", line)
        if m:  # array-of-tables
            cur = _descend(root, m.group(1).split("."), array=True)
            continue
        m = re.match(r"\[(.+?)\]$", line)
        if m:  # table
            cur = _descend(root, m.group(1).split("."), array=False)
            continue
        if "=" in line:
            k, v = line.split("=", 1)
            cur[k.strip()] = _value(v.strip())
    return root

def _descend(root: dict, parts: list[str], array: bool) -> dict:
    node = root
    for p in parts[:-1]:
        node = node.setdefault(p, {})
        if isinstance(node, list):
            node = node[-1]
    last = parts[-1]
    if array:
        node.setdefault(last, [])
        d: dict = {}
        node[last].append(d)
        return d
    return node.setdefault(last, {})

def _value(s: str):
    if s.startswith("[") and s.endswith("]"):
        inner = s[1:-1].strip()
        if not inner:
            return []
        return [_value(x.strip()) for x in _split_array(inner)]
    if s.startswith('"') and s.endswith('"'):
        return s[1:-1]
    if re.match(r"^-?\d+$", s):
        return int(s)
    return s.strip('"')

def _split_array(inner: str) -> list[str]:
    out, buf, depth, q = [], "", 0, False
    for ch in inner:
        if ch == '"':
            q = not q
        if ch == "," and not q and depth == 0:
            out.append(buf)
            buf = ""
        else:
            buf += ch
    if buf.strip():
        out.append(buf)
    return out

# ── helpers mirroring the Rust logic ────────────────────────────────────────
def norm(s: str) -> str:
    s = unicodedata.normalize("NFKD", s)
    s = "".join(c for c in s if not unicodedata.combining(c))
    return s.lower().strip()

def search_products(products, query=None, color=None, talla=None, categoria=None, max_precio=None):
    out = []
    for p in products:
        if categoria and norm(categoria) not in norm(p["categoria"]):
            continue
        if query:
            hay = norm(f"{p['nombre_es']} {p['descripcion_es']} {p['categoria']}")
            if not all(tok in hay for tok in norm(query).split()):
                continue
        matched = []
        for v in p["variantes"]:
            if color and norm(color) not in norm(v["color"]):
                continue
            if talla and (v.get("talla") is None or norm(str(v["talla"])) != norm(str(talla))):
                continue
            price = v.get("precio_mxn") or p["precio_mxn"]
            if max_precio is not None and price > max_precio:
                continue
            matched.append(v)
        if matched:
            out.append((p, matched))
    return out

def variant_by_sku(products, sku):
    for p in products:
        for v in p["variantes"]:
            if v["sku"].lower() == sku.lower():
                return p, v
    return None

def create_order_link(products, sku, qty, pay_base):
    hit = variant_by_sku(products, sku)
    if not hit:
        return {"creado": False, "error": "sku_no_encontrado"}
    p, v = hit
    if v["stock"] == 0:
        alts = [a for a in p["variantes"] if a["sku"] != v["sku"] and a["stock"] > 0]
        return {"creado": False, "error": "sin_stock", "alternativas": alts}
    if v["stock"] < qty:
        return {"creado": False, "error": "stock_insuficiente"}
    price = v.get("precio_mxn") or p["precio_mxn"]
    return {"creado": True, "pay_link": f"{pay_base}?sku={v['sku']}&qty={qty}&amount={price*qty}",
            "total_mxn": price * qty}

def check_shipping(cfg, destino):
    es_cp = destino.isdigit() and len(destino) >= 2
    for row in cfg["envios"]["tabla"]:
        if es_cp:
            if any(destino.startswith(str(pref)) for pref in row.get("cp_prefijos", [])):
                return {"zona": row["zona"], "costo_mxn": row["costo_mxn"], "dias": row["dias"]}
        else:
            d = norm(destino)
            if any(d in norm(c) or norm(c) in d for c in row.get("ciudades", [])):
                return {"zona": row["zona"], "costo_mxn": row["costo_mxn"], "dias": row["dias"]}
    return {"zona": "Nacional (general)", "costo_mxn": cfg["envios"]["default_costo_mxn"],
            "dias": cfg["envios"]["default_dias"]}

def order_by_id(orders, oid):
    oid = norm(oid.lstrip("#"))
    return next((o for o in orders if norm(o["order_id"]) == oid), None)

def return_decision(order, plazo_dias):
    if order is None:
        return ("pedido_no_encontrado", None)
    if order["estado"] != "entregado":
        return ("no_entregado", None)
    base = order.get("fecha_entrega") or order.get("entrega_estimada") or order["fecha_pedido"]
    days = (TODAY - date.fromisoformat(base)).days
    if days > plazo_dias:
        return ("fuera_de_plazo", days)
    return ("elegible", days)

# ── assertion runner ────────────────────────────────────────────────────────
PASS, FAIL = 0, 0

def check(name, cond):
    global PASS, FAIL
    if cond:
        PASS += 1
        print(f"  \033[32m✓\033[0m {name}")
    else:
        FAIL += 1
        print(f"  \033[31m✗ {name}\033[0m")

def main() -> int:
    products = json.loads((ROOT / "data/products.json").read_text(encoding="utf-8"))
    orders = json.loads((ROOT / "data/orders.json").read_text(encoding="utf-8"))
    cfg = load_toml(ROOT / "config/store.toml")
    plazo = cfg["devoluciones"]["dias"]
    pay_base = cfg["pagos"]["pay_link_base"]

    print("grounding")
    res = search_products(products, query="plato talavera", color="azul")
    check("busca 'plato talavera' azul → 1 producto (TAL-002/TAL-002-AZL)",
          len(res) == 1 and res[0][0]["sku"] == "TAL-002"
          and [v["sku"] for v in res[0][1]] == ["TAL-002-AZL"])
    check("producto inexistente → 0 resultados (sin alucinar)",
          len(search_products(products, query="dron submarino nuclear")) == 0)

    print("inventario")
    _, v = variant_by_sku(products, "TAL-002-AZL")
    check("TAL-002-AZL disponible, stock 5", v["stock"] == 5 and v["stock"] > 0)
    _, vg = variant_by_sku(products, "ALB-004-MUL")
    check("ALB-004-MUL agotado (stock 0)", vg["stock"] == 0)
    check("SKU desconocido no existe", variant_by_sku(products, "ZZZ-000") is None)

    print("venta consciente de stock")
    oos = create_order_link(products, "JOY-004-P8", 1, pay_base)
    check("no vende agotado (sin pay_link)", oos["creado"] is False and "pay_link" not in oos)
    check("ofrece alternativas reales en stock",
          len(oos["alternativas"]) > 0 and all(a["stock"] > 0 for a in oos["alternativas"]))
    over = create_order_link(products, "TAL-002-AZL", 999, pay_base)
    check("no sobrevende (stock_insuficiente)", over["error"] == "stock_insuficiente")
    ok = create_order_link(products, "TAL-002-AZL", 1, pay_base)
    check("genera link para SKU en stock (total 650)",
          ok["creado"] and ok["total_mxn"] == 650 and "sku=TAL-002-AZL" in ok["pay_link"])

    print("envíos")
    check("Guadalajara → $99 / 2-3 días",
          check_shipping(cfg, "Guadalajara") == {"zona": "Guadalajara (ZMG)", "costo_mxn": 99, "dias": "2-3"})
    check("CP 44100 → $99 (prefijo ZMG)", check_shipping(cfg, "44100")["costo_mxn"] == 99)
    fb = check_shipping(cfg, "Tijuana")
    check("ciudad sin tabla → tarifa general", fb["zona"] == "Nacional (general)" and fb["costo_mxn"] == 149)

    print("pedidos")
    o = order_by_id(orders, "10482")
    check("pedido 10482 en_camino, guía TRACK-99213",
          o["estado"] == "en_camino" and o["guia"] == "TRACK-99213")
    check("acepta '#10482'", order_by_id(orders, "#10482") is not None)
    check("pedido inexistente → None (sin inventar guía)", order_by_id(orders, "99999") is None)

    print("devoluciones (hoy = 2026-06-29)")
    check("10488 entregado hace 9 días → elegible",
          return_decision(order_by_id(orders, "10488"), plazo) == ("elegible", 9))
    check("10310 entregado hace 50 días → fuera de plazo",
          return_decision(order_by_id(orders, "10310"), plazo) == ("fuera_de_plazo", 50))
    check("10500 (pagado, no entregado) → no_entregado",
          return_decision(order_by_id(orders, "10500"), plazo)[0] == "no_entregado")
    check("pedido inexistente → pedido_no_encontrado",
          return_decision(None, plazo)[0] == "pedido_no_encontrado")

    print(f"\n{PASS} passed, {FAIL} failed")
    return 1 if FAIL else 0

if __name__ == "__main__":
    sys.exit(main())

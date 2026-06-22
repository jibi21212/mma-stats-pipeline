#!/usr/bin/env python3
"""Hermetic STUB for the MMA ML sidecar (drop-in for ``ml/serve.py``).

Point the TUI at this via ``MMA_SIDECAR=<abs path to this file>`` so E2E tests
run OFFLINE and DETERMINISTICALLY with no real model, no sklearn, no DB. It
speaks the exact same newline-delimited JSON ("JSON lines") protocol as the real
sidecar: one compact JSON request object per line on stdin, one response line
per request on stdout (echoing the request ``id``); stderr is logs only.

Design goals:
  * NO heavy imports (no sklearn / numpy / joblib) -> starts INSTANTLY.
  * CANNED, deterministic data for a small known roster.
  * Flush after EVERY line so the Rust client never blocks.
  * NEVER crash on bad input -> emit ``{"ok": false, "error": ...}`` instead.

Canned roster (sorted, 3 fighters):
    ["Alex Pereira", "Israel Adesanya", "Robert Whittaker"]

Responses:
    ping       -> {"ok": true}
    status     -> {"ok": true, "model_loaded": true, "n_fighters": 3,
                   "metrics": {"test_accuracy": 0.6}, "model_path": "<stub>"}
    roster     -> {"ok": true, "fighters": [...the 3 above...]}
    eligibility-> {"ok": true, "rules": {...RULES...},
                   "divisions": {<fighter>: [[gender, ordinal], ...]},
                   "weight_classes": [{"name", "gender", "ordinal"}, ...]}
                  (see RULES + DIVISIONS + WEIGHT_CLASSES below; ok:false only if a
                   model were "not trained", which the stub never is)
    predict    -> {"ok": true, "result": {... allowed, prob_a 0.62 / prob_b 0.38,
                   full tale_a / tale_b with every key, model "stub",
                   test_accuracy 0.6 ...}}
    reload     -> {"ok": true, "model_loaded": true, "n_fighters": 3}
    anything else / bad input -> {"ok": false, "error": "..."}

ELIGIBILITY (startup, ONE shot): the TUI calls ``eligibility`` exactly once at
startup, then filters eligible opponents LOCALLY for every slot selection (no
per-selection round-trip). The response carries:

  * RULES — the eligibility POLICY the TUI applies locally (these mirror the real
    sidecar's predict.py constants):
        {"max_distance": 1, "allow_cross_gender": false,
         "allow_unknown_division": true}

  * DIVISIONS — CANNED per-fighter (gender, ordinal) divisions chosen to make the
    local filter TESTABLE:
        "Alex Pereira"     -> [["M", 8]]   # Heavyweight  (the ODD ONE OUT)
        "Israel Adesanya"  -> [["M", 6]]   # Middleweight
        "Robert Whittaker" -> [["M", 6]]   # Middleweight

So the local rule (min |ord_a - ord_b| <= rules.max_distance on a shared gender)
gives:
    eligible(Adesanya, Whittaker) = true   # both M#6, distance 0
    eligible(Adesanya, Pereira)   = false  # M#6 vs M#8, distance 2 (> 1)
"Alex Pereira" is the load-bearing INELIGIBLE fighter: when "Israel Adesanya"
holds one slot, Pereira must be filtered OUT of the OTHER slot's pool, so a Rust
e2e can prove the filtering is real (not just "all-but-A"). Adesanya<->Whittaker
stays eligible.

  * WEIGHT_CLASSES — the canned weight-class ladder (mirrors the real sidecar's
    predict.weight_class_ladder(), built FROM MEN_LADDER + WOMEN_LADDER). Each
    entry is {"name", "gender", "ordinal"}. These are CONSISTENT with DIVISIONS:
        {"name": "Middleweight", "gender": "M", "ordinal": 6}  # Adesanya + Whittaker
        {"name": "Heavyweight",  "gender": "M", "ordinal": 8}  # Pereira
    A fighter "is in" a class C iff their DIVISIONS contain [C.gender, C.ordinal].
    So selecting "Middleweight" (M#6) surfaces Adesanya + Whittaker; selecting
    "Heavyweight" (M#8) surfaces only Pereira. This lets a Rust e2e prove the
    weight-class filter narrows the candidate pool by class membership (and that
    it composes with the eligibility RULES on the other slot).
"""

import json
import sys

# Sorted to match the real sidecar (which sorts snapshot keys).
ROSTER = ["Alex Pereira", "Israel Adesanya", "Robert Whittaker"]
MODEL_PATH = "<stub>/predictor.joblib"
TEST_ACCURACY = 0.6

# Canned eligibility POLICY for the "eligibility" command. These mirror the real
# sidecar's predict.py constants (MAX_DIVISION_DISTANCE / ALLOW_CROSS_GENDER /
# ALLOW_UNKNOWN_DIVISION); the TUI applies them LOCALLY to DIVISIONS' ordinals.
RULES = {
    "max_distance": 1,
    "allow_cross_gender": False,
    "allow_unknown_division": True,
}

# Canned per-fighter division metadata for the "eligibility" command. Each value
# is a list of [gender, ordinal] pairs mirroring the real sidecar's serialised
# payload["divisions"]. "Alex Pereira" sits at Heavyweight (M#8) while the other
# two are Middleweight (M#6), so the local filter (RULES.max_distance == 1) makes
# Pereira INELIGIBLE vs Adesanya/Whittaker (distance 2 > 1) but keeps Adesanya and
# Whittaker eligible vs each other (distance 0). This is what lets a Rust e2e prove
# the OTHER slot is genuinely filtered, not just "all-but-A".
DIVISIONS = {
    "Alex Pereira": [["M", 8]],      # Heavyweight  -> the ineligible odd one out
    "Israel Adesanya": [["M", 6]],   # Middleweight
    "Robert Whittaker": [["M", 6]],  # Middleweight
}

# Canned weight-class ladder for the "eligibility" command. Mirrors the real
# sidecar's predict.weight_class_ladder() (built FROM MEN_LADDER + WOMEN_LADDER):
# each entry is {"name", "gender", "ordinal"}. CONSISTENT with DIVISIONS above so
# the TUI's weight-class filter is TESTABLE: a fighter "is in" a class C iff their
# DIVISIONS contain [C.gender, C.ordinal]. Middleweight (M#6) surfaces Adesanya +
# Whittaker; Heavyweight (M#8) surfaces only Pereira -- so a Rust e2e can prove the
# class filter narrows the pool by membership AND composes with RULES.
WEIGHT_CLASSES = [
    {"name": "Middleweight", "gender": "M", "ordinal": 6},  # Adesanya + Whittaker
    {"name": "Heavyweight", "gender": "M", "ordinal": 8},   # Pereira
]


def _tale(elo, age, record, reach_in, height_in, stance,
          recent_winrate, form_delta, layoff_days, divisions):
    """Build a tale-of-the-tape dict with EVERY key the Rust model expects."""
    return {
        "elo": elo,
        "age": age,
        "record": record,
        "reach_in": reach_in,
        "height_in": height_in,
        "stance": stance,
        "recent_winrate": recent_winrate,
        "form_delta": form_delta,
        "layoff_days": layoff_days,
        "divisions": divisions,
    }


def _predict_result(name_a, name_b):
    """Deterministic ALLOWED prediction: prob_a 0.62, prob_b 0.38."""
    return {
        "name_a": name_a,
        "name_b": name_b,
        "allowed": True,
        "reason": None,
        "prob_a": 0.62,
        "prob_b": 0.38,
        "low_confidence": False,
        "distance": 0,
        "tale_a": _tale(
            elo=1650.0, age=34.0, record="24-3", reach_in=80.0,
            height_in=76.0, stance="Orthodox", recent_winrate=0.8,
            form_delta=0.2, layoff_days=180.0,
            divisions=["Middleweight", "Light Heavyweight"],
        ),
        "tale_b": _tale(
            elo=1580.0, age=33.0, record="25-7", reach_in=73.5,
            height_in=72.0, stance="Orthodox", recent_winrate=0.6,
            form_delta=-0.1, layoff_days=300.0,
            divisions=["Middleweight"],
        ),
        "model": "stub",
        "test_accuracy": TEST_ACCURACY,
    }


def handle(req):
    """Pure dispatch: request dict -> response dict (echoes ``id``)."""
    if not isinstance(req, dict):
        return {"id": None, "ok": False, "error": "request must be a JSON object"}

    rid = req.get("id")
    cmd = req.get("cmd")

    if cmd == "ping":
        return {"id": rid, "ok": True}
    if cmd == "status":
        return {
            "id": rid, "ok": True,
            "model_loaded": True,
            "n_fighters": len(ROSTER),
            "metrics": {"test_accuracy": TEST_ACCURACY},
            "model_path": MODEL_PATH,
        }
    if cmd == "roster":
        return {"id": rid, "ok": True, "fighters": list(ROSTER)}
    if cmd == "eligibility":
        # Policy RULES + per-fighter DIVISIONS + the WEIGHT_CLASSES ladder, fetched
        # ONCE at startup. The TUI filters eligible opponents LOCALLY from these
        # (no per-selection IPC) and offers a weight-class picker over
        # WEIGHT_CLASSES (a fighter "is in" a class iff its [gender, ordinal] is in
        # that fighter's DIVISIONS).
        return {"id": rid, "ok": True,
                "rules": dict(RULES),
                "divisions": {k: [list(p) for p in v] for k, v in DIVISIONS.items()},
                "weight_classes": [dict(c) for c in WEIGHT_CLASSES]}
    if cmd == "predict":
        a = req.get("a")
        b = req.get("b")
        if not isinstance(a, str) or not isinstance(b, str) or not a or not b:
            return {"id": rid, "ok": False,
                    "error": "predict requires string fields 'a' and 'b'"}
        return {"id": rid, "ok": True, "result": _predict_result(a, b)}
    if cmd == "reload":
        return {"id": rid, "ok": True,
                "model_loaded": True, "n_fighters": len(ROSTER)}

    return {"id": rid, "ok": False, "error": "unknown command: %r" % (cmd,)}


def main():
    out = sys.stdout
    sys.stderr.write("stub_sidecar: ready (%d fighters)\n" % len(ROSTER))
    sys.stderr.flush()
    for raw in sys.stdin:
        line = raw.strip()
        if not line:
            continue  # ignore blank keep-alive lines
        try:
            req = json.loads(line)
        except Exception as exc:  # never crash on bad input
            resp = {"id": None, "ok": False, "error": "invalid JSON: %s" % exc}
        else:
            try:
                resp = handle(req)
            except Exception as exc:  # defensive: still never crash
                rid = req.get("id") if isinstance(req, dict) else None
                resp = {"id": rid, "ok": False, "error": "stub error: %s" % exc}
        out.write(json.dumps(resp, separators=(",", ":")) + "\n")
        out.flush()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

"""Long-lived Python prediction SIDECAR for the Rust TUI.

This process is spawned ONCE by the Rust front-end and kept alive for the
duration of the session. It loads the pickled sklearn model exactly once (via
``ml/predict.py``) and then answers requests over stdin/stdout using a simple
newline-delimited JSON ("JSON lines") protocol. ALL prediction logic lives in
Python here; Rust never reimplements any ML math -- it just shuttles JSON.

PROTOCOL (JSON lines)
=====================
ONE compact JSON object per line, no embedded newlines. ``stdin`` carries
requests (Rust -> sidecar); ``stdout`` carries responses (sidecar -> Rust);
``stderr`` is for human-readable logs ONLY (never structured output).

Requests each carry an integer ``id`` and a string ``cmd``:
    {"id":N,"cmd":"ping"}
    {"id":N,"cmd":"status"}
    {"id":N,"cmd":"roster"}
    {"id":N,"cmd":"eligibility"}
    {"id":N,"cmd":"predict","a":"<name>","b":"<name>"}
    {"id":N,"cmd":"reload"}

Responses echo the same ``id`` and carry an ``ok`` boolean:
    ok:    {"id":N,"ok":true, ...payload}
    error: {"id":N,"ok":false,"error":"<message>"}

Payloads:
    ping       -> {"ok":true}
    status     -> {"ok":true,"model_loaded":bool,"n_fighters":int,
                   "metrics":{...}|null,"model_path":str}
    roster     -> {"ok":true,"fighters":[...]}   (ok:false "model not trained")
    eligibility-> {"ok":true,
                   "rules":{"max_distance":1,"allow_cross_gender":false,
                            "allow_unknown_division":true},
                   "divisions":{"<fighter>":[["M",6],["M",7]], ...},
                   "weight_classes":[{"name":"Flyweight","gender":"M",
                                      "ordinal":1}, ...,
                                     {"name":"Women's Strawweight","gender":"W",
                                      "ordinal":1}, ...]}
                  The TUI calls this exactly ONCE at startup. ``rules`` is the
                  eligibility POLICY, serialised straight from the predict.py
                  constants (MAX_DIVISION_DISTANCE / ALLOW_CROSS_GENDER /
                  ALLOW_UNKNOWN_DIVISION). ``divisions`` maps each fighter to a
                  list of [gender "M"|"W", ordinal int] pairs (empty list if none
                  resolvable), from the model payload's ``divisions``.
                  ``weight_classes`` is the full ladder (predict.weight_class_ladder(),
                  built FROM MEN_LADDER + WOMEN_LADDER): a list of
                  {"name","gender","ordinal"} dicts. A fighter "is in" a class C iff
                  their ``divisions`` contain [C.gender, C.ordinal], so the TUI can
                  offer a weight-class picker and filter the candidate pool to those
                  who fought in the chosen class -- composing with ``rules``. The TUI
                  then filters eligible opponents LOCALLY for every slot selection
                  (like fuzzy search) -- ZERO per-selection round-trips. The ladder
                  definitions (names + ordinals) live ONLY here in Python; Rust only
                  ever compares the ordinals it was handed against ``rules`` and the
                  (gender, ordinal) identities in ``weight_classes``.
                  (ok:false "model not trained" when no model is loaded.)
    predict    -> {"ok":true,"result":{...predict() dict, NaN/Inf -> null...}}
                  (ok:false if no model / unknown fighter)
    reload     -> {"ok":true,"model_loaded":bool,"n_fighters":int}

LIFECYCLE
=========
On startup the sidecar TRIES to load the model. If the model file is absent it
STAYS UP and keeps answering ``ping``/``status`` (so the TUI can report state),
while ``roster``/``predict`` return ``ok:false`` with a clear "model not
trained" error so the TUI can offer to train. ``reload`` re-attempts loading
(``predict.load_model(force=True)``) and refreshes the cached state.

ROBUSTNESS GUARANTEES
=====================
* The loop NEVER crashes on bad input -- malformed JSON, unknown commands and
  missing fields all produce an ``ok:false`` error response.
* Every response is a single compact line, written and then FLUSHED.
* ``predict()`` results can contain NaN/Inf (missing reach/height/age). Before
  serialising, those are recursively replaced with ``null`` -- we do NOT rely on
  ``json``'s ``allow_nan`` (which would emit non-standard ``NaN`` tokens Rust's
  serde cannot parse).

The request dispatcher is the PURE function ``handle(request, state) -> dict``,
so tests can exercise every command without touching real stdin/stdout.
"""

from __future__ import annotations

import json
import math
import os
import sys

# Make ``import predict`` work whether run from ml/ or as ml.serve.
_THIS_DIR = os.path.dirname(os.path.abspath(__file__))
if _THIS_DIR not in sys.path:
    sys.path.insert(0, _THIS_DIR)

import predict  # noqa: E402  (local module; path set above)


# --------------------------------------------------------------------------- #
# State
# --------------------------------------------------------------------------- #

class State:
    """Mutable holder for the loaded model payload and derived caches.

    ``payload`` is the dict returned by ``predict.load_model`` (or ``None`` when
    no model has been trained yet). ``roster`` / ``metrics`` are cached views so
    repeated ``status``/``roster`` calls don't rebuild them.
    """

    __slots__ = ("payload", "roster", "metrics", "model_path", "load_error")

    def __init__(self):
        self.payload = None
        self.roster = []
        self.metrics = None
        self.model_path = predict.MODEL_PATH
        self.load_error = None

    @property
    def model_loaded(self) -> bool:
        return self.payload is not None

    def n_fighters(self) -> int:
        return len(self.roster)

    def refresh_from_payload(self, payload) -> None:
        """Recompute cached roster/metrics from a freshly loaded payload."""
        self.payload = payload
        if payload is None:
            self.roster = []
            self.metrics = None
            return
        snaps = payload.get("snapshots") or {}
        self.roster = sorted(snaps.keys())
        self.metrics = payload.get("metrics")


def load_state(force: bool = False) -> State:
    """Build a fresh State by attempting to load the model.

    Never raises on a missing model: a FileNotFoundError leaves the State in the
    model-not-trained condition (``payload is None``) with ``load_error`` set.
    """
    state = State()
    try:
        payload = predict.load_model(predict.MODEL_PATH, force=force)
        state.refresh_from_payload(payload)
    except FileNotFoundError as exc:
        state.load_error = str(exc)
        state.refresh_from_payload(None)
    return state


# --------------------------------------------------------------------------- #
# NaN/Inf sanitiser
# --------------------------------------------------------------------------- #

def sanitize(obj):
    """Recursively replace NaN/Inf floats with None so output is valid JSON.

    Standard JSON has no representation for NaN/Inf; Rust's serde_json rejects
    the non-standard ``NaN``/``Infinity`` tokens that ``json.dumps`` would emit
    with ``allow_nan=True``. We therefore convert them to ``null`` BEFORE
    serialising and dump with ``allow_nan=False`` as a belt-and-braces guard.

    Handles dicts, lists/tuples/sets, floats (incl. numpy float scalars via the
    ``float`` cast in callers) and leaves other scalars untouched.
    """
    if isinstance(obj, float):
        if math.isnan(obj) or math.isinf(obj):
            return None
        return obj
    if isinstance(obj, dict):
        return {k: sanitize(v) for k, v in obj.items()}
    if isinstance(obj, (list, tuple)):
        return [sanitize(v) for v in obj]
    if isinstance(obj, set):
        return [sanitize(v) for v in obj]
    # numpy scalar floats expose .item(); normalise them too.
    item = getattr(obj, "item", None)
    if callable(item) and obj.__class__.__module__ == "numpy":
        try:
            return sanitize(item())
        except Exception:  # pragma: no cover - defensive
            return obj
    return obj


# --------------------------------------------------------------------------- #
# Command handlers (pure)
# --------------------------------------------------------------------------- #

_MODEL_NOT_TRAINED = "model not trained"


def _status_payload(state: State) -> dict:
    return {
        "ok": True,
        "model_loaded": state.model_loaded,
        "n_fighters": state.n_fighters(),
        "metrics": state.metrics if state.model_loaded else None,
        "model_path": state.model_path,
    }


def _divisions_payload(state: State) -> dict:
    """Serialise the model's ``divisions`` map to JSON-friendly lists.

    Maps each fighter name to a sorted list of ``[gender, ordinal]`` pairs (the
    persisted ``(gender, ordinal)`` tuples; empty list when a fighter has no
    resolvable division). The caller (``handle``) guarantees the model is loaded.
    NEVER raises on a malformed payload entry: anything that does not unpack into
    a ``(gender, ordinal)`` pair is skipped, so one bad entry can't crash the loop.

    This is the ``divisions`` half of the ``eligibility`` response; the ``rules``
    half is ``predict.eligibility_rules()`` (sourced from the policy constants).
    """
    payload = state.payload or {}
    raw = payload.get("divisions") or {}
    out = {}
    for name, divs in raw.items():
        pairs = []
        for div in divs or ():
            try:
                gender, ordinal = div
            except (TypeError, ValueError):
                continue  # skip anything not shaped like (gender, ordinal)
            pairs.append([gender, int(ordinal)])
        # Sort for a stable, deterministic response.
        out[name] = sorted(pairs)
    return out


def handle(request: dict, state: State) -> dict:
    """Dispatch ONE request against ``state`` and return a response dict.

    PURE: does no I/O. The response always includes an echoed ``id`` (the
    request's ``id`` if present, else null) and an ``ok`` boolean. This function
    must never raise on bad input -- it returns ``{"ok": False, "error": ...}``
    instead, so the main loop can serialise it like any other response.
    """
    if not isinstance(request, dict):
        return {"id": None, "ok": False, "error": "request must be a JSON object"}

    rid = request.get("id")
    cmd = request.get("cmd")

    def err(msg: str) -> dict:
        return {"id": rid, "ok": False, "error": msg}

    def ok(payload: dict) -> dict:
        return {"id": rid, **payload}

    if not isinstance(cmd, str) or not cmd:
        return err("missing or invalid 'cmd'")

    if cmd == "ping":
        return ok({"ok": True})

    if cmd == "status":
        return ok(_status_payload(state))

    if cmd == "roster":
        if not state.model_loaded:
            return err(_MODEL_NOT_TRAINED)
        return ok({"ok": True, "fighters": list(state.roster)})

    if cmd == "eligibility":
        # The eligibility POLICY + per-fighter division metadata + the weight-class
        # ladder, fetched ONCE at startup by the TUI so it can filter eligible
        # opponents LOCALLY (like fuzzy search) -- ZERO per-selection round-trips.
        # ``rules`` is sourced from the predict.py policy constants (so what Python
        # GATES and what it TELLS the TUI can never diverge); ``divisions`` is the
        # persisted {name -> set of (gender, ordinal)} map serialised to
        # {name: [[gender, ord]]} (empty list when no resolvable division);
        # ``weight_classes`` is predict.weight_class_ladder() (built FROM the
        # MEN_LADDER + WOMEN_LADDER constants), the single source for the TUI's
        # weight-class picker -- a fighter "is in" a class iff its [gender, ordinal]
        # appears in that fighter's ``divisions``. The TUI applies ``rules`` to
        # those ordinals exactly as gate_matchup does; the real weight-string
        # parsing + the ladder definitions (names + ordinals) stay here.
        if not state.model_loaded:
            return err(_MODEL_NOT_TRAINED)
        divs = _divisions_payload(state)
        return ok({
            "ok": True,
            "rules": predict.eligibility_rules(),
            "divisions": divs,
            "weight_classes": predict.weight_class_ladder(),
        })

    if cmd == "predict":
        if not state.model_loaded:
            return err(_MODEL_NOT_TRAINED)
        a = request.get("a")
        b = request.get("b")
        if not isinstance(a, str) or not isinstance(b, str) or not a or not b:
            return err("predict requires string fields 'a' and 'b'")
        try:
            result = predict.predict(a, b, path=state.model_path)
        except FileNotFoundError:
            # Model vanished between load and call -> report not trained.
            return err(_MODEL_NOT_TRAINED)
        except Exception as exc:  # pragma: no cover - defensive
            return err(f"prediction failed: {exc}")
        if not result.get("allowed", False) and result.get("reason"):
            # Surface refusals (unknown fighter, gating) as ok:false so the TUI
            # can show the reason without parsing a nested 'allowed' flag.
            if "unknown fighter" in str(result.get("reason", "")):
                return err(result["reason"])
        return ok({"ok": True, "result": sanitize(result)})

    if cmd == "reload":
        try:
            payload = predict.load_model(state.model_path, force=True)
            state.refresh_from_payload(payload)
            state.load_error = None
        except FileNotFoundError as exc:
            state.load_error = str(exc)
            state.refresh_from_payload(None)
        return ok({
            "ok": True,
            "model_loaded": state.model_loaded,
            "n_fighters": state.n_fighters(),
        })

    return err(f"unknown command: {cmd}")


# --------------------------------------------------------------------------- #
# I/O loop
# --------------------------------------------------------------------------- #

def _log(msg: str) -> None:
    """Write a human-readable line to stderr (logs only; never stdout)."""
    print(msg, file=sys.stderr, flush=True)


def _write_response(out, response: dict) -> None:
    """Serialise ONE response as a compact JSON line and FLUSH.

    ``allow_nan=False`` is a guard: ``sanitize`` should have removed every
    NaN/Inf already, but if some slipped through we fall back to a JSON-safe
    error rather than emitting an unparseable ``NaN`` token.
    """
    try:
        line = json.dumps(response, separators=(",", ":"), allow_nan=False)
    except (ValueError, TypeError) as exc:
        safe = {
            "id": response.get("id") if isinstance(response, dict) else None,
            "ok": False,
            "error": f"failed to serialise response: {exc}",
        }
        line = json.dumps(safe, separators=(",", ":"), allow_nan=False)
    out.write(line + "\n")
    out.flush()


def serve(stdin=None, stdout=None, state: State | None = None) -> int:
    """Run the request/response loop until stdin is exhausted (EOF).

    Reads one JSON object per input line, dispatches it via ``handle``, and
    writes one compact JSON response line per request. Malformed lines yield an
    ``ok:false`` error response and DO NOT terminate the loop.
    """
    stdin = stdin if stdin is not None else sys.stdin
    stdout = stdout if stdout is not None else sys.stdout
    if state is None:
        state = load_state()

    if state.model_loaded:
        _log(f"sidecar: model loaded ({state.n_fighters()} fighters) "
             f"from {state.model_path}")
    else:
        _log(f"sidecar: no model ({state.load_error or 'not trained'}); "
             f"serving ping/status, predict/roster will report not trained")

    for raw in stdin:
        line = raw.strip()
        if not line:
            continue  # ignore blank keep-alive lines
        try:
            request = json.loads(line)
        except (ValueError, json.JSONDecodeError) as exc:
            _write_response(stdout, {"id": None, "ok": False,
                                     "error": f"invalid JSON: {exc}"})
            continue
        response = handle(request, state)
        _write_response(stdout, response)

    _log("sidecar: stdin closed, exiting")
    return 0


def main(argv=None) -> int:
    return serve()


if __name__ == "__main__":
    raise SystemExit(main())

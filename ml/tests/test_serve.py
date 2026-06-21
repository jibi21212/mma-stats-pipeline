"""Unit tests for the prediction SIDECAR (ml/serve.py).

Run from the ml/ directory:  python -m pytest tests/test_serve.py -q

These tests exercise the PURE ``handle(request, state)`` dispatcher and the
NaN/Inf sanitiser WITHOUT a real joblib model or real stdin/stdout. We build a
minimal fake payload/State and monkeypatch ``predict.predict`` /
``predict.load_model`` where a command needs to call into predict.py.

Covered: ping; status (model present vs absent); roster (present vs absent);
predict happy path; predict unknown fighter (ok:false); bad/unknown cmd
(ok:false); malformed request; NaN/Inf -> null conversion; and the full
stdin/stdout loop via in-memory string streams.
"""
import io
import json
import math
import os
import sys

import pytest

# Make the ml/ modules importable from ml/tests/.
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import predict as P  # noqa: E402
import serve as S  # noqa: E402


# --------------------------------------------------------------------------- #
# Fixtures: a minimal fake payload + State (no joblib needed)
# --------------------------------------------------------------------------- #

def _fake_payload():
    """A minimal payload shaped like predict.load_model()'s return value."""
    return {
        "snapshots": {"Alice": {}, "Bob": {}, "Carol": {}},
        "divisions": {},
        "static": {},
        "metrics": {"best_model": "hgb", "test_accuracy": 0.63},
        "pipeline": object(),
    }


@pytest.fixture
def loaded_state():
    """A State with a model loaded (roster/metrics derived from fake payload)."""
    state = S.State()
    state.model_path = "/tmp/fake-predictor.joblib"
    state.refresh_from_payload(_fake_payload())
    return state


@pytest.fixture
def empty_state():
    """A State in the model-not-trained condition (no payload)."""
    state = S.State()
    state.model_path = "/tmp/missing-predictor.joblib"
    state.load_error = "No saved model at '/tmp/missing-predictor.joblib'."
    state.refresh_from_payload(None)
    return state


# --------------------------------------------------------------------------- #
# ping
# --------------------------------------------------------------------------- #

def test_ping(loaded_state):
    resp = S.handle({"id": 1, "cmd": "ping"}, loaded_state)
    assert resp == {"id": 1, "ok": True}


def test_ping_works_without_model(empty_state):
    # ping must stay up even with no model.
    resp = S.handle({"id": 7, "cmd": "ping"}, empty_state)
    assert resp["id"] == 7 and resp["ok"] is True


# --------------------------------------------------------------------------- #
# status (model present vs absent)
# --------------------------------------------------------------------------- #

def test_status_model_present(loaded_state):
    resp = S.handle({"id": 2, "cmd": "status"}, loaded_state)
    assert resp["id"] == 2
    assert resp["ok"] is True
    assert resp["model_loaded"] is True
    assert resp["n_fighters"] == 3
    assert resp["metrics"] == {"best_model": "hgb", "test_accuracy": 0.63}
    assert resp["model_path"] == "/tmp/fake-predictor.joblib"


def test_status_model_absent(empty_state):
    resp = S.handle({"id": 3, "cmd": "status"}, empty_state)
    assert resp["id"] == 3
    assert resp["ok"] is True          # status itself succeeds...
    assert resp["model_loaded"] is False   # ...but reports no model
    assert resp["n_fighters"] == 0
    assert resp["metrics"] is None
    assert resp["model_path"] == "/tmp/missing-predictor.joblib"


# --------------------------------------------------------------------------- #
# roster
# --------------------------------------------------------------------------- #

def test_roster_present_sorted(loaded_state):
    resp = S.handle({"id": 4, "cmd": "roster"}, loaded_state)
    assert resp["id"] == 4
    assert resp["ok"] is True
    assert resp["fighters"] == ["Alice", "Bob", "Carol"]  # sorted


def test_roster_absent_reports_not_trained(empty_state):
    resp = S.handle({"id": 5, "cmd": "roster"}, empty_state)
    assert resp["id"] == 5
    assert resp["ok"] is False
    assert "not trained" in resp["error"]


# --------------------------------------------------------------------------- #
# predict happy path (monkeypatched predict.predict)
# --------------------------------------------------------------------------- #

def test_predict_happy_path(loaded_state, monkeypatch):
    captured = {}

    def fake_predict(a, b, path=None):
        captured["args"] = (a, b, path)
        return {
            "name_a": a, "name_b": b, "allowed": True, "reason": None,
            "prob_a": 0.62, "prob_b": 0.38, "low_confidence": False,
            "distance": 0, "tale_a": {"elo": 1500.0}, "tale_b": {"elo": 1490.0},
            "model": "hgb", "test_accuracy": 0.63,
        }

    monkeypatch.setattr(P, "predict", fake_predict)
    resp = S.handle({"id": 6, "cmd": "predict", "a": "Alice", "b": "Bob"},
                    loaded_state)
    assert resp["id"] == 6
    assert resp["ok"] is True
    assert resp["result"]["prob_a"] == 0.62
    assert resp["result"]["name_a"] == "Alice"
    # predict() was called with the state's model_path.
    assert captured["args"] == ("Alice", "Bob", "/tmp/fake-predictor.joblib")


def test_predict_without_model_reports_not_trained(empty_state):
    resp = S.handle({"id": 8, "cmd": "predict", "a": "Alice", "b": "Bob"},
                    empty_state)
    assert resp["ok"] is False
    assert "not trained" in resp["error"]


def test_predict_missing_fields(loaded_state):
    resp = S.handle({"id": 9, "cmd": "predict", "a": "Alice"}, loaded_state)
    assert resp["ok"] is False
    assert "'a' and 'b'" in resp["error"]


# --------------------------------------------------------------------------- #
# predict unknown fighter -> ok:false
# --------------------------------------------------------------------------- #

def test_predict_unknown_fighter(loaded_state, monkeypatch):
    def fake_predict(a, b, path=None):
        return {"name_a": a, "name_b": b, "allowed": False,
                "reason": f"unknown fighter: {a}", "prob_a": None,
                "prob_b": None}

    monkeypatch.setattr(P, "predict", fake_predict)
    resp = S.handle({"id": 10, "cmd": "predict", "a": "Ghost", "b": "Bob"},
                    loaded_state)
    assert resp["id"] == 10
    assert resp["ok"] is False
    assert "unknown fighter" in resp["error"]


def test_predict_gating_refusal_is_ok_true_result(loaded_state, monkeypatch):
    # A gating refusal (not unknown fighter) is a successful response whose
    # result carries allowed=False -- the TUI shows the reason from the result.
    def fake_predict(a, b, path=None):
        return {"name_a": a, "name_b": b, "allowed": False,
                "reason": "too far apart in weight class", "prob_a": None,
                "prob_b": None, "distance": 5}

    monkeypatch.setattr(P, "predict", fake_predict)
    resp = S.handle({"id": 11, "cmd": "predict", "a": "Alice", "b": "Bob"},
                    loaded_state)
    assert resp["ok"] is True
    assert resp["result"]["allowed"] is False
    assert "too far apart" in resp["result"]["reason"]


# --------------------------------------------------------------------------- #
# eligibility (STARTUP: policy RULES + per-fighter division metadata, ONE shot)
# --------------------------------------------------------------------------- #

def _divisions_state():
    """A loaded State whose payload carries a real ``divisions`` map.

    Mirrors the persisted shape (name -> set of (gender, ordinal) tuples) with
    cross-gender + no-division variety so the serialised response can be asserted.
    """
    state = S.State()
    state.model_path = "/tmp/fake-predictor.joblib"
    payload = _fake_payload()
    payload["snapshots"] = {
        "Middle": {}, "LightHeavy": {}, "WomanBantam": {}, "NoDivision": {},
    }
    payload["divisions"] = {
        "Middle": {("M", 6), ("M", 7)},   # two divisions -> two pairs, sorted
        "LightHeavy": {("M", 7)},
        "WomanBantam": {("W", 3)},
        "NoDivision": set(),              # no resolvable division -> empty list
    }
    state.refresh_from_payload(payload)
    return state


def test_eligibility_happy_path():
    state = _divisions_state()
    resp = S.handle({"id": 20, "cmd": "eligibility"}, state)
    assert resp["id"] == 20
    assert resp["ok"] is True

    # RULES are present and serialised FROM the predict.py policy constants (never
    # re-typed), so the wire policy can never drift from what gate_matchup gates.
    rules = resp["rules"]
    assert rules == {
        "max_distance": P.MAX_DIVISION_DISTANCE,
        "allow_cross_gender": P.ALLOW_CROSS_GENDER,
        "allow_unknown_division": P.ALLOW_UNKNOWN_DIVISION,
    }
    # Documented default policy values (the contract the TUI applies locally).
    assert rules == {
        "max_distance": 1,
        "allow_cross_gender": False,
        "allow_unknown_division": True,
    }

    # DIVISIONS: each entry is a list of [gender, ordinal] pairs (sorted), empty
    # when a fighter has no resolvable division.
    divs = resp["divisions"]
    assert divs["Middle"] == [["M", 6], ["M", 7]]
    assert divs["LightHeavy"] == [["M", 7]]
    assert divs["WomanBantam"] == [["W", 3]]
    assert divs["NoDivision"] == []
    # Every fighter in the roster is present.
    assert set(divs.keys()) == {"Middle", "LightHeavy", "WomanBantam", "NoDivision"}


def test_eligibility_response_is_json_serialisable():
    # The wire format must serialise cleanly (lists, ints, strs, bools only).
    state = _divisions_state()
    resp = S.handle({"id": 25, "cmd": "eligibility"}, state)
    text = json.dumps(resp, allow_nan=False)
    round_trip = json.loads(text)
    assert round_trip["divisions"]["Middle"] == [["M", 6], ["M", 7]]
    assert round_trip["rules"]["max_distance"] == 1
    assert round_trip["rules"]["allow_cross_gender"] is False
    assert round_trip["rules"]["allow_unknown_division"] is True


def test_eligibility_without_model_reports_not_trained(empty_state):
    resp = S.handle({"id": 21, "cmd": "eligibility"}, empty_state)
    assert resp["id"] == 21
    assert resp["ok"] is False
    assert "not trained" in resp["error"]


def test_eligibility_never_crashes_on_bad_entry():
    # A malformed division entry (not a (gender, ordinal) pair) is skipped, never
    # crashes the handler.
    state = S.State()
    state.model_path = "/tmp/fake-predictor.joblib"
    payload = _fake_payload()
    payload["snapshots"] = {"Alice": {}}
    payload["divisions"] = {"Alice": {("M", 6), "garbage", ("M",)}}
    state.refresh_from_payload(payload)
    resp = S.handle({"id": 22, "cmd": "eligibility"}, state)
    assert resp["ok"] is True
    # Only the well-formed pair survives; the bad entries are skipped.
    assert resp["divisions"]["Alice"] == [["M", 6]]
    # Rules are still present even when divisions need sanitising.
    assert resp["rules"]["max_distance"] == 1


# --------------------------------------------------------------------------- #
# bad / unknown command -> ok:false
# --------------------------------------------------------------------------- #

def test_unknown_command(loaded_state):
    resp = S.handle({"id": 12, "cmd": "frobnicate"}, loaded_state)
    assert resp["id"] == 12
    assert resp["ok"] is False
    assert "unknown command" in resp["error"]


def test_missing_cmd(loaded_state):
    resp = S.handle({"id": 13}, loaded_state)
    assert resp["ok"] is False
    assert "cmd" in resp["error"]


def test_non_dict_request(loaded_state):
    resp = S.handle(["not", "a", "dict"], loaded_state)
    assert resp["ok"] is False
    assert resp["id"] is None


def test_id_echoed_even_when_absent(loaded_state):
    resp = S.handle({"cmd": "ping"}, loaded_state)
    assert resp["id"] is None
    assert resp["ok"] is True


# --------------------------------------------------------------------------- #
# reload (monkeypatched predict.load_model)
# --------------------------------------------------------------------------- #

def test_reload_loads_model(empty_state, monkeypatch):
    monkeypatch.setattr(P, "load_model", lambda path, force=False: _fake_payload())
    resp = S.handle({"id": 14, "cmd": "reload"}, empty_state)
    assert resp["ok"] is True
    assert resp["model_loaded"] is True
    assert resp["n_fighters"] == 3
    # State was refreshed in place.
    assert empty_state.model_loaded is True
    assert empty_state.roster == ["Alice", "Bob", "Carol"]


def test_reload_missing_model_stays_untrained(empty_state, monkeypatch):
    def boom(path, force=False):
        raise FileNotFoundError("no model")

    monkeypatch.setattr(P, "load_model", boom)
    resp = S.handle({"id": 15, "cmd": "reload"}, empty_state)
    assert resp["ok"] is True            # reload command succeeds...
    assert resp["model_loaded"] is False     # ...but no model present
    assert resp["n_fighters"] == 0


# --------------------------------------------------------------------------- #
# NaN / Inf -> null sanitiser
# --------------------------------------------------------------------------- #

def test_sanitize_replaces_nan_inf():
    raw = {
        "prob_a": 0.6,
        "reach_in": float("nan"),
        "height_in": float("inf"),
        "neg": float("-inf"),
        "tale_a": {"age": float("nan"), "elo": 1500.0,
                   "divisions": [1, float("nan"), 3]},
        "nested": [{"x": float("nan")}, {"y": 2.0}],
    }
    clean = S.sanitize(raw)
    assert clean["prob_a"] == 0.6
    assert clean["reach_in"] is None
    assert clean["height_in"] is None
    assert clean["neg"] is None
    assert clean["tale_a"]["age"] is None
    assert clean["tale_a"]["elo"] == 1500.0
    assert clean["tale_a"]["divisions"] == [1, None, 3]
    assert clean["nested"][0]["x"] is None
    assert clean["nested"][1]["y"] == 2.0


def test_sanitize_output_is_json_serialisable_without_allow_nan():
    raw = {"a": float("nan"), "b": [float("inf"), 1.0], "c": {"d": float("-inf")}}
    clean = S.sanitize(raw)
    # This is the load-bearing guarantee: serialises with allow_nan=False.
    text = json.dumps(clean, allow_nan=False)
    assert "NaN" not in text and "Infinity" not in text
    assert json.loads(text) == {"a": None, "b": [None, 1.0], "c": {"d": None}}


def test_predict_response_nan_becomes_null(loaded_state, monkeypatch):
    # End-to-end: predict() returns NaN tale-of-the-tape values; the response
    # must serialise to valid JSON with nulls (no NaN tokens).
    def fake_predict(a, b, path=None):
        return {
            "name_a": a, "name_b": b, "allowed": True, "reason": None,
            "prob_a": 0.55, "prob_b": 0.45,
            "tale_a": {"reach_in": float("nan"), "age": float("nan")},
            "tale_b": {"reach_in": 72.0, "age": 30.0},
        }

    monkeypatch.setattr(P, "predict", fake_predict)
    resp = S.handle({"id": 16, "cmd": "predict", "a": "Alice", "b": "Bob"},
                    loaded_state)
    assert resp["result"]["tale_a"]["reach_in"] is None
    assert resp["result"]["tale_a"]["age"] is None
    assert resp["result"]["tale_b"]["reach_in"] == 72.0
    # The whole response serialises cleanly.
    text = json.dumps(resp, allow_nan=False)
    assert "NaN" not in text


# --------------------------------------------------------------------------- #
# State helpers + load_state
# --------------------------------------------------------------------------- #

def test_refresh_from_payload_none_clears():
    state = S.State()
    state.refresh_from_payload(_fake_payload())
    assert state.model_loaded is True
    state.refresh_from_payload(None)
    assert state.model_loaded is False
    assert state.roster == []
    assert state.metrics is None


def test_load_state_missing_model_stays_up(monkeypatch):
    def boom(path, force=False):
        raise FileNotFoundError("no model at path")

    monkeypatch.setattr(P, "load_model", boom)
    state = S.load_state()
    assert state.model_loaded is False
    assert state.load_error is not None
    assert state.n_fighters() == 0


def test_load_state_with_model(monkeypatch):
    monkeypatch.setattr(P, "load_model", lambda path, force=False: _fake_payload())
    state = S.load_state()
    assert state.model_loaded is True
    assert state.roster == ["Alice", "Bob", "Carol"]


# --------------------------------------------------------------------------- #
# Full I/O loop over in-memory string streams (no real stdin/stdout)
# --------------------------------------------------------------------------- #

def test_serve_loop_compact_lines_and_flush(loaded_state):
    # Three requests: ping, status, then a malformed line, then unknown cmd.
    stdin = io.StringIO(
        '{"id":1,"cmd":"ping"}\n'
        '{"id":2,"cmd":"status"}\n'
        'this is not json\n'
        '\n'                                  # blank keep-alive line ignored
        '{"id":3,"cmd":"frobnicate"}\n'
    )
    stdout = io.StringIO()
    rc = S.serve(stdin=stdin, stdout=stdout, state=loaded_state)
    assert rc == 0

    lines = [ln for ln in stdout.getvalue().splitlines() if ln]
    # ping, status, invalid-json, unknown-cmd -> 4 responses (blank skipped).
    assert len(lines) == 4
    # Each line is exactly one compact JSON object (no embedded newlines/spaces).
    parsed = [json.loads(ln) for ln in lines]
    assert parsed[0] == {"id": 1, "ok": True}
    assert parsed[1]["id"] == 2 and parsed[1]["model_loaded"] is True
    assert parsed[2]["ok"] is False and "invalid JSON" in parsed[2]["error"]
    assert parsed[3]["ok"] is False and "unknown command" in parsed[3]["error"]
    # Compact: no spaces after separators in the ping line.
    assert lines[0] == '{"id":1,"ok":true}'


def test_serve_loop_never_crashes_on_garbage(loaded_state):
    stdin = io.StringIO(
        '\x00\x01garbage\n'
        '{"id":1}\n'                  # missing cmd
        '42\n'                        # JSON scalar, not an object
        '{"id":2,"cmd":"ping"}\n'
    )
    stdout = io.StringIO()
    rc = S.serve(stdin=stdin, stdout=stdout, state=loaded_state)
    assert rc == 0
    parsed = [json.loads(ln) for ln in stdout.getvalue().splitlines() if ln]
    assert len(parsed) == 4
    assert parsed[0]["ok"] is False        # garbage -> invalid JSON
    assert parsed[1]["ok"] is False        # missing cmd
    assert parsed[2]["ok"] is False        # scalar 42 is not a dict
    assert parsed[3] == {"id": 2, "ok": True}  # valid ping still answered

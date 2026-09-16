"""Unit tests for ML OpenTelemetry instrumentation.

Uses the in-memory span exporter — no collector required.
"""
from __future__ import annotations

import json

import pytest

import archetypes
import db
import predict as P
import serve as S
import telemetry
from opentelemetry.sdk.trace.export import SpanExportResult


@pytest.fixture
def otel_memory():
    exp = telemetry.setup_test()
    exp.clear()
    yield exp
    exp.clear()


@pytest.fixture
def loaded_state():
    state = S.State()
    state.model_path = "/tmp/fake-predictor.joblib"
    state.refresh_from_payload({
        "snapshots": {"Alice": {}, "Bob": {}},
        "divisions": {},
        "static": {},
        "metrics": {"best_model": "hgb", "test_accuracy": 0.63},
        "pipeline": object(),
    })
    return state


def test_enabled_defaults_off(monkeypatch):
    monkeypatch.delenv("MMA_OTEL", raising=False)
    monkeypatch.delenv("MMA_OTEL_FILE", raising=False)
    monkeypatch.delenv("OTEL_EXPORTER_OTLP_ENDPOINT", raising=False)
    monkeypatch.delenv("OTEL_SDK_DISABLED", raising=False)
    assert telemetry.enabled() is False


def test_enabled_by_file(monkeypatch):
    monkeypatch.delenv("MMA_OTEL", raising=False)
    monkeypatch.delenv("OTEL_EXPORTER_OTLP_ENDPOINT", raising=False)
    monkeypatch.delenv("OTEL_SDK_DISABLED", raising=False)
    monkeypatch.setenv("MMA_OTEL_FILE", "/tmp/spans.jsonl")
    assert telemetry.enabled() is True


def test_nested_spans_share_trace(otel_memory):
    with telemetry.span("ml.run_all"):
        with telemetry.span("ml.cluster", {"ml.k": 6}):
            pass
        with telemetry.span("ml.predict", {"ml.fighter_a": "A", "ml.fighter_b": "B"}):
            pass
    finished = list(otel_memory.get_finished_spans())
    names = {s.name for s in finished}
    assert names == {"ml.run_all", "ml.cluster", "ml.predict"}
    by_name = {s.name: s for s in finished}
    root = by_name["ml.run_all"]
    cluster = by_name["ml.cluster"]
    assert cluster.context.trace_id == root.context.trace_id
    assert cluster.parent.span_id == root.context.span_id
    assert cluster.attributes["ml.k"] == 6


def test_run_archetypes_emits_cluster_span(synthetic_db, tmp_path, otel_memory):
    feats = db.build_fighter_features(db_path=synthetic_db)
    out = archetypes.run_archetypes(features=feats, outdir=str(tmp_path / "arch"), k=3)
    assert out["best_k"] == 3
    finished = list(otel_memory.get_finished_spans())
    cluster = [s for s in finished if s.name == "ml.cluster"]
    assert len(cluster) == 1
    assert cluster[0].attributes["ml.k"] == 3
    assert cluster[0].attributes["ml.best_k"] == 3
    assert cluster[0].attributes["ml.n_fighters"] == out["n_fighters"]


def test_predict_emits_span(otel_memory, monkeypatch):
    monkeypatch.setattr(P, "_predict_impl",
                        lambda *a, **k: {"allowed": True, "name_a": a[0], "name_b": a[1]})
    result = P.predict("Alice", "Bob")
    assert result["allowed"] is True
    finished = list(otel_memory.get_finished_spans())
    pred = [s for s in finished if s.name == "ml.predict"]
    assert len(pred) == 1
    assert pred[0].attributes["ml.fighter_a"] == "Alice"
    assert pred[0].attributes["ml.fighter_b"] == "Bob"
    assert pred[0].attributes["ml.allowed"] is True


def test_sidecar_predict_nests_under_sidecar_span(loaded_state, otel_memory, monkeypatch):
    monkeypatch.setattr(P, "predict",
                        lambda a, b, path=None, snap_a=None, snap_b=None: {
                            "allowed": True, "prob_a": 0.6, "prob_b": 0.4,
                        })
    resp = S.handle({"id": 1, "cmd": "predict", "a": "Alice", "b": "Bob"}, loaded_state)
    assert resp["ok"] is True
    finished = list(otel_memory.get_finished_spans())
    names = [s.name for s in finished]
    assert "ml.sidecar.predict" in names
    side = next(s for s in finished if s.name == "ml.sidecar.predict")
    assert side.attributes["ml.fighter_a"] == "Alice"


def test_jsonl_exporter_schema(tmp_path, otel_memory):
    path = tmp_path / "spans.jsonl"
    exp = telemetry.JSONLSpanExporter(str(path))
    with telemetry.span("ml.train", {"ml.db_path": "/tmp/ufc.db"}):
        pass
    finished = list(otel_memory.get_finished_spans())
    assert exp.export(finished) is SpanExportResult.SUCCESS
    exp.shutdown()
    lines = path.read_text(encoding="utf-8").strip().splitlines()
    assert lines
    rec = json.loads(lines[-1])
    assert rec["name"] == "ml.train"
    assert rec["duration_ns"] >= 0
    assert rec["trace_id"]
    assert rec["span_id"]
    assert rec["attrs"]["ml.db_path"] == "/tmp/ufc.db"

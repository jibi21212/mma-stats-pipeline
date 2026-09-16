"""Opt-in OpenTelemetry for the Python ML pipeline (run_all / predict / serve).

Off unless MMA_OTEL is truthy, MMA_OTEL_FILE is set, or
OTEL_EXPORTER_OTLP_ENDPOINT is set (and OTEL_SDK_DISABLED is not "true").
Tests call setup_test() which installs an in-memory exporter.
"""
from __future__ import annotations

import atexit
import json
import os
import threading
from collections.abc import Sequence
from contextlib import contextmanager
from typing import Any, Iterator, Optional

from opentelemetry import trace
from opentelemetry.exporter.otlp.proto.http.trace_exporter import OTLPSpanExporter
from opentelemetry.sdk.resources import Resource
from opentelemetry.sdk.trace import ReadableSpan, TracerProvider
from opentelemetry.sdk.trace.export import (
    BatchSpanProcessor,
    SimpleSpanProcessor,
    SpanExporter,
    SpanExportResult,
)
from opentelemetry.sdk.trace.export.in_memory_span_exporter import InMemorySpanExporter
from opentelemetry.trace import Span, Status, StatusCode

TRACER_NAME = "mma-ml"

_provider: Optional[TracerProvider] = None
_test_exporter: Optional[InMemorySpanExporter] = None
_lock = threading.Lock()


def _truthy(v: str) -> bool:
    return v.strip().lower() in ("1", "true", "yes", "on")


def enabled() -> bool:
    if os.environ.get("OTEL_SDK_DISABLED", "").lower() == "true":
        return False
    if os.environ.get("MMA_OTEL", "") in ("0", "false", "False"):
        return False
    return (
        _truthy(os.environ.get("MMA_OTEL", ""))
        or bool(os.environ.get("MMA_OTEL_FILE"))
        or bool(os.environ.get("OTEL_EXPORTER_OTLP_ENDPOINT"))
    )


def _status_code(span: ReadableSpan) -> str:
    st = span.status
    if st.status_code is StatusCode.OK:
        return "OK"
    if st.status_code is StatusCode.ERROR:
        return "ERROR"
    return "UNSET"


def _attrs(span: ReadableSpan) -> dict[str, Any]:
    out: dict[str, Any] = {}
    for k, v in (span.attributes or {}).items():
        out[str(k)] = v
    return out


class JSONLSpanExporter(SpanExporter):
    """Write finished spans as JSON lines matching the Go FileExporter schema."""

    def __init__(self, path: str) -> None:
        directory = os.path.dirname(path)
        if directory:
            os.makedirs(directory, exist_ok=True)
        self._path = path
        self._fh = open(path, "a", encoding="utf-8")
        self._lock = threading.Lock()

    def export(self, spans: Sequence[ReadableSpan]) -> SpanExportResult:
        with self._lock:
            for span in spans:
                ctx = span.get_span_context()
                parent = span.parent
                rec = {
                    "name": span.name,
                    "trace_id": format(ctx.trace_id, "032x") if ctx else "",
                    "span_id": format(ctx.span_id, "016x") if ctx else "",
                    "parent_span_id": format(parent.span_id, "016x") if parent else "",
                    "start_unix_nano": span.start_time,
                    "end_unix_nano": span.end_time,
                    "duration_ns": (span.end_time or 0) - (span.start_time or 0),
                    "status_code": _status_code(span),
                    "service_name": (
                        span.resource.attributes.get("service.name")
                        if span.resource else None
                    ),
                    "attrs": _attrs(span),
                }
                self._fh.write(json.dumps(rec, ensure_ascii=False) + "\n")
            self._fh.flush()
        return SpanExportResult.SUCCESS

    def shutdown(self) -> None:
        with self._lock:
            if not self._fh.closed:
                self._fh.close()


def setup(service_name: str = "mma-ml") -> None:
    """Install the global TracerProvider. Safe to call more than once."""
    global _provider
    if not enabled():
        return
    with _lock:
        if _provider is not None:
            return
        resource = Resource.create({
            "service.name": os.environ.get("OTEL_SERVICE_NAME") or service_name,
            "service.version": "1.0.0",
        })
        provider = TracerProvider(resource=resource)

        file_path = os.environ.get("MMA_OTEL_FILE")
        if not file_path and _truthy(os.environ.get("MMA_OTEL", "")):
            file_path = "otel-spans.jsonl"
        if file_path:
            provider.add_span_processor(
                BatchSpanProcessor(JSONLSpanExporter(file_path))
            )

        if os.environ.get("OTEL_EXPORTER_OTLP_ENDPOINT"):
            provider.add_span_processor(BatchSpanProcessor(OTLPSpanExporter()))

        if file_path or os.environ.get("OTEL_EXPORTER_OTLP_ENDPOINT"):
            trace.set_tracer_provider(provider)
            _provider = provider
            atexit.register(shutdown)


def setup_test() -> InMemorySpanExporter:
    """In-memory exporter for unit tests. Never dials a collector.

    The global TracerProvider can only be set once per process, so later
    calls reuse the same provider and clear the exporter.
    """
    global _provider, _test_exporter
    if _test_exporter is not None and _provider is not None:
        _test_exporter.clear()
        return _test_exporter
    exp = InMemorySpanExporter()
    provider = TracerProvider(
        resource=Resource.create({"service.name": "mma-ml-test"}),
    )
    provider.add_span_processor(SimpleSpanProcessor(exp))
    trace.set_tracer_provider(provider)
    _provider = provider
    _test_exporter = exp
    return exp


def shutdown() -> None:
    global _provider
    with _lock:
        if _provider is None:
            return
        _provider.force_flush()
        _provider.shutdown()
        _provider = None


def get_test_exporter() -> Optional[InMemorySpanExporter]:
    return _test_exporter


@contextmanager
def span(name: str, attributes: Optional[dict[str, Any]] = None) -> Iterator[Span]:
    """Start a span as the current span. No-op recording when SDK is unset."""
    tracer = trace.get_tracer(TRACER_NAME)
    with tracer.start_as_current_span(name) as sp:
        if attributes:
            for key, value in attributes.items():
                if value is None:
                    continue
                try:
                    sp.set_attribute(key, value)
                except Exception:
                    sp.set_attribute(key, str(value))
        try:
            yield sp
        except Exception as exc:
            sp.set_status(Status(StatusCode.ERROR, str(exc)))
            sp.record_exception(exc)
            raise

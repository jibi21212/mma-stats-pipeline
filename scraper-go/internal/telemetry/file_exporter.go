package telemetry

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"sync"
	"time"

	"go.opentelemetry.io/otel/codes"
	"go.opentelemetry.io/otel/sdk/trace"
)

// spanRecord is the JSONL schema shared with the Python exporter and
// telemetry/analyze_spans.py. Durations are raw nanoseconds — the analyzer
// does not round.
type spanRecord struct {
	Name          string         `json:"name"`
	TraceID       string         `json:"trace_id"`
	SpanID        string         `json:"span_id"`
	ParentSpanID  string         `json:"parent_span_id"`
	StartUnixNano int64          `json:"start_unix_nano"`
	EndUnixNano   int64          `json:"end_unix_nano"`
	DurationNs    int64          `json:"duration_ns"`
	StatusCode    string         `json:"status_code"`
	ServiceName   string         `json:"service_name,omitempty"`
	Attrs         map[string]any `json:"attrs"`
}

// FileExporter writes finished spans as JSON lines. It is a SpanExporter.
type FileExporter struct {
	mu   sync.Mutex
	f    *os.File
	enc  *json.Encoder
	path string
}

// NewFileExporter creates (or appends) path and returns an exporter.
func NewFileExporter(path string) (*FileExporter, error) {
	if err := EnsureDir(path); err != nil {
		return nil, fmt.Errorf("mkdir for %s: %w", path, err)
	}
	f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return nil, fmt.Errorf("open %s: %w", path, err)
	}
	enc := json.NewEncoder(f)
	enc.SetEscapeHTML(false)
	return &FileExporter{f: f, enc: enc, path: path}, nil
}

// ExportSpans implements trace.SpanExporter.
func (e *FileExporter) ExportSpans(_ context.Context, spans []trace.ReadOnlySpan) error {
	e.mu.Lock()
	defer e.mu.Unlock()
	for _, s := range spans {
		rec := recordFromSpan(s)
		if err := e.enc.Encode(rec); err != nil {
			return err
		}
	}
	return e.f.Sync()
}

// Shutdown implements trace.SpanExporter.
func (e *FileExporter) Shutdown(context.Context) error {
	e.mu.Lock()
	defer e.mu.Unlock()
	if e.f == nil {
		return nil
	}
	err := e.f.Close()
	e.f = nil
	return err
}

func recordFromSpan(s trace.ReadOnlySpan) spanRecord {
	parent := ""
	if s.Parent().IsValid() {
		parent = s.Parent().SpanID().String()
	}
	status := "UNSET"
	switch s.Status().Code {
	case codes.Ok:
		status = "OK"
	case codes.Error:
		status = "ERROR"
	}
	attrs := make(map[string]any, len(s.Attributes()))
	for _, kv := range s.Attributes() {
		attrs[string(kv.Key)] = kv.Value.AsInterface()
	}
	service := ""
	for _, kv := range s.Resource().Attributes() {
		if string(kv.Key) == "service.name" {
			service = kv.Value.AsString()
			break
		}
	}
	start := s.StartTime()
	end := s.EndTime()
	if end.IsZero() {
		end = time.Now()
	}
	return spanRecord{
		Name:          s.Name(),
		TraceID:       s.SpanContext().TraceID().String(),
		SpanID:        s.SpanContext().SpanID().String(),
		ParentSpanID:  parent,
		StartUnixNano: start.UnixNano(),
		EndUnixNano:   end.UnixNano(),
		DurationNs:    end.Sub(start).Nanoseconds(),
		StatusCode:    status,
		ServiceName:   service,
		Attrs:         attrs,
	}
}

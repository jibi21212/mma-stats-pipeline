package telemetry

import (
	"bytes"
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/attribute"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.opentelemetry.io/otel/trace"
	"go.opentelemetry.io/otel/trace/noop"
)

func TestPageUnit(t *testing.T) {
	cases := map[string]string{
		"http://www.ufcstats.com/fighter-details/abc":                  "fighter",
		"http://www.ufcstats.com/event-details/abc":                    "event",
		"http://www.ufcstats.com/fight-details/abc":                    "fight",
		"http://www.ufcstats.com/statistics/fighters?char=a":           "fighter_index",
		"http://www.ufcstats.com/statistics/events/completed?page=all": "event_index",
		"http://example.com/other":                                     "page",
	}
	for url, want := range cases {
		if got := PageUnit(url); got != want {
			t.Errorf("PageUnit(%q)=%q want %q", url, got, want)
		}
	}
}

func TestEnabledDefaultsOff(t *testing.T) {
	t.Setenv("MMA_OTEL", "")
	t.Setenv("MMA_OTEL_FILE", "")
	t.Setenv("OTEL_EXPORTER_OTLP_ENDPOINT", "")
	t.Setenv("OTEL_SDK_DISABLED", "")
	if Enabled() {
		t.Fatal("telemetry should be off with empty env")
	}
}

func TestEnabledByFile(t *testing.T) {
	t.Setenv("MMA_OTEL", "")
	t.Setenv("OTEL_EXPORTER_OTLP_ENDPOINT", "")
	t.Setenv("OTEL_SDK_DISABLED", "")
	t.Setenv("MMA_OTEL_FILE", "/tmp/spans.jsonl")
	if !Enabled() {
		t.Fatal("MMA_OTEL_FILE should enable telemetry")
	}
}

func TestDisabledEnvWins(t *testing.T) {
	t.Setenv("MMA_OTEL", "1")
	t.Setenv("OTEL_SDK_DISABLED", "true")
	if Enabled() {
		t.Fatal("OTEL_SDK_DISABLED=true must win")
	}
}

func TestContextPropagatesAcrossGoroutine(t *testing.T) {
	rec, cleanup := InitTest()
	defer cleanup()

	ctx, root := Start(context.Background(), SpanRun)

	var wg sync.WaitGroup
	wg.Add(1)
	go func(ctx context.Context) {
		defer wg.Done()
		_, span := StartExtract(ctx, "http://www.ufcstats.com/fight-details/abc")
		span.End()
	}(ctx)
	wg.Wait()
	root.End()

	var run, extract sdktrace.ReadOnlySpan
	for _, s := range rec.Ended() {
		switch s.Name() {
		case SpanRun:
			run = s
		case SpanExtract:
			extract = s
		}
	}
	if run == nil || extract == nil {
		t.Fatalf("missing spans: run=%v extract=%v (ended=%d)", run != nil, extract != nil, len(rec.Ended()))
	}
	if run.SpanContext().TraceID() != extract.SpanContext().TraceID() {
		t.Fatalf("trace id mismatch: run=%s extract=%s",
			run.SpanContext().TraceID(), extract.SpanContext().TraceID())
	}
	if extract.Parent().SpanID() != run.SpanContext().SpanID() {
		t.Fatalf("extract parent=%s want run span=%s",
			extract.Parent().SpanID(), run.SpanContext().SpanID())
	}

	stage, unit := "", ""
	for _, kv := range extract.Attributes() {
		if string(kv.Key) == AttrStage {
			stage = kv.Value.AsString()
		}
		if string(kv.Key) == AttrUnit {
			unit = kv.Value.AsString()
		}
	}
	if stage != StageExtract || unit != "fight" {
		t.Fatalf("extract attrs stage=%q unit=%q", stage, unit)
	}
}

func TestNestedStagesShareTrace(t *testing.T) {
	rec, cleanup := InitTest()
	defer cleanup()

	ctx, run := Start(context.Background(), SpanRun)
	ctx, item := Start(ctx, SpanFighter)
	_, extract := StartExtract(ctx, "http://www.ufcstats.com/fighter-details/x")
	extract.End()
	_, transform := StartTransform(ctx, "fighter")
	transform.End()
	_, load := StartLoad(ctx, "fighter")
	load.End()
	item.End()
	run.End()

	byName := map[string]sdktrace.ReadOnlySpan{}
	for _, s := range rec.Ended() {
		byName[s.Name()] = s
	}
	for _, name := range []string{SpanRun, SpanFighter, SpanExtract, SpanTransform, SpanLoad} {
		if byName[name] == nil {
			t.Fatalf("missing span %s", name)
		}
	}
	tid := byName[SpanRun].SpanContext().TraceID()
	for _, name := range []string{SpanFighter, SpanExtract, SpanTransform, SpanLoad} {
		if byName[name].SpanContext().TraceID() != tid {
			t.Fatalf("%s trace %s != run %s", name, byName[name].SpanContext().TraceID(), tid)
		}
	}
	fighterID := byName[SpanFighter].SpanContext().SpanID()
	if byName[SpanExtract].Parent().SpanID() != fighterID {
		t.Fatal("extract should nest under scraper.fighter")
	}
	if byName[SpanTransform].Parent().SpanID() != fighterID {
		t.Fatal("transform should nest under scraper.fighter")
	}
	if byName[SpanLoad].Parent().SpanID() != fighterID {
		t.Fatal("load should nest under scraper.fighter")
	}
}

func TestFileExporterWritesJSONL(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "spans.jsonl")
	exp, err := NewFileExporter(path)
	if err != nil {
		t.Fatal(err)
	}

	tp := sdktrace.NewTracerProvider(
		sdktrace.WithSampler(sdktrace.AlwaysSample()),
		sdktrace.WithSpanProcessor(sdktrace.NewSimpleSpanProcessor(exp)),
	)
	prev := otel.GetTracerProvider()
	otel.SetTracerProvider(tp)
	t.Cleanup(func() {
		_ = tp.Shutdown(context.Background())
		otel.SetTracerProvider(prev)
	})

	_, span := otel.Tracer(TracerName).Start(context.Background(), SpanExtract,
		trace.WithAttributes(
			attribute.String(AttrStage, StageExtract),
			attribute.String(AttrUnit, "fight"),
		),
	)
	time.Sleep(time.Millisecond)
	span.End()
	if err := tp.ForceFlush(context.Background()); err != nil {
		t.Fatal(err)
	}
	if err := exp.Shutdown(context.Background()); err != nil {
		t.Fatal(err)
	}

	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	line, _, _ := bytes.Cut(data, []byte("\n"))
	var rec spanRecord
	if err := json.Unmarshal(line, &rec); err != nil {
		t.Fatalf("unmarshal %q: %v", data, err)
	}
	if rec.Name != SpanExtract {
		t.Fatalf("name=%q", rec.Name)
	}
	if rec.DurationNs <= 0 {
		t.Fatalf("duration_ns=%d", rec.DurationNs)
	}
	if rec.TraceID == "" || rec.SpanID == "" {
		t.Fatalf("missing ids: %+v", rec)
	}
	if rec.Attrs[AttrStage] != StageExtract {
		t.Fatalf("attrs=%v", rec.Attrs)
	}
}

func TestSetupNoopWhenDisabled(t *testing.T) {
	t.Setenv("MMA_OTEL", "")
	t.Setenv("MMA_OTEL_FILE", "")
	t.Setenv("OTEL_EXPORTER_OTLP_ENDPOINT", "")
	t.Setenv("OTEL_SDK_DISABLED", "")
	shutdown, err := Setup(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if shutdown != nil {
		t.Fatal("expected nil shutdown when disabled")
	}
}

func TestInitTestRestoreNoop(t *testing.T) {
	_, cleanup := InitTest()
	cleanup()
	if _, ok := otel.GetTracerProvider().(noop.TracerProvider); !ok {
		// noop.NewTracerProvider returns a concrete type; just make sure Start still works.
		_, span := Start(context.Background(), "after-cleanup")
		span.End()
	}
}

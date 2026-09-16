// Package telemetry is opt-in OpenTelemetry tracing + metrics for the scraper.
//
// It is OFF unless MMA_OTEL is truthy, MMA_OTEL_FILE is set, or
// OTEL_EXPORTER_OTLP_ENDPOINT is set (and OTEL_SDK_DISABLED is not "true").
// Tests use InitTest, which installs an in-memory SpanRecorder and never
// dials a collector.
package telemetry

import (
	"context"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"strings"
	"time"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/codes"
	"go.opentelemetry.io/otel/exporters/otlp/otlpmetric/otlpmetrichttp"
	"go.opentelemetry.io/otel/exporters/otlp/otlptrace/otlptracehttp"
	"go.opentelemetry.io/otel/metric"
	sdkmetric "go.opentelemetry.io/otel/sdk/metric"
	"go.opentelemetry.io/otel/sdk/resource"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.opentelemetry.io/otel/sdk/trace/tracetest"
	"go.opentelemetry.io/otel/trace"
	"go.opentelemetry.io/otel/trace/noop"
)

const (
	TracerName = "mma-scraper"

	SpanRun       = "scraper.run"
	SpanFighters  = "scraper.fighters"
	SpanEvents    = "scraper.events"
	SpanRefresh   = "scraper.refresh"
	SpanFighter   = "scraper.fighter"
	SpanEvent     = "scraper.event"
	SpanFight     = "scraper.fight"
	SpanExtract   = "etl.extract"
	SpanTransform = "etl.transform"
	SpanLoad      = "etl.load"

	AttrStage = "etl.stage"
	AttrUnit  = "etl.unit"

	StageExtract   = "extract"
	StageTransform = "transform"
	StageLoad      = "load"
)

var (
	stageDuration metric.Float64Histogram
	recordCount   metric.Int64Counter
)

func init() {
	initInstruments()
}

func initInstruments() {
	m := otel.Meter(TracerName)
	var err error
	stageDuration, err = m.Float64Histogram(
		"mma.etl.stage.duration",
		metric.WithUnit("s"),
		metric.WithDescription("Duration of one ETL stage operation (extract/transform/load)"),
	)
	if err != nil {
		log.Printf("telemetry: histogram: %v", err)
	}
	recordCount, err = m.Int64Counter(
		"mma.etl.records",
		metric.WithDescription("Records processed by the scraper (fighters, events, fights, round_stats)"),
	)
	if err != nil {
		log.Printf("telemetry: counter: %v", err)
	}
}

// Enabled reports whether production telemetry should start. Tests call
// InitTest instead and do not go through this gate.
func Enabled() bool {
	if strings.EqualFold(os.Getenv("OTEL_SDK_DISABLED"), "true") {
		return false
	}
	if v := os.Getenv("MMA_OTEL"); v == "0" || strings.EqualFold(v, "false") {
		return false
	}
	return isTruthy(os.Getenv("MMA_OTEL")) ||
		os.Getenv("MMA_OTEL_FILE") != "" ||
		os.Getenv("OTEL_EXPORTER_OTLP_ENDPOINT") != ""
}

func isTruthy(v string) bool {
	switch strings.ToLower(strings.TrimSpace(v)) {
	case "1", "true", "yes", "on":
		return true
	default:
		return false
	}
}

// PageUnit classifies a ufcstats.com URL into the ETL unit of work.
func PageUnit(rawURL string) string {
	switch {
	case strings.Contains(rawURL, "/fighter-details/"):
		return "fighter"
	case strings.Contains(rawURL, "/event-details/"):
		return "event"
	case strings.Contains(rawURL, "/fight-details/"):
		return "fight"
	case strings.Contains(rawURL, "/statistics/fighters"):
		return "fighter_index"
	case strings.Contains(rawURL, "/statistics/events"):
		return "event_index"
	default:
		return "page"
	}
}

// Setup installs the global TracerProvider / MeterProvider. The returned
// shutdown flushes exporters; call it on process exit so spans are not lost.
// If telemetry is disabled, Setup returns a nil shutdown and a nil error.
func Setup(ctx context.Context) (func(context.Context) error, error) {
	if !Enabled() {
		return nil, nil
	}

	service := os.Getenv("OTEL_SERVICE_NAME")
	if service == "" {
		service = "mma-scraper"
	}
	res, err := resource.New(ctx,
		resource.WithAttributes(
			attribute.String("service.name", service),
			attribute.String("service.version", "1.0.0"),
		),
	)
	if err != nil {
		return nil, fmt.Errorf("otel resource: %w", err)
	}

	var spanExporters []sdktrace.SpanExporter
	var metricReaders []sdkmetric.Reader

	filePath := os.Getenv("MMA_OTEL_FILE")
	if filePath == "" && isTruthy(os.Getenv("MMA_OTEL")) {
		filePath = "otel-spans.jsonl"
	}
	if filePath != "" {
		fe, err := NewFileExporter(filePath)
		if err != nil {
			return nil, fmt.Errorf("otel file exporter: %w", err)
		}
		spanExporters = append(spanExporters, fe)
		log.Printf("telemetry: writing spans to %s", filePath)
	}

	if os.Getenv("OTEL_EXPORTER_OTLP_ENDPOINT") != "" {
		texp, err := otlptracehttp.New(ctx, otlptracehttp.WithInsecure())
		if err != nil {
			log.Printf("telemetry: OTLP trace exporter failed (continuing): %v", err)
		} else {
			spanExporters = append(spanExporters, texp)
			log.Printf("telemetry: OTLP traces -> %s", os.Getenv("OTEL_EXPORTER_OTLP_ENDPOINT"))
		}
		// Jaeger all-in-one accepts OTLP traces, not metrics. Opt into OTLP
		// metrics only when a collector that implements /v1/metrics is in front.
		if isTruthy(os.Getenv("MMA_OTEL_METRICS")) {
			mexp, err := otlpmetrichttp.New(ctx, otlpmetrichttp.WithInsecure())
			if err != nil {
				log.Printf("telemetry: OTLP metric exporter failed (continuing): %v", err)
			} else {
				metricReaders = append(metricReaders, sdkmetric.NewPeriodicReader(mexp,
					sdkmetric.WithInterval(15*time.Second)))
			}
		}
	}

	if len(spanExporters) == 0 {
		return nil, fmt.Errorf("telemetry enabled but no exporter configured (set MMA_OTEL_FILE or OTEL_EXPORTER_OTLP_ENDPOINT)")
	}

	opts := []sdktrace.TracerProviderOption{
		sdktrace.WithResource(res),
		sdktrace.WithSampler(sdktrace.AlwaysSample()),
	}
	for _, exp := range spanExporters {
		opts = append(opts, sdktrace.WithSpanProcessor(sdktrace.NewBatchSpanProcessor(exp,
			sdktrace.WithMaxQueueSize(65536),
			sdktrace.WithMaxExportBatchSize(512),
			sdktrace.WithBatchTimeout(2*time.Second),
		)))
	}
	tp := sdktrace.NewTracerProvider(opts...)
	otel.SetTracerProvider(tp)

	mpOpts := []sdkmetric.Option{sdkmetric.WithResource(res)}
	mpOpts = append(mpOpts, metricReaderOpts(metricReaders)...)
	mp := sdkmetric.NewMeterProvider(mpOpts...)
	otel.SetMeterProvider(mp)
	initInstruments()

	shutdown := func(ctx context.Context) error {
		var first error
		if err := tp.ForceFlush(ctx); err != nil && first == nil {
			first = err
		}
		if err := tp.Shutdown(ctx); err != nil && first == nil {
			first = err
		}
		if err := mp.Shutdown(ctx); err != nil && first == nil {
			first = err
		}
		otel.SetTracerProvider(noop.NewTracerProvider())
		return first
	}
	return shutdown, nil
}

func metricReaderOpts(readers []sdkmetric.Reader) []sdkmetric.Option {
	out := make([]sdkmetric.Option, 0, len(readers))
	for _, r := range readers {
		out = append(out, sdkmetric.WithReader(r))
	}
	return out
}

// InitTest installs an in-memory SpanRecorder. It never contacts a collector.
// The returned cleanup restores a no-op provider.
func InitTest() (*tracetest.SpanRecorder, func()) {
	rec := tracetest.NewSpanRecorder()
	tp := sdktrace.NewTracerProvider(
		sdktrace.WithSampler(sdktrace.AlwaysSample()),
		sdktrace.WithSpanProcessor(rec),
	)
	otel.SetTracerProvider(tp)
	initInstruments()
	return rec, func() {
		_ = tp.Shutdown(context.Background())
		otel.SetTracerProvider(noop.NewTracerProvider())
		initInstruments()
	}
}

// Start starts a span and returns a wrapper whose End() also records the
// mma.etl.stage.duration histogram when etl.stage is set.
func Start(ctx context.Context, name string, attrs ...attribute.KeyValue) (context.Context, trace.Span) {
	ctx, span := otel.Tracer(TracerName).Start(ctx, name, trace.WithAttributes(attrs...))
	stage, unit := "", ""
	for _, a := range attrs {
		switch a.Key {
		case AttrStage:
			stage = a.Value.AsString()
		case AttrUnit:
			unit = a.Value.AsString()
		}
	}
	return ctx, &stageSpan{Span: span, start: time.Now(), stage: stage, unit: unit}
}

// StartExtract is the extract-stage span around one HTTP page fetch.
func StartExtract(ctx context.Context, rawURL string) (context.Context, trace.Span) {
	return Start(ctx, SpanExtract,
		attribute.String(AttrStage, StageExtract),
		attribute.String(AttrUnit, PageUnit(rawURL)),
		attribute.String("url", rawURL),
	)
}

// StartTransform is the transform-stage span around one parse/assemble.
func StartTransform(ctx context.Context, unit string, attrs ...attribute.KeyValue) (context.Context, trace.Span) {
	all := []attribute.KeyValue{
		attribute.String(AttrStage, StageTransform),
		attribute.String(AttrUnit, unit),
	}
	all = append(all, attrs...)
	return Start(ctx, SpanTransform, all...)
}

// StartLoad is the load-stage span around one SQLite write.
func StartLoad(ctx context.Context, unit string, attrs ...attribute.KeyValue) (context.Context, trace.Span) {
	all := []attribute.KeyValue{
		attribute.String(AttrStage, StageLoad),
		attribute.String(AttrUnit, unit),
	}
	all = append(all, attrs...)
	return Start(ctx, SpanLoad, all...)
}

// RecordError marks a span as failed. Safe on no-op spans.
func RecordError(span trace.Span, err error) {
	if err == nil || span == nil {
		return
	}
	span.RecordError(err)
	span.SetStatus(codes.Error, err.Error())
}

// AddRecords increments the records-processed counter.
func AddRecords(ctx context.Context, kind string, n int64) {
	if recordCount == nil || n == 0 {
		return
	}
	recordCount.Add(ctx, n, metric.WithAttributes(attribute.String("record.kind", kind)))
}

type stageSpan struct {
	trace.Span
	start time.Time
	stage string
	unit  string
}

func (s *stageSpan) End(options ...trace.SpanEndOption) {
	s.Span.End(options...)
	if s.stage == "" || stageDuration == nil {
		return
	}
	stageDuration.Record(context.Background(), time.Since(s.start).Seconds(),
		metric.WithAttributes(
			attribute.String(AttrStage, s.stage),
			attribute.String(AttrUnit, s.unit),
		),
	)
}

// EnsureDir is a tiny helper so callers can create the spans-file parent dir.
func EnsureDir(path string) error {
	dir := filepath.Dir(path)
	if dir == "" || dir == "." {
		return nil
	}
	return os.MkdirAll(dir, 0o755)
}

package fetch

import (
	"context"
	"net/http"
	"net/http/httptest"
	"testing"

	"ufcscraper/internal/telemetry"
)

func TestGetDocEmitsExtractSpan(t *testing.T) {
	rec, cleanup := telemetry.InitTest()
	defer cleanup()

	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write([]byte(`<html><body><p>ok</p></body></html>`))
	}))
	t.Cleanup(srv.Close)

	ctx, parent := telemetry.Start(context.Background(), telemetry.SpanRun)
	c := New(1000)
	doc, err := c.GetDoc(ctx, srv.URL+"/fighter-details/abc")
	if err != nil {
		t.Fatalf("GetDoc: %v", err)
	}
	if doc == nil {
		t.Fatal("nil doc")
	}
	parent.End()

	var extract bool
	var parentID, extractParent string
	for _, s := range rec.Ended() {
		if s.Name() == telemetry.SpanRun {
			parentID = s.SpanContext().SpanID().String()
		}
		if s.Name() == telemetry.SpanExtract {
			extract = true
			extractParent = s.Parent().SpanID().String()
			unit := ""
			for _, kv := range s.Attributes() {
				if string(kv.Key) == telemetry.AttrUnit {
					unit = kv.Value.AsString()
				}
			}
			if unit != "fighter" {
				t.Errorf("etl.unit=%q want fighter (url classified from path)", unit)
			}
		}
	}
	if !extract {
		t.Fatal("missing etl.extract span")
	}
	if parentID == "" || extractParent != parentID {
		t.Fatalf("extract parent=%s want run span=%s", extractParent, parentID)
	}
}

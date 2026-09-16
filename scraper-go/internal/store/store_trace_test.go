package store

import (
	"context"
	"testing"

	"ufcscraper/internal/model"
	"ufcscraper/internal/telemetry"
)

func TestUpsertFighterCtxEmitsLoadSpan(t *testing.T) {
	rec, cleanup := telemetry.InitTest()
	defer cleanup()

	s := openTemp(t)
	ctx, parent := telemetry.Start(context.Background(), telemetry.SpanFighter)
	if err := s.UpsertFighterCtx(ctx, model.Fighter{Name: "Jane Doe", Nationality: "Unlisted"}); err != nil {
		t.Fatalf("UpsertFighterCtx: %v", err)
	}
	parent.End()

	var load bool
	var parentID, loadParent string
	for _, sp := range rec.Ended() {
		if sp.Name() == telemetry.SpanFighter {
			parentID = sp.SpanContext().SpanID().String()
		}
		if sp.Name() == telemetry.SpanLoad {
			load = true
			loadParent = sp.Parent().SpanID().String()
			unit := ""
			for _, kv := range sp.Attributes() {
				if string(kv.Key) == telemetry.AttrUnit {
					unit = kv.Value.AsString()
				}
			}
			if unit != "fighter" {
				t.Errorf("etl.unit=%q want fighter", unit)
			}
		}
	}
	if !load {
		t.Fatal("missing etl.load span")
	}
	if parentID == "" || loadParent != parentID {
		t.Fatalf("load parent=%s want fighter span=%s", loadParent, parentID)
	}
}

func TestSaveEventCtxEmitsLoadSpan(t *testing.T) {
	rec, cleanup := telemetry.InitTest()
	defer cleanup()

	s := openTemp(t)
	if err := s.UpsertFighter(model.Fighter{Name: "A", Nationality: "Unlisted"}); err != nil {
		t.Fatal(err)
	}

	ev := model.Event{
		Title: "Test Event",
		Date:  "2020-01-01",
		Fights: []model.Fight{
			{
				WinnerName: "A",
				LoserName:  "B",
				Competitor1: model.Competitor{
					FighterName: "A",
					Rounds:      []model.RoundStat{{FighterName: "A", RoundNumber: 1}},
				},
				Competitor2: model.Competitor{
					FighterName: "B",
					Rounds:      []model.RoundStat{{FighterName: "B", RoundNumber: 1}},
				},
			},
		},
	}

	ctx, parent := telemetry.Start(context.Background(), telemetry.SpanEvent)
	if err := s.SaveEventCtx(ctx, ev); err != nil {
		t.Fatalf("SaveEventCtx: %v", err)
	}
	parent.End()

	var load bool
	var fights, rounds int
	var loadParent string
	var eventSpanID string
	for _, sp := range rec.Ended() {
		if sp.Name() == telemetry.SpanEvent {
			eventSpanID = sp.SpanContext().SpanID().String()
		}
		if sp.Name() != telemetry.SpanLoad {
			continue
		}
		unit := ""
		for _, kv := range sp.Attributes() {
			switch string(kv.Key) {
			case telemetry.AttrUnit:
				unit = kv.Value.AsString()
			case "etl.fight_count":
				fights = int(kv.Value.AsInt64())
			case "etl.round_count":
				rounds = int(kv.Value.AsInt64())
			}
		}
		if unit != "event" {
			continue
		}
		load = true
		loadParent = sp.Parent().SpanID().String()
	}
	if !load {
		t.Fatal("missing etl.load event span")
	}
	if fights != 1 || rounds != 2 {
		t.Errorf("fight_count=%d round_count=%d want 1/2", fights, rounds)
	}
	if eventSpanID == "" || loadParent != eventSpanID {
		t.Fatalf("load parent=%s want event span=%s", loadParent, eventSpanID)
	}
}

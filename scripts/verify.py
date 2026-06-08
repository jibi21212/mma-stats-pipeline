#!/usr/bin/env python3
"""Verify the scraped data in data/ufc.db.

Read-only health + integrity report: row counts, coverage, and structural
checks. Exit code 0 = all checks passed, 1 = something looks wrong.

Usage:
    python scripts/verify.py
    python scripts/verify.py --db path/to/ufc.db
"""
import argparse
import os
import sqlite3
import sys

DEFAULT_DB = os.path.join(os.path.dirname(__file__), "..", "data", "ufc.db")


def main() -> int:
    ap = argparse.ArgumentParser(description="Verify data/ufc.db health and integrity.")
    ap.add_argument("--db", default=DEFAULT_DB, help="Path to the SQLite database.")
    args = ap.parse_args()

    if not os.path.exists(args.db):
        print(f"FAIL: database not found at {os.path.abspath(args.db)}")
        print("      Run the scraper first:  cd scraper-go && go run .")
        return 1

    con = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True)
    q = lambda sql: con.execute(sql).fetchone()[0]
    checks = []

    def check(name, ok, detail=""):
        checks.append(ok)
        tag = "PASS" if ok else "FAIL"
        print(f"  [{tag}] {name}" + (f"  ({detail})" if detail else ""))

    print(f"database: {os.path.abspath(args.db)}\n")

    counts = {t: q(f"SELECT count(*) FROM {t}") for t in ("fighters", "events", "fights", "round_stats")}
    print("== row counts ==")
    for t, n in counts.items():
        print(f"  {t:13s} {n:>9,}")

    print("\n== coverage ==")
    print(f"  fighters with career stats : {q('SELECT count(*) FROM fighters WHERE slpm IS NOT NULL'):>9,}")
    print(f"  fighters with round data   : {q('SELECT count(DISTINCT fighter_name) FROM round_stats'):>9,}")
    print(f"  champions (title winners)  : {q('SELECT count(*) FROM fighters WHERE was_champion=1'):>9,}")
    print(f"  event date range           : {q('SELECT min(date) FROM events')} .. {q('SELECT max(date) FROM events')}")

    print("\n== integrity ==")
    check("all core tables non-empty", all(n > 0 for n in counts.values()))
    check("no orphan fights (event FK)",
          q("SELECT count(*) FROM fights f LEFT JOIN events e ON f.event_id=e.event_id WHERE e.event_id IS NULL") == 0)
    check("no orphan round_stats (fight FK)",
          q("SELECT count(*) FROM round_stats r LEFT JOIN fights f ON r.fight_id=f.fight_id WHERE f.fight_id IS NULL") == 0)
    check("no duplicate fighter names",
          q("SELECT count(*) FROM (SELECT name FROM fighters GROUP BY name HAVING count(*)>1)") == 0)
    check("no duplicate event titles",
          q("SELECT count(*) FROM (SELECT title FROM events GROUP BY title HAVING count(*)>1)") == 0)
    fights_wo_rounds = q("SELECT count(*) FROM fights f WHERE NOT EXISTS (SELECT 1 FROM round_stats r WHERE r.fight_id=f.fight_id)")
    check("at least 95% of fights have round_stats", fights_wo_rounds <= max(1, counts["fights"] * 0.05),
          f"{fights_wo_rounds} fights without round rows")
    check("percentages within 0..1",
          q("SELECT count(*) FROM round_stats WHERE sig_str_pct<0 OR sig_str_pct>1 OR td_pct<0 OR td_pct>1") == 0)
    check("round results in (w,l,d)",
          q("SELECT count(*) FROM round_stats WHERE result NOT IN ('w','l','d')") == 0)

    con.close()
    ok = all(checks)
    print("\n" + ("ALL CHECKS PASSED" if ok else "SOME CHECKS FAILED"))
    print("\nTo confirm the DB is CURRENT vs the live site, run an incremental pass:")
    print("  cd scraper-go && go run . --events-only")
    print("  (new=0 + 'reached already-stored ... stopping' means you have every listed event)")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())

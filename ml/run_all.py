"""End-to-end CLI for the UFC unsupervised-ML pipeline.

Runs, in order:
    1. :func:`db.build_fighter_features`  — load + engineer the feature matrix;
    2. :func:`archetypes.run_archetypes`  — clustering + plots;
    3. :func:`relationships.run_relationships` — correlations + association rules.

Creates the output directory and prints a short text report of what was
produced. The database is READ-ONLY; nothing here writes to ``data/ufc.db``.

Usage
-----
    cd ml
    python run_all.py                       # defaults: ../data/ufc.db, ./outputs
    python run_all.py --db /path/to/ufc.db --outdir ./outputs
    python run_all.py --min-fights 5        # stricter feature filtering
"""

from __future__ import annotations

import argparse
import os
import sys

from db import DEFAULT_DB_PATH, build_fighter_features


def _build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="run_all.py",
        description="Run the UFC unsupervised-ML pipeline "
                    "(archetype clustering + relationship mining).",
    )
    p.add_argument(
        "--db",
        default=DEFAULT_DB_PATH,
        help="Path to the SQLite database produced by the Go scraper "
             "(default: ../data/ufc.db relative to ml/).",
    )
    p.add_argument(
        "--outdir",
        default="./outputs",
        help="Directory for output CSVs and PNGs (default: ./outputs).",
    )
    p.add_argument(
        "--min-fights",
        type=int,
        default=None,
        help="Override the minimum recorded bouts a fighter needs to be kept.",
    )
    p.add_argument(
        "--k-min", type=int, default=2,
        help="Minimum k for the KMeans silhouette sweep (default: 2).",
    )
    p.add_argument(
        "--k-max", type=int, default=8,
        help="Maximum k for the KMeans silhouette sweep (default: 8).",
    )
    p.add_argument(
        "--k", type=int, default=None,
        help="Force the number of archetype clusters (overrides the silhouette "
             "auto-pick, which is unreliable when the curve is flat). E.g. --k 6.",
    )
    p.add_argument(
        "--all-fighters", action="store_true",
        help="Include fighters with no per-round data. Default: cluster only "
             "fighters who appear in round_stats, for cleaner style archetypes.",
    )
    return p


def main(argv=None) -> int:
    args = _build_parser().parse_args(argv)

    # Import the heavy stages lazily so `--help` works without the deps, and so
    # any missing-DB error surfaces with a clear message rather than a stack
    # trace on import.
    from archetypes import run_archetypes
    from relationships import run_relationships

    outdir = os.path.abspath(args.outdir)
    os.makedirs(outdir, exist_ok=True)

    print("=" * 70)
    print("UFC unsupervised-ML pipeline")
    print("=" * 70)
    print(f"  database : {os.path.abspath(args.db)}")
    print(f"  outdir   : {outdir}")
    print()

    # --- 1) Features ------------------------------------------------------- #
    try:
        fb_kwargs = {}
        if args.min_fights is not None:
            fb_kwargs["min_fights"] = args.min_fights
        if args.all_fighters:
            fb_kwargs["require_round_data"] = False
        features = build_fighter_features(db_path=args.db, **fb_kwargs)
    except FileNotFoundError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 2
    except ValueError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 3

    print(f"[1/3] Built feature matrix: {features.shape[0]} fighters x "
          f"{features.shape[1]} features.")

    # --- 2) Archetypes ----------------------------------------------------- #
    arche = run_archetypes(
        outdir=outdir, features=features, k_min=args.k_min, k_max=args.k_max, k=args.k
    )
    print(f"[2/3] Archetype clustering: k={arche['best_k']} clusters "
          f"({arche['n_pca_components']} PCA components, "
          f"{arche['explained_variance'] * 100:.1f}% variance retained).")
    if args.k is None and arche["best_k"] <= 2:
        print("      hint: silhouette is flat for fighter stats (styles are a "
              "continuum); try --k 6 for finer, more interpretable archetypes.")

    # --- 3) Relationships -------------------------------------------------- #
    rel = run_relationships(outdir=outdir, features=features)
    print(f"[3/3] Relationship mining: {rel['n_rules']} association rules "
          f"above thresholds.")

    # --- Report ------------------------------------------------------------ #
    all_files = list(arche["files"]) + list(rel["files"])
    print()
    print("-" * 70)
    print("Outputs written:")
    for path in all_files:
        rel_path = os.path.relpath(path, start=os.getcwd())
        print(f"  - {rel_path}")
    print("-" * 70)

    # A quick peek at the discovered archetypes.
    profiles = arche.get("profiles")
    if profiles is not None and "archetype_label" in profiles.columns:
        print("Suggested archetypes:")
        for cl, row in profiles.iterrows():
            print(f"  cluster {cl} (n={int(row['size'])}): "
                  f"{row['archetype_label']}")
    print()
    print("Done.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

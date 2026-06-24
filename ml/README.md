# UFC Unsupervised-ML Component (`ml/`)

The Python half of a two-part pipeline. A Go scraper (`scraper-go/`) writes the
SQLite database `data/ufc.db`; this component **reads it (read-only)** and runs
two unsupervised analyses over a per-fighter feature matrix:

- **(A) Fighter archetypes** — StandardScaler -> PCA -> KMeans (k by best
  silhouette) plus Agglomerative/Ward hierarchical clustering, with an
  auto-generated human-readable archetype label per cluster.
- **(B) Stat relationships** — Pearson & Spearman correlations plus
  association-rule mining (mlxtend apriori) over discretized features.

Everything is computed from one engineered feature matrix built in `db.py`;
results are written to `outputs/` as CSVs and PNGs. The database is never
modified.

---

## Prerequisites

- **Python 3.11** (tested; 3.10+ should work).
- **`data/ufc.db`** must exist — produced by the Go scraper. The modules
  *import* fine without it, but the loaders raise a clear `FileNotFoundError`
  when run against a missing DB, so run the scraper first.

## Install

**Option A - isolated virtualenv (recommended).** From `ml/`, run once:

```bash
./setup_env.ps1        # Windows (PowerShell)
bash setup_env.sh      # macOS / Linux
```

This creates `.venv/`, installs `requirements.txt` + `requirements-dev.txt`, and registers a Jupyter
kernel named **"UFC ML (.venv)"**. Activate it (`.\.venv\Scripts\Activate.ps1` or
`source .venv/bin/activate`) to run the scripts, or just pick that kernel in the notebook.

**Option B - install into your current environment:**

```bash
cd ml
pip install -r requirements.txt          # add -r requirements-dev.txt to run the tests
```

Core deps: pandas, numpy, scikit-learn, matplotlib, seaborn, mlxtend, jupyter.

`umap-learn` is **optional** (commented out in `requirements.txt`,
import-guarded in code). Install it only if you want an extra
`umap_scatter.png`:

```bash
pip install umap-learn
```

Plotting uses the headless `Agg` backend, so no display is required.

---

## Run

### CLI (`run_all.py`)

Builds the feature matrix once, runs both analyses, creates the output
directory, and prints a short report including the discovered archetypes.

```bash
cd ml
python run_all.py                       # defaults: --db ../data/ufc.db, --outdir ./outputs
```

| Flag | Default | Description |
|---|---|---|
| `--db PATH` | `../data/ufc.db` (relative to `ml/`) | Path to the scraper's SQLite database. |
| `--outdir DIR` | `./outputs` | Directory for the output CSVs and PNGs (created if missing). |
| `--min-fights N` | `3` | Minimum recorded bouts (wins+losses+draws+no_contests) a fighter needs to be kept. |
| `--k N` | `None` (auto) | Force the number of archetype clusters, overriding the silhouette auto-pick. |
| `--k-min N` | `2` | Minimum `k` for the KMeans silhouette sweep. |
| `--k-max N` | `8` | Maximum `k` for the KMeans silhouette sweep. |
| `--all-fighters` | off | Include fighters with no per-round data (default: only fighters who appear in `round_stats`, for cleaner style archetypes). |

**Recommended for archetypes:** `python run_all.py --min-fights 5 --k 6`

Fighter stats form a *continuum*, not cleanly separated blobs, so the silhouette curve is
near-flat and the auto-pick lands on a trivial `k=2`. For interpretable fighting-style archetypes
(high-volume strikers, rangy knockout artists, control-time wrestlers, submission hunters,
leg-kickers, …) pass an explicit `--k` (6 works well). By default the pipeline clusters only
fighters who have real per-round data; `--all-fighters` widens it (at the cost of imputing round
features for fighters who lack them).

Examples:

```bash
python run_all.py --min-fights 5 --k 6           # recommended: 6 style archetypes
python run_all.py --db /path/to/ufc.db --outdir ./outputs
python run_all.py --all-fighters --k 5           # include round-data-less fighters
python run_all.py --help                          # full usage (works without heavy deps installed)
```

Exit codes: `0` success; `2` database file not found; `3` no fighters passed
the quality thresholds (or empty `fighters` table).

### Notebook

Walks the same flow with inline charts. Run it **from the `ml/` directory** so
`import db / archetypes / relationships` resolve:

```bash
cd ml
jupyter notebook notebook.ipynb
```

The notebook's **first code cell** runs `%pip install -q -r requirements.txt`, installing its
dependencies into the running kernel so it works even on a bare Python (restart the kernel and re-run
if an import fails right after). For an isolated setup, run `setup_env.ps1` / `setup_env.sh` first and
choose the **"UFC ML (.venv)"** kernel - then you can skip that cell.

### As a library

```python
from db import build_fighter_features
from archetypes import run_archetypes
from relationships import run_relationships

features = build_fighter_features(db_path="../data/ufc.db")   # numeric DataFrame, indexed by fighter name
arche = run_archetypes(features=features, outdir="outputs")    # -> dict (best_k, profiles, files, ...)
rel   = run_relationships(features=features, outdir="outputs") # -> dict (n_rules, files, ...)
```

---

## What it produces

All artifacts are written to `outputs/` (or `--outdir`).

### Analysis A — archetype clustering

| File | Contents |
|---|---|
| `fighter_clusters.csv` | One row per fighter: `name`, `cluster` (KMeans), `agglo_cluster` (hierarchical cross-check), plus key stats `slpm, str_acc, td_avg, sub_avg, win_rate, finish_rate, avg_control_time`. |
| `cluster_profiles.csv` | One row per cluster: `size`, `archetype_label` (auto, e.g. `takedown-heavy grappler / control-time dominant`, from the cluster's highest z-scored deviations), and per-cluster feature means. |
| `pca_scatter.png` | 2-D PCA projection of fighters, coloured by cluster. |
| `dendrogram.png` | Ward hierarchical-clustering tree (truncated to 30 leaves when >40 fighters). |
| `silhouette.png` | Silhouette score and inertia (elbow) vs `k`, with the chosen `k` marked. |
| `umap_scatter.png` | *Optional* — only if `umap-learn` is installed. |

### Analysis B — relationship mining

| File | Contents |
|---|---|
| `correlations.csv` | Top correlated feature pairs ranked by absolute correlation, for **both** methods: columns `feature_a, feature_b, correlation, method (pearson/spearman), abs_correlation`. |
| `correlation_heatmap.png` | Pearson correlation heatmap over all features (incl. `win_rate`). |
| `association_rules.csv` | mlxtend rules over low/med/high-binned features, ranked by lift; columns `antecedents, consequents, support, confidence, lift` (+ `leverage`/`conviction` when available). Filtered to support >= 0.10, confidence >= 0.60, lift >= 1.10. **Header-only** if fewer than 20 fighters or no rules clear the thresholds. |

### The feature matrix (`db.build_fighter_features`)

Per-fighter, indexed by name, all numeric:

- **Career averages** (verbatim from `fighters`): `slpm, str_acc, sapm,
  str_def, td_avg, td_acc, td_def, sub_avg`.
- **Physical**: `height_in, reach_in, weight_lbs`.
- **Derived**: `win_rate` (= wins / total bouts), `finish_rate` (= fraction of
  a fighter's **wins** by KO/TKO or submission, i.e. method not `decision`).
- **Per-round aggregates** (joined from `round_stats` by `fighter_name`):
  `avg_sig_str_landed`, `head_share` / `body_share` / `leg_share` (target
  distribution, sum ~1), `avg_control_time` (seconds), `knockdown_rate`,
  `sub_attempt_rate`.

**Row filtering** (overridable): a fighter is dropped if `total_fights <
min_fights` (default 3), if more than 50% of the 8 career stats are NULL, or - by
default - if they have no per-round data (pass `--all-fighters` to keep that group).
**NULL handling**: `finish_rate` is filled with `0` (no wins = no finishes); the per-round
aggregates and everything else are **median-imputed** - so a fighter missing round data is treated
as "typical" rather than "zero", which prevents the clustering from collapsing onto a
has-data/no-data split.

**Value conventions** (per `docs/SCHEMA_CONTRACT.md`): percentages are 0..1
fractions, heights/reach in inches, weight in lbs, control time in seconds.

---

## Fight-outcome predictor (`predict.py`)

A **supervised** add-on alongside the unsupervised analyses: it predicts
`P(fighter A beats fighter B)` from a leakage-safe, weight-class-gated model
covering **age**, **activity** (layoff / recent frequency), **career
trajectory** (Elo momentum / form), and **skill** (Elo).

```bash
cd ml
python predict.py --train                 # train both models, save the best, print metrics
python predict.py --a "Israel Adesanya" --b "Robert Whittaker"   # one prediction
```

The trained model is written to `ml/models/predictor.joblib` (gitignored —
regenerate with `--train`). The TUI's prediction sidecar (`serve.py`) loads this
file.

### Three correctness invariants

1. **No temporal leakage.** Every feature for a historical fight is computed
   from each fighter's **prior fights only** (strictly before that fight's
   date), by walking all fights chronologically and snapshotting each fighter's
   running state *before* applying the fight's result. The post-hoc career
   aggregates in the `fighters` table (`slpm, str_acc, sapm, str_def, td_avg,
   td_acc, td_def, sub_avg`) are **never** used — they are computed over each
   fighter's whole career (incl. this and future fights) and would leak the
   label. Only **static** physical attributes (`reach_in, height_in, stance,
   date_of_birth`) are read from `fighters`. The train/test split is
   **temporal** (train `< 2023-01-01`, test on/after), never random. Fights with
   no real winner (method containing draw / NC / overturn) are dropped from the
   label set.
2. **Symmetry.** Features are **signed differences** (`A − B`); each training
   fight is emitted in **both** orderings (winner-as-A label 1, loser-as-A label
   0). At predict time both orderings are run and averaged, so
   `P(A beats B) == 1 − P(B beats A)` exactly.
3. **Weight-class gating.** Each `weight_class` string is normalised to a
   division on one of two **separate** ordinal ladders (men 1–8, women 1–4). A
   matchup is allowed only if both fighters share a gender ladder **and** the
   minimum division distance over the divisions they've actually fought in is
   `≤ 1`; otherwise it is **refused** (e.g. a Bantam/Featherweight vs a
   Heavyweight is ~5–6 steps apart → refused). Cross-gender → refused. A fighter
   with no resolvable division (only catch/open weight) → allowed but flagged
   low confidence.

### Features (all leakage-safe, as-of fight date, used as A − B differences)

`elo_pre` (running Elo, start 1500, K=32), `elo_momentum` (Elo now minus Elo ~5
fights ago), `age_years`, `n_prior_fights`, `winrate_prior`, `recent_winrate`
(last 5), `form_delta` (recent − career), `days_since_last` (layoff;
`is_debut` flag + 365-day sentinel for debuts), `fights_last_365` (activity),
`reach_in`, `height_in`, `southpaw`. Plus two symmetric fight-context features:
`stance_mismatch` (orthodox-vs-southpaw) and `title_bout`. (As-of-date
striking/grappling rates from `round_stats` are a noted future enhancement —
skipped to keep the leakage proof simple.)

### Models & evaluation

Two models are trained on the symmetric, leakage-safe difference matrix (NaNs
median-imputed): **LogisticRegression** (in a `StandardScaler` pipeline) and
**HistGradientBoostingClassifier**. The better model **by test log-loss** is
persisted. On the temporal hold-out the predictor reports accuracy, log-loss and
Brier score, alongside three baselines (pick higher pre-fight Elo, pick more
experienced, base rate). Honest MMA models land **~60–65%**; test accuracy
`> ~70%` is a **red flag for leakage** (the trainer prints a warning if it sees
that). The saved payload bundles the fitted pipeline, the feature column order,
each fighter's **current** snapshot (as of `max(fights.date)`, not the wall
clock), their division set, and the metrics.

```bash
cd ml && python -m pytest tests/test_predict.py -q   # normaliser, gating, symmetry, leakage, Elo
```

---

## Interpreting the charts

**`pca_scatter.png` - the archetype map.** Each dot is a fighter. PCA compresses the 20 features
onto two axes - PC1 (~17% of the variance) and PC2 (~13%) - chosen to spread fighters out as much as
possible in 2-D; colour is the cluster (archetype) each fighter was assigned. Dots close together are
statistically similar fighters. Only ~30% of the variance fits in 2-D (the clustering itself uses 16
components / ~96%), so expect overlapping coloured *regions* rather than crisp islands - the
separated blob on one side is the most distinct group; the central colours blend because fighting
styles are a continuum.

**`correlation_heatmap.png` - which stats move together.** A 20x20 grid of every feature against
every other. Red = positive correlation (rise together), blue = negative (one up, the other down),
near-white = no linear link; the red diagonal is each feature with itself (=1). Scan for coloured
blocks: `height_in / reach_in / weight_lbs` form a red block (bigger fighters are taller, longer,
heavier); `slpm` ~ `avg_sig_str_landed` is red (volume strikers); `head_share` vs
`body_share`/`leg_share` is blue (fighters specialize to the head *or* to the body/legs). These are
the relationships the model surfaced without being told what to look for.

**`silhouette.png` - how many archetypes?** Blue line = silhouette score (cluster separation; higher
is better) at each candidate k; red dashed = inertia (the "elbow"); the green line marks the k used.
Here the silhouette is low and nearly flat (~0.10 at every k) - direct evidence that fighters do NOT
fall into a few clean groups, so k is a readability choice (6), not a number the data forces.

**`dendrogram.png` - the nested family tree.** Hierarchical (Ward) clustering merges fighters
bottom-up by similarity. Height = how different two groups are when they merge; the numbers in
parentheses are how many fighters sit under each branch (the tree is truncated to its top ~30
branches). A horizontal cut gives that many groups - cut low for many small archetypes, high for a
few broad ones. It's an alternative view of the same structure KMeans captures as a flat k=6.

---

## Files

| File | Role |
|---|---|
| `db.py` | Read-only SQLite access + feature engineering (`build_fighter_features`, `load_*`, `get_connection`). |
| `archetypes.py` | Analysis A: scaling, PCA, KMeans/Agglomerative clustering, plots (`run_archetypes`). |
| `relationships.py` | Analysis B: correlations + association-rule mining (`run_relationships`). |
| `predict.py` | Supervised fight-outcome predictor: leakage-safe features, Elo, weight-class gating, symmetric `predict(a, b)` + `--train` CLI. |
| `models/` | Trained predictor (`predictor.joblib`), gitignored — regenerate with `python predict.py --train`. |
| `run_all.py` | Argparse CLI that wires the three stages together. |
| `notebook.ipynb` | Interactive walkthrough of the same pipeline. |
| `requirements.txt` | Python dependencies. |
| `outputs/` | Generated CSVs and PNGs (created at runtime). |

---

## Pipeline fit

`scraper-go/` (writer) -> `data/ufc.db` (shared SQLite, contract in
`docs/SCHEMA_CONTRACT.md`) -> **`ml/` (this read-only consumer)** -> archetype +
relationship artifacts in `outputs/`.

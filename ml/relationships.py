"""Stat RELATIONSHIP mining (unsupervised).

Two complementary views over the per-fighter feature matrix from :mod:`db`:

1. **Correlation analysis** — Pearson (linear) AND Spearman (monotonic)
   correlation matrices over the engineered features plus ``win_rate``.
   Saves ``correlation_heatmap.png`` and ``correlations.csv`` (the top
   correlated feature pairs by absolute correlation).

2. **Association-rule mining** — discretize each continuous feature into
   low / med / high bins (``pandas.qcut`` with duplicate-edge handling) plus a
   win-rate bin, one-hot encode, run ``mlxtend`` ``apriori`` then
   ``association_rules``. Saves ``association_rules.csv`` ranked by lift and
   filtered to meaningful support / confidence.

Headless: forces the matplotlib ``Agg`` backend. Importing this module is
always safe (no DB / no plotting at import time).
"""

from __future__ import annotations

import os
from typing import Optional

import matplotlib

matplotlib.use("Agg")  # headless backend — set BEFORE pyplot import.

import matplotlib.pyplot as plt  # noqa: E402
import numpy as np  # noqa: E402
import pandas as pd  # noqa: E402
import seaborn as sns  # noqa: E402

from db import DEFAULT_DB_PATH, build_fighter_features  # noqa: E402

# Filtering defaults for association rules (documented; overridable).
MIN_SUPPORT = 0.10        # itemset must appear in >=10% of fighters.
MIN_CONFIDENCE = 0.70     # rule confidence floor.
MIN_LIFT = 1.25           # keep rules clearly better than chance.
MIN_ROWS_FOR_RULES = 20   # below this, rule mining is unreliable -> skip.
MAX_ITEMSET_LEN = 3       # cap apriori itemset size to curb combinatorial blow-up.
MAX_RULES = 500           # keep only the strongest rules (by lift) in the output.


def _ensure_outdir(outdir: str) -> None:
    os.makedirs(outdir, exist_ok=True)


# --------------------------------------------------------------------------- #
# Correlation analysis
# --------------------------------------------------------------------------- #

def correlation_matrices(features: pd.DataFrame):
    """Return ``(pearson_df, spearman_df)`` over the numeric features."""
    numeric = features.select_dtypes(include=[np.number])
    pearson = numeric.corr(method="pearson")
    spearman = numeric.corr(method="spearman")
    return pearson, spearman


def top_correlations(corr: pd.DataFrame, method_name: str, top_n: int = 25) -> pd.DataFrame:
    """Flatten the upper triangle of a corr matrix into ranked unique pairs."""
    cols = corr.columns
    records = []
    for i in range(len(cols)):
        for j in range(i + 1, len(cols)):
            val = corr.iloc[i, j]
            if pd.isna(val):
                continue
            records.append((cols[i], cols[j], float(val)))
    out = pd.DataFrame(records, columns=["feature_a", "feature_b", "correlation"])
    out["method"] = method_name
    out["abs_correlation"] = out["correlation"].abs()
    out = out.sort_values("abs_correlation", ascending=False).head(top_n)
    return out.reset_index(drop=True)


def plot_correlation_heatmap(pearson: pd.DataFrame, path: str) -> None:
    fig, ax = plt.subplots(figsize=(12, 10))
    sns.heatmap(
        pearson,
        annot=False,
        cmap="coolwarm",
        center=0,
        vmin=-1,
        vmax=1,
        square=True,
        linewidths=0.5,
        cbar_kws={"shrink": 0.8, "label": "Pearson r"},
        ax=ax,
    )
    ax.set_title("Feature correlation heatmap (Pearson)")
    fig.tight_layout()
    fig.savefig(path, dpi=120)
    plt.close(fig)


# --------------------------------------------------------------------------- #
# Association-rule mining
# --------------------------------------------------------------------------- #

def discretize_features(features: pd.DataFrame, n_bins: int = 3) -> pd.DataFrame:
    """Bin each continuous feature into low/med/high via ``qcut``.

    Uses ``duplicates="drop"`` so columns with many tied values (e.g. zero-heavy
    shares) don't blow up on duplicate bin edges. Columns that collapse to a
    single bin (no variance) are skipped. Returns a DataFrame of categorical
    string labels like ``"slpm=high"``.
    """
    labels3 = ["low", "med", "high"]
    binned = {}
    for col in features.columns:
        series = features[col]
        if series.nunique(dropna=True) < 2:
            continue  # constant column carries no association signal.
        try:
            cats = pd.qcut(series, q=n_bins, labels=labels3, duplicates="drop")
        except (ValueError, IndexError):
            # qcut can still fail if too few distinct quantiles remain; fall
            # back to a rank-based 3-way split.
            try:
                cats = pd.qcut(series.rank(method="first"), q=n_bins,
                               labels=labels3, duplicates="drop")
            except (ValueError, IndexError):
                continue
        # When duplicates="drop" reduced the number of bins, labels may be
        # fewer than requested; relabel positionally to stay readable.
        if cats.dtype.name == "category" and len(cats.cat.categories) < n_bins:
            n = len(cats.cat.categories)
            cats = cats.cat.rename_categories(labels3[:n])
        binned[col] = cats.astype(str)
    return pd.DataFrame(binned, index=features.index)


def _one_hot(binned: pd.DataFrame) -> pd.DataFrame:
    """One-hot encode ``col=label`` into a boolean transaction matrix."""
    frames = []
    for col in binned.columns:
        dummies = pd.get_dummies(binned[col], prefix=col, prefix_sep="=")
        frames.append(dummies)
    if not frames:
        return pd.DataFrame(index=binned.index)
    onehot = pd.concat(frames, axis=1)
    # mlxtend prefers boolean.
    return onehot.astype(bool)


def mine_association_rules(
    features: pd.DataFrame,
    min_support: float = MIN_SUPPORT,
    min_confidence: float = MIN_CONFIDENCE,
    min_lift: float = MIN_LIFT,
    n_bins: int = 3,
) -> pd.DataFrame:
    """Discretize -> one-hot -> apriori -> association_rules, ranked by lift.

    Returns an empty (correctly-columned) DataFrame when there are too few rows
    or no rules clear the thresholds, so callers never crash on sparse data.
    """
    empty = pd.DataFrame(
        columns=[
            "antecedents",
            "consequents",
            "support",
            "confidence",
            "lift",
        ]
    )
    if features.shape[0] < MIN_ROWS_FOR_RULES:
        return empty

    # Imported here (not at module top) so the module imports without mlxtend.
    try:
        from mlxtend.frequent_patterns import apriori, association_rules
    except Exception:  # noqa: BLE001
        return empty

    binned = discretize_features(features, n_bins=n_bins)
    onehot = _one_hot(binned)
    if onehot.shape[1] == 0:
        return empty

    frequent = apriori(onehot, min_support=min_support, use_colnames=True, max_len=MAX_ITEMSET_LEN)
    if frequent.empty:
        return empty

    try:
        rules = association_rules(
            frequent, metric="confidence", min_threshold=min_confidence
        )
    except (ValueError, KeyError):
        return empty
    if rules.empty:
        return empty

    rules = rules[rules["lift"] >= min_lift]
    if rules.empty:
        return empty

    # Render frozensets as readable comma-joined strings.
    rules = rules.copy()
    rules["antecedents"] = rules["antecedents"].apply(
        lambda s: ", ".join(sorted(s))
    )
    rules["consequents"] = rules["consequents"].apply(
        lambda s: ", ".join(sorted(s))
    )
    keep_cols = [
        "antecedents",
        "consequents",
        "support",
        "confidence",
        "lift",
    ]
    for extra in ("leverage", "conviction"):
        if extra in rules.columns:
            keep_cols.append(extra)
    rules = rules[keep_cols].sort_values("lift", ascending=False).head(MAX_RULES)
    return rules.reset_index(drop=True)


# --------------------------------------------------------------------------- #
# Orchestration
# --------------------------------------------------------------------------- #

def run_relationships(
    db_path: Optional[str] = None,
    outdir: str = "outputs",
    features: Optional[pd.DataFrame] = None,
    top_n: int = 25,
) -> dict:
    """Run correlation + association-rule mining and write all artifacts.

    Returns
    -------
    dict
        ``n_fighters``, ``n_rules``, ``files`` (written paths), and the
        in-memory ``top_correlations`` / ``rules`` DataFrames.
    """
    _ensure_outdir(outdir)
    if features is None:
        features = build_fighter_features(db_path=db_path)

    # --- correlations ------------------------------------------------------ #
    pearson, spearman = correlation_matrices(features)
    top_pearson = top_correlations(pearson, "pearson", top_n=top_n)
    top_spearman = top_correlations(spearman, "spearman", top_n=top_n)
    top_corr = pd.concat([top_pearson, top_spearman], ignore_index=True)

    corr_csv = os.path.join(outdir, "correlations.csv")
    top_corr.to_csv(corr_csv, index=False)

    heatmap_path = os.path.join(outdir, "correlation_heatmap.png")
    plot_correlation_heatmap(pearson, heatmap_path)

    # --- association rules ------------------------------------------------- #
    rules = mine_association_rules(features)
    rules_csv = os.path.join(outdir, "association_rules.csv")
    rules.to_csv(rules_csv, index=False)

    files = [corr_csv, heatmap_path, rules_csv]
    return {
        "n_fighters": int(features.shape[0]),
        "n_rules": int(rules.shape[0]),
        "files": files,
        "top_correlations": top_corr,
        "rules": rules,
    }


if __name__ == "__main__":  # pragma: no cover - manual smoke check
    result = run_relationships(db_path=DEFAULT_DB_PATH, outdir="outputs")
    print(f"Analyzed {result['n_fighters']} fighters; "
          f"found {result['n_rules']} association rules.")
    for f in result["files"]:
        print("  wrote", f)

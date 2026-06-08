"""Fighter ARCHETYPE clustering (unsupervised).

Pipeline
--------
1. ``StandardScaler`` the per-fighter feature matrix from :mod:`db`.
2. ``PCA`` retaining ~95% variance (for clustering) plus a separate
   2-component projection (for plotting).
3. ``KMeans`` with ``k`` chosen by the best silhouette score across a small
   range; the inertia (elbow) curve is also emitted.
4. ``AgglomerativeClustering`` + a SciPy linkage dendrogram for a
   hierarchical view.

Outputs (written to ``--outdir`` / ``outputs/``)
------------------------------------------------
  * ``fighter_clusters.csv``  — name, cluster, a few key stats.
  * ``cluster_profiles.csv``  — per-cluster feature means + a suggested
    human-readable archetype label derived from each cluster's
    highest-deviation stats.
  * ``pca_scatter.png``       — 2-D PCA projection coloured by cluster.
  * ``dendrogram.png``        — hierarchical clustering dendrogram.
  * ``silhouette.png``        — silhouette-score AND inertia (elbow) vs k.

Headless: forces the matplotlib ``Agg`` backend so everything runs without a
display. Importing this module is always safe (no DB / no plotting at import).
"""

from __future__ import annotations

import os
from typing import Optional

import matplotlib

matplotlib.use("Agg")  # headless backend — set BEFORE pyplot import.

import matplotlib.pyplot as plt  # noqa: E402
import numpy as np  # noqa: E402
import pandas as pd  # noqa: E402
from scipy.cluster.hierarchy import dendrogram, linkage  # noqa: E402
from sklearn.cluster import AgglomerativeClustering, KMeans  # noqa: E402
from sklearn.decomposition import PCA  # noqa: E402
from sklearn.metrics import silhouette_score  # noqa: E402
from sklearn.preprocessing import StandardScaler  # noqa: E402

from db import DEFAULT_DB_PATH, build_fighter_features  # noqa: E402

# A few human-friendly stats surfaced in fighter_clusters.csv.
_KEY_STATS = [
    "slpm",
    "str_acc",
    "td_avg",
    "sub_avg",
    "win_rate",
    "finish_rate",
    "avg_control_time",
]

# Optional UMAP — import-guarded so it is NEVER a hard dependency.
try:  # pragma: no cover - optional dependency
    import umap  # type: ignore

    _HAVE_UMAP = True
except Exception:  # noqa: BLE001 - any import failure means "not available"
    _HAVE_UMAP = False


def _ensure_outdir(outdir: str) -> None:
    os.makedirs(outdir, exist_ok=True)


def scale_and_reduce(features: pd.DataFrame, variance: float = 0.95):
    """Standardize then PCA-reduce.

    Returns
    -------
    (X_scaled, X_pca, X_pca2, scaler, pca_full, pca2)
        ``X_pca`` retains ``variance`` of total variance (used for
        clustering); ``X_pca2`` is the 2-component projection (used for
        plotting).
    """
    scaler = StandardScaler()
    X_scaled = scaler.fit_transform(features.values)

    # PCA retaining the requested cumulative variance. n_components as a float
    # in (0,1) selects the smallest number of components reaching that ratio.
    n_samples, n_feats = X_scaled.shape
    max_comp = max(1, min(n_samples, n_feats))
    pca_full = PCA(n_components=min(variance, 0.999), svd_solver="full")
    X_pca = pca_full.fit_transform(X_scaled)

    # Separate 2-D projection for plotting (independent fit, clamped to data).
    pca2 = PCA(n_components=min(2, max_comp))
    X_pca2 = pca2.fit_transform(X_scaled)
    if X_pca2.shape[1] == 1:  # degenerate (single feature) — pad a zero column.
        X_pca2 = np.column_stack([X_pca2[:, 0], np.zeros(len(X_pca2))])

    return X_scaled, X_pca, X_pca2, scaler, pca_full, pca2


def choose_k(X: np.ndarray, k_min: int = 2, k_max: int = 8):
    """Pick ``k`` for KMeans by best silhouette across ``[k_min, k_max]``.

    Returns
    -------
    (best_k, ks, silhouettes, inertias)
        ``ks`` is the list of evaluated k values; ``silhouettes`` and
        ``inertias`` are aligned lists (the elbow curve uses ``inertias``).
    """
    n_samples = X.shape[0]
    # Silhouette needs at least 2 clusters and k < n_samples.
    k_hi = min(k_max, max(k_min, n_samples - 1))
    ks = list(range(k_min, k_hi + 1)) if k_hi >= k_min else [min(2, n_samples)]

    silhouettes: list = []
    inertias: list = []
    for k in ks:
        km = KMeans(n_clusters=k, n_init=10, random_state=42)
        labels = km.fit_predict(X)
        inertias.append(float(km.inertia_))
        # Silhouette is undefined for a single populated cluster.
        if len(set(labels)) > 1:
            silhouettes.append(float(silhouette_score(X, labels)))
        else:
            silhouettes.append(float("nan"))

    valid = [(k, s) for k, s in zip(ks, silhouettes) if not np.isnan(s)]
    best_k = max(valid, key=lambda t: t[1])[0] if valid else ks[0]
    return best_k, ks, silhouettes, inertias


def _archetype_label(profile_row: pd.Series, z_row: pd.Series, top_n: int = 2) -> str:
    """Build a readable archetype label from a cluster's highest-deviation stats.

    ``z_row`` is the cluster's per-feature mean expressed as a z-score relative
    to the across-cluster distribution. We take the ``top_n`` features whose
    magnitude is largest and render direction-aware phrases.
    """
    # Friendly phrasing for "high" vs "low" on each feature.
    phrases = {
        "slpm": ("high-volume striker", "low-output striker"),
        "sapm": ("gets hit a lot", "hard to hit"),
        "str_acc": ("accurate striker", "inaccurate striker"),
        "str_def": ("strong striking defense", "leaky striking defense"),
        "td_avg": ("takedown-heavy grappler", "rarely shoots takedowns"),
        "td_acc": ("efficient takedowns", "inefficient takedowns"),
        "td_def": ("strong takedown defense", "weak takedown defense"),
        "sub_avg": ("submission hunter", "rarely submits"),
        "win_rate": ("high win rate", "low win rate"),
        "finish_rate": ("finisher", "decision-prone"),
        "avg_sig_str_landed": ("heavy significant-strike output", "light strike output"),
        "head_share": ("head-hunter", "avoids the head"),
        "body_share": ("body-work specialist", "ignores the body"),
        "leg_share": ("leg-kick specialist", "ignores the legs"),
        "avg_control_time": ("control-time dominant", "little control time"),
        "knockdown_rate": ("knockdown threat", "few knockdowns"),
        "sub_attempt_rate": ("active submission attempts", "few submission attempts"),
        "height_in": ("tall", "short"),
        "reach_in": ("rangy", "short reach"),
        "weight_lbs": ("heavier", "lighter"),
    }
    ranked = z_row.reindex(z_row.abs().sort_values(ascending=False).index)
    parts = []
    for feat, z in ranked.head(top_n).items():
        if np.isclose(z, 0.0):
            continue
        hi, lo = phrases.get(feat, (f"high {feat}", f"low {feat}"))
        parts.append(hi if z > 0 else lo)
    return " / ".join(parts) if parts else "balanced generalist"


def build_cluster_profiles(features: pd.DataFrame, labels: np.ndarray) -> pd.DataFrame:
    """Per-cluster feature means + size + a suggested archetype label.

    The label is derived from each cluster's highest-deviation features
    (z-scored across clusters).
    """
    df = features.copy()
    df["cluster"] = labels
    means = df.groupby("cluster").mean(numeric_only=True)
    sizes = df.groupby("cluster").size().rename("size")

    # z-score each cluster-mean column across clusters to find the standout
    # (highest-deviation) features per cluster.
    col_mean = means.mean(axis=0)
    col_std = means.std(axis=0, ddof=0).replace(0, np.nan)
    z = (means - col_mean) / col_std
    z = z.fillna(0.0)

    labels_out = {
        cl: _archetype_label(means.loc[cl], z.loc[cl]) for cl in means.index
    }

    profiles = means.copy()
    profiles.insert(0, "size", sizes)
    profiles.insert(1, "archetype_label", pd.Series(labels_out))
    profiles.index.name = "cluster"
    return profiles


# --------------------------------------------------------------------------- #
# Plotting
# --------------------------------------------------------------------------- #

def plot_pca_scatter(X_pca2: np.ndarray, labels: np.ndarray, path: str,
                     pca2_obj: Optional[PCA] = None) -> None:
    fig, ax = plt.subplots(figsize=(8, 6))
    scatter = ax.scatter(
        X_pca2[:, 0], X_pca2[:, 1], c=labels, cmap="tab10", s=28, alpha=0.85
    )
    if pca2_obj is not None and hasattr(pca2_obj, "explained_variance_ratio_"):
        evr = pca2_obj.explained_variance_ratio_
        ax.set_xlabel(f"PC1 ({evr[0] * 100:.1f}% var)")
        if len(evr) > 1:
            ax.set_ylabel(f"PC2 ({evr[1] * 100:.1f}% var)")
        else:
            ax.set_ylabel("PC2")
    else:
        ax.set_xlabel("PC1")
        ax.set_ylabel("PC2")
    ax.set_title("Fighter archetypes — PCA projection (coloured by cluster)")
    legend = ax.legend(*scatter.legend_elements(), title="cluster",
                       loc="best", fontsize=8)
    ax.add_artist(legend)
    fig.tight_layout()
    fig.savefig(path, dpi=120)
    plt.close(fig)


def plot_dendrogram(X: np.ndarray, path: str, names: Optional[list] = None) -> None:
    Z = linkage(X, method="ward")
    fig, ax = plt.subplots(figsize=(11, 6))
    # For readable axes, cap the number of leaves shown via truncation when big.
    n = X.shape[0]
    if n > 40:
        dendrogram(Z, ax=ax, truncate_mode="lastp", p=30,
                   show_leaf_counts=True, no_labels=False)
        ax.set_xlabel("cluster size / merged leaves (truncated)")
    else:
        labels = names if names is not None else None
        dendrogram(Z, ax=ax, labels=labels, leaf_rotation=90, leaf_font_size=7)
        ax.set_xlabel("fighter")
    ax.set_title("Hierarchical clustering dendrogram (Ward linkage)")
    ax.set_ylabel("distance")
    fig.tight_layout()
    fig.savefig(path, dpi=120)
    plt.close(fig)


def plot_silhouette_and_elbow(ks, silhouettes, inertias, best_k, path: str) -> None:
    """Combined silhouette (left axis) + inertia/elbow (right axis) vs k."""
    fig, ax1 = plt.subplots(figsize=(8, 5))
    color1 = "tab:blue"
    ax1.set_xlabel("k (number of clusters)")
    ax1.set_ylabel("silhouette score", color=color1)
    ax1.plot(ks, silhouettes, "o-", color=color1, label="silhouette")
    ax1.tick_params(axis="y", labelcolor=color1)
    ax1.axvline(best_k, color="green", linestyle="--", alpha=0.7,
                label=f"chosen k={best_k}")

    ax2 = ax1.twinx()
    color2 = "tab:red"
    ax2.set_ylabel("inertia (elbow)", color=color2)
    ax2.plot(ks, inertias, "s--", color=color2, alpha=0.7, label="inertia")
    ax2.tick_params(axis="y", labelcolor=color2)

    ax1.set_title("Choosing k: silhouette score and inertia (elbow)")
    ax1.set_xticks(list(ks))
    fig.tight_layout()
    fig.savefig(path, dpi=120)
    plt.close(fig)


def maybe_plot_umap(X_scaled: np.ndarray, labels: np.ndarray, outdir: str) -> Optional[str]:
    """If umap-learn is installed, also save a UMAP 2-D scatter. Optional."""
    if not _HAVE_UMAP:
        return None
    try:  # pragma: no cover - only runs when umap is installed
        reducer = umap.UMAP(n_components=2, random_state=42)
        emb = reducer.fit_transform(X_scaled)
        path = os.path.join(outdir, "umap_scatter.png")
        fig, ax = plt.subplots(figsize=(8, 6))
        sc = ax.scatter(emb[:, 0], emb[:, 1], c=labels, cmap="tab10",
                        s=28, alpha=0.85)
        ax.set_title("Fighter archetypes — UMAP projection")
        ax.set_xlabel("UMAP-1")
        ax.set_ylabel("UMAP-2")
        ax.add_artist(ax.legend(*sc.legend_elements(), title="cluster"))
        fig.tight_layout()
        fig.savefig(path, dpi=120)
        plt.close(fig)
        return path
    except Exception:  # noqa: BLE001
        return None


# --------------------------------------------------------------------------- #
# Orchestration
# --------------------------------------------------------------------------- #

def run_archetypes(
    db_path: Optional[str] = None,
    outdir: str = "outputs",
    features: Optional[pd.DataFrame] = None,
    k_min: int = 2,
    k_max: int = 8,
    variance: float = 0.95,
    k: Optional[int] = None,
) -> dict:
    """Run the full archetype-clustering pipeline and write all artifacts.

    Parameters
    ----------
    features:
        Optional pre-built feature matrix (e.g. from a notebook). If omitted it
        is built from the database at ``db_path``.

    Returns
    -------
    dict
        Summary with ``best_k``, ``n_fighters``, ``n_pca_components``,
        ``files`` (list of written paths) and ``profiles`` (DataFrame).
    """
    _ensure_outdir(outdir)
    if features is None:
        features = build_fighter_features(db_path=db_path)

    if features.shape[0] < 2:
        raise ValueError(
            f"Need at least 2 fighters to cluster; got {features.shape[0]}."
        )

    X_scaled, X_pca, X_pca2, scaler, pca_full, pca2 = scale_and_reduce(
        features, variance=variance
    )

    best_k, ks, silhouettes, inertias = choose_k(X_pca, k_min=k_min, k_max=k_max)
    # The silhouette curve for fighter stats is typically flat (styles form a
    # continuum, not separated blobs), so the auto-pick can land on a trivial
    # k=2. An explicit k overrides it for interpretable archetype granularity;
    # the silhouette/elbow curve is still emitted so the choice stays informed.
    if k is not None and k >= 2:
        best_k = k

    km = KMeans(n_clusters=best_k, n_init=10, random_state=42)
    labels = km.fit_predict(X_pca)

    # Hierarchical labels (same k) for completeness / agreement inspection.
    agglo = AgglomerativeClustering(n_clusters=best_k, linkage="ward")
    agglo_labels = agglo.fit_predict(X_scaled)

    # --- fighter_clusters.csv ---------------------------------------------- #
    key_stats = [c for c in _KEY_STATS if c in features.columns]
    clusters_df = pd.DataFrame(index=features.index)
    clusters_df.insert(0, "cluster", labels)
    clusters_df["agglo_cluster"] = agglo_labels
    for c in key_stats:
        clusters_df[c] = features[c].values
    # The feature index is the fighter name; surface it as a "name" column.
    clusters_df = clusters_df.reset_index()
    clusters_df = clusters_df.rename(
        columns={clusters_df.columns[0]: "name"}
    )
    clusters_path = os.path.join(outdir, "fighter_clusters.csv")
    clusters_df.to_csv(clusters_path, index=False)

    # --- cluster_profiles.csv ---------------------------------------------- #
    profiles = build_cluster_profiles(features, labels)
    profiles_path = os.path.join(outdir, "cluster_profiles.csv")
    profiles.to_csv(profiles_path)

    # --- plots ------------------------------------------------------------- #
    pca_path = os.path.join(outdir, "pca_scatter.png")
    plot_pca_scatter(X_pca2, labels, pca_path, pca2_obj=pca2)

    dendro_path = os.path.join(outdir, "dendrogram.png")
    plot_dendrogram(X_scaled, dendro_path, names=list(features.index))

    sil_path = os.path.join(outdir, "silhouette.png")
    plot_silhouette_and_elbow(ks, silhouettes, inertias, best_k, sil_path)

    files = [clusters_path, profiles_path, pca_path, dendro_path, sil_path]
    umap_path = maybe_plot_umap(X_scaled, labels, outdir)
    if umap_path:
        files.append(umap_path)

    return {
        "best_k": best_k,
        "n_fighters": int(features.shape[0]),
        "n_pca_components": int(X_pca.shape[1]),
        "explained_variance": float(pca_full.explained_variance_ratio_.sum()),
        "ks": ks,
        "silhouettes": silhouettes,
        "inertias": inertias,
        "files": files,
        "profiles": profiles,
    }


if __name__ == "__main__":  # pragma: no cover - manual smoke check
    result = run_archetypes(db_path=DEFAULT_DB_PATH, outdir="outputs")
    print(f"Clustered {result['n_fighters']} fighters into {result['best_k']} "
          f"archetypes ({result['n_pca_components']} PCA components, "
          f"{result['explained_variance'] * 100:.1f}% variance).")
    for f in result["files"]:
        print("  wrote", f)

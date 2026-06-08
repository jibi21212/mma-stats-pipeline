"""Unit tests for the Python ML component (db / archetypes / relationships).

Run from the ml/ directory:  python -m pytest -q
They use the synthetic_db fixture (see conftest.py) — no scraped DB required.
"""
import os

import pandas as pd
import pytest

import db
import archetypes
import relationships


# --------------------------------------------------------------------------- #
# db.py
# --------------------------------------------------------------------------- #

def test_loaders(synthetic_db):
    fighters = db.load_fighters(synthetic_db)
    fights = db.load_fights(synthetic_db)
    rounds = db.load_round_stats(synthetic_db)
    assert len(fighters) == 35 and "name" in fighters.columns        # 30 main + 2 low + 3 no-round
    assert len(fights) == 17 and "winner_name" in fights.columns     # 15 main + 2 low
    assert len(rounds) == 92 and "fighter_name" in rounds.columns    # 15*6 + 2


def test_get_connection_missing(tmp_path):
    with pytest.raises(FileNotFoundError):
        db.get_connection(str(tmp_path / "nope.db"))


def test_features_columns_and_clean(synthetic_db):
    feats = db.build_fighter_features(db_path=synthetic_db)
    assert set(feats.columns) == set(db.feature_columns())
    assert feats.index.name == "name"
    assert feats.isna().sum().sum() == 0                  # everything imputed
    assert (feats.dtypes != object).all()                 # numeric only


def test_row_filters(synthetic_db):
    # Defaults: require_round_data=True, min_fights=3 -> only the 30 main fighters.
    feats = db.build_fighter_features(db_path=synthetic_db)
    assert len(feats) == 30
    assert "Fighter00" in feats.index
    assert "LowA" not in feats.index                      # dropped: too few fights
    assert "NoRoundA" not in feats.index                  # dropped: no round data

    incl = db.build_fighter_features(db_path=synthetic_db, require_round_data=False)
    assert "NoRoundA" in incl.index and len(incl) == 33   # +3 round-less fighters

    allf = db.build_fighter_features(db_path=synthetic_db, require_round_data=False, min_fights=1)
    assert "LowA" in allf.index and len(allf) == 35       # +2 low-fight fighters


def test_derived_features(synthetic_db):
    feats = db.build_fighter_features(db_path=synthetic_db)
    # Fighter00: wins=5, losses=1 -> win_rate 5/6; its one win was a KO -> finish_rate 1.0
    assert feats.loc["Fighter00", "win_rate"] == pytest.approx(5 / 6, abs=1e-6)
    assert feats.loc["Fighter00", "finish_rate"] == pytest.approx(1.0)
    # Fighter02's only win was a Decision -> finish_rate 0.0
    assert feats.loc["Fighter02", "finish_rate"] == pytest.approx(0.0)


# --------------------------------------------------------------------------- #
# relationships.py
# --------------------------------------------------------------------------- #

def test_correlations(synthetic_db):
    feats = db.build_fighter_features(db_path=synthetic_db)
    pearson, spearman = relationships.correlation_matrices(feats)
    assert pearson.shape[0] == pearson.shape[1] == feats.shape[1]
    # reach_in was generated to track height_in closely.
    assert pearson.loc["height_in", "reach_in"] > 0.8
    top = relationships.top_correlations(pearson, "pearson", top_n=5)
    for col in ("feature_a", "feature_b", "correlation", "method"):
        assert col in top.columns


def test_association_rules(synthetic_db):
    feats = db.build_fighter_features(db_path=synthetic_db)
    rules = relationships.mine_association_rules(feats)   # 30 rows >= MIN_ROWS_FOR_RULES
    assert isinstance(rules, pd.DataFrame)
    for col in ("antecedents", "consequents", "support", "confidence", "lift"):
        assert col in rules.columns
    # Too-few-rows guard returns a well-formed empty frame, not a crash.
    small = relationships.mine_association_rules(feats.head(5))
    assert isinstance(small, pd.DataFrame) and small.empty


def test_relationships_run(synthetic_db, tmp_path):
    feats = db.build_fighter_features(db_path=synthetic_db)
    out = relationships.run_relationships(features=feats, outdir=str(tmp_path / "rel"))
    assert "files" in out
    for path in out["files"]:
        assert os.path.exists(path)


# --------------------------------------------------------------------------- #
# archetypes.py
# --------------------------------------------------------------------------- #

def test_scale_reduce_and_choose_k(synthetic_db):
    feats = db.build_fighter_features(db_path=synthetic_db)
    X_scaled, X_pca, X_pca2, scaler, pca_full, pca2 = archetypes.scale_and_reduce(feats)
    assert X_scaled.shape[0] == len(feats)
    assert X_pca2.shape[1] == 2
    best_k, ks, sils, inertias = archetypes.choose_k(X_pca, k_min=2, k_max=5)
    assert 2 <= best_k <= 5
    assert len(ks) == len(sils) == len(inertias)


def test_run_archetypes_with_forced_k(synthetic_db, tmp_path):
    feats = db.build_fighter_features(db_path=synthetic_db)
    out = archetypes.run_archetypes(features=feats, outdir=str(tmp_path / "arch"), k=3)
    assert out["best_k"] == 3
    assert "files" in out and out["files"]
    for path in out["files"]:
        assert os.path.exists(path)


def test_build_cluster_profiles(synthetic_db):
    from sklearn.cluster import KMeans
    feats = db.build_fighter_features(db_path=synthetic_db)
    _, X_pca, _, _, _, _ = archetypes.scale_and_reduce(feats)
    labels = KMeans(n_clusters=3, n_init=10, random_state=0).fit_predict(X_pca)
    profiles = archetypes.build_cluster_profiles(feats, labels)
    assert len(profiles) == 3
    assert "archetype_label" in profiles.columns
    assert "size" in profiles.columns
    assert profiles["size"].sum() == len(feats)

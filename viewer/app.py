"""Local record viewer for data/ufc.db - a Streamlit GUI.

This is NOT a hosted web app: it runs locally on demand and opens in your browser
at http://localhost. It reads the SQLite database READ-ONLY.

    pip install -r viewer/requirements.txt
    streamlit run viewer/app.py

Browse fighters (with fight history + round-by-round stats), events, fights, and
the ML archetypes/charts produced by ml/run_all.py.
"""
import os
import sqlite3

import pandas as pd
import streamlit as st

_HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_DB = os.path.normpath(os.path.join(_HERE, "..", "data", "ufc.db"))
ML_OUTPUTS = os.path.normpath(os.path.join(_HERE, "..", "ml", "outputs"))


@st.cache_data(show_spinner=False)
def load_table(db_path: str, table: str) -> pd.DataFrame:
    """Read a whole table from the read-only SQLite DB into a DataFrame."""
    uri = f"file:{os.path.abspath(db_path)}?mode=ro"
    con = sqlite3.connect(uri, uri=True)
    try:
        return pd.read_sql_query(f"SELECT * FROM {table}", con)
    finally:
        con.close()


def _fmt(v) -> str:
    if v is None or (isinstance(v, float) and pd.isna(v)):
        return "-"
    if isinstance(v, float) and float(v).is_integer():
        return str(int(v))
    return str(v)


def _cols(df: pd.DataFrame, wanted) -> list:
    return [c for c in wanted if c in df.columns]


def render_overview(fighters, events, fights, rounds):
    st.header("Overview")
    c1, c2, c3, c4 = st.columns(4)
    c1.metric("Fighters", f"{len(fighters):,}")
    c2.metric("Events", f"{len(events):,}")
    c3.metric("Fights", f"{len(fights):,}")
    c4.metric("Round rows", f"{len(rounds):,}")
    if not events.empty and "date" in events.columns:
        st.caption(f"Event date range: {events['date'].min()} -> {events['date'].max()}")

    st.subheader("Champions")
    champs = fighters[fighters.get("was_champion", 0) == 1]
    cols = _cols(champs, ["name", "wins", "losses", "draws", "championship_bouts_won"])
    if champs.empty:
        st.caption("No champions flagged in the data.")
    else:
        st.dataframe(champs[cols].sort_values("championship_bouts_won", ascending=False),
                     width="stretch", hide_index=True)


def render_fighters(fighters, fights, rounds):
    st.header("Fighters")
    q = st.text_input("Search by name", "")
    view = fighters[fighters["name"].str.contains(q, case=False, na=False)] if q else fighters
    st.caption(f"{len(view):,} match")
    st.dataframe(
        view[_cols(view, ["name", "nickname", "stance", "height_in", "reach_in", "weight_lbs",
                          "wins", "losses", "draws", "slpm", "str_acc", "td_avg", "sub_avg", "was_champion"])],
        width="stretch", hide_index=True, height=300,
    )
    if view.empty:
        return

    name = st.selectbox("Select a fighter for full profile", sorted(view["name"].tolist()))
    row = fighters[fighters["name"] == name].iloc[0]

    title = name + (f'   "{row["nickname"]}"' if pd.notna(row.get("nickname")) else "")
    st.subheader(title)
    m = st.columns(5)
    m[0].metric("Record", f'{int(row.get("wins", 0))}-{int(row.get("losses", 0))}-{int(row.get("draws", 0))}')
    m[1].metric("Height (in)", _fmt(row.get("height_in")))
    m[2].metric("Reach (in)", _fmt(row.get("reach_in")))
    m[3].metric("Weight (lbs)", _fmt(row.get("weight_lbs")))
    m[4].metric("Stance", row.get("stance") if pd.notna(row.get("stance")) else "-")

    st.markdown("**Career averages**  (percentages are 0..1 fractions)")
    career = {k: row.get(k) for k in ["slpm", "str_acc", "sapm", "str_def", "td_avg", "td_acc", "td_def", "sub_avg"]
              if k in fighters.columns}
    st.dataframe(pd.DataFrame([career]), width="stretch", hide_index=True)

    st.markdown("**Fight history**")
    fh = fights[(fights["winner_name"] == name) | (fights["loser_name"] == name)].copy()
    if fh.empty:
        st.caption("No fights found for this fighter in the data.")
    else:
        fh["result"] = fh["winner_name"].apply(lambda w: "W" if w == name else "L")
        fh["opponent"] = fh.apply(lambda r: r["loser_name"] if r["winner_name"] == name else r["winner_name"], axis=1)
        st.dataframe(
            fh[_cols(fh, ["date", "event_name", "result", "opponent", "weight_class", "method",
                          "round_ended", "time_ended", "title_bout"])].sort_values("date", ascending=False),
            width="stretch", hide_index=True,
        )

    st.markdown("**Round-by-round stats**")
    rr = rounds[rounds["fighter_name"] == name]
    if rr.empty:
        st.caption("No round-by-round data for this fighter.")
    else:
        st.dataframe(
            rr[_cols(rr, ["fight_id", "round_number", "result", "sig_str_landed", "sig_str_attempted",
                          "head_landed", "body_landed", "leg_landed", "td_landed", "td_attempted",
                          "control_time", "knockdowns", "sub_attempts"])].sort_values(["fight_id", "round_number"]),
            width="stretch", hide_index=True, height=320,
        )


def render_events(events, fights):
    st.header("Events")
    ev = events.sort_values("date", ascending=False) if "date" in events.columns else events
    st.dataframe(ev[_cols(ev, ["title", "date", "location"])], width="stretch", hide_index=True, height=300)
    if ev.empty:
        return
    title = st.selectbox("Select an event", ev["title"].tolist())
    eid = events[events["title"] == title]["event_id"].iloc[0]
    ef = fights[fights["event_id"] == eid]
    st.subheader(f"{title}  -  {len(ef)} fights")
    st.dataframe(
        ef[_cols(ef, ["winner_name", "loser_name", "weight_class", "method", "round_ended",
                      "time_ended", "title_bout", "referee"])],
        width="stretch", hide_index=True,
    )


def render_fights(fights, rounds):
    st.header("Fights")
    q = st.text_input("Filter by fighter name", "")
    view = fights
    if q:
        view = fights[fights["winner_name"].str.contains(q, case=False, na=False)
                      | fights["loser_name"].str.contains(q, case=False, na=False)]
    st.caption(f"{len(view):,} match")
    st.dataframe(
        view[_cols(view, ["fight_id", "date", "event_name", "winner_name", "loser_name",
                          "weight_class", "method", "round_ended"])].sort_values("date", ascending=False),
        width="stretch", hide_index=True, height=300,
    )
    if view.empty:
        return
    fid = st.selectbox("Inspect a fight_id (round-by-round, both fighters)", view["fight_id"].tolist())
    rr = rounds[rounds["fight_id"] == fid]
    st.dataframe(
        rr[_cols(rr, ["fighter_name", "round_number", "result", "sig_str_landed", "head_landed",
                      "body_landed", "leg_landed", "td_landed", "control_time", "knockdowns", "sub_attempts"])]
        .sort_values(["fighter_name", "round_number"]),
        width="stretch", hide_index=True,
    )


def render_archetypes():
    st.header("Archetypes (ML)")
    profiles = os.path.join(ML_OUTPUTS, "cluster_profiles.csv")
    if not os.path.exists(profiles):
        st.info("No ML outputs yet. Generate them:\n\n`cd ml && python run_all.py --min-fights 5 --k 6`")
        return
    st.subheader("Cluster profiles")
    st.dataframe(pd.read_csv(profiles), width="stretch", hide_index=True)

    clusters = os.path.join(ML_OUTPUTS, "fighter_clusters.csv")
    if os.path.exists(clusters):
        st.subheader("Fighter -> cluster")
        fc = pd.read_csv(clusters)
        q = st.text_input("Search fighter", "")
        if q:
            fc = fc[fc["name"].str.contains(q, case=False, na=False)]
        st.dataframe(fc, width="stretch", hide_index=True, height=300)

    st.subheader("Charts")
    for png, cap in [("pca_scatter.png", "PCA projection coloured by archetype"),
                     ("correlation_heatmap.png", "Feature correlations"),
                     ("silhouette.png", "Choosing k (silhouette + elbow)"),
                     ("dendrogram.png", "Hierarchical clustering")]:
        p = os.path.join(ML_OUTPUTS, png)
        if os.path.exists(p):
            st.image(p, caption=cap, width="stretch")


def main():
    st.set_page_config(page_title="UFC Stats Viewer", page_icon="(MMA)", layout="wide")
    st.sidebar.title("UFC Stats Viewer")
    db_path = st.sidebar.text_input("Database path", DEFAULT_DB)

    if not os.path.exists(db_path):
        st.error(f"Database not found at:\n\n`{db_path}`\n\nRun the scraper first: "
                 f"`cd scraper-go && go run .`")
        st.stop()

    fighters = load_table(db_path, "fighters")
    events = load_table(db_path, "events")
    fights = load_table(db_path, "fights")
    rounds = load_table(db_path, "round_stats")

    section = st.sidebar.radio("View", ["Overview", "Fighters", "Events", "Fights", "Archetypes (ML)"])
    st.sidebar.caption(f"{len(fighters):,} fighters | {len(events):,} events | {len(fights):,} fights")
    st.sidebar.caption("Read-only local viewer. Not hosted.")

    if section == "Overview":
        render_overview(fighters, events, fights, rounds)
    elif section == "Fighters":
        render_fighters(fighters, fights, rounds)
    elif section == "Events":
        render_events(events, fights)
    elif section == "Fights":
        render_fights(fights, rounds)
    else:
        render_archetypes()


if __name__ == "__main__":
    main()

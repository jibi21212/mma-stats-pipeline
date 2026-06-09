# Record Viewer (`viewer/`)

A small **local** GUI for browsing `data/ufc.db`. It is **not a hosted web app** - it runs on your
machine on demand (`streamlit run`) and opens in your browser at `http://localhost:8501`. It reads the
database **read-only**.

## Install & run

```bash
pip install -r viewer/requirements.txt
streamlit run viewer/app.py
```

(Or reuse the ML venv: `ml/.venv` already has pandas - just `pip install streamlit` into it.)

A browser tab opens automatically. Stop it with `Ctrl+C` in the terminal.

## What you can see

Pick a view from the sidebar:

| View | Shows |
|---|---|
| **Overview** | Row counts, event date range, and the champions list. |
| **Fighters** | Searchable fighter table; pick one for a full profile: bio, record, career averages, **fight history** (opponent / result / method), and **round-by-round** stats. |
| **Events** | All events (newest first); pick one to see its fight card. |
| **Fights** | Filter fights by fighter; pick a `fight_id` to see both fighters' round-by-round stats. |
| **Archetypes (ML)** | The cluster profiles, a searchable fighter->cluster table, and the charts from `ml/outputs/` (run `python ml/run_all.py --min-fights 5 --k 6` first). |
| **Fight Predictor** | Pick two fighters: predicts `P(A beats B)` from the leakage-safe, weight-class-gated model in `ml/models/predictor.joblib` (train it with `cd ml && python predict.py --train`). Cross-weight / cross-gender matchups are **refused**; allowed matchups show win probabilities, a side-by-side tale of the tape, and the model's held-out accuracy. |

## Notes

- Default DB path is `../data/ufc.db` (editable in the sidebar).
- Percentages are stored as `0..1` fractions (see `docs/SCHEMA_CONTRACT.md`).
- The viewer never writes to the database.

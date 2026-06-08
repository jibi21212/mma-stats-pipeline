#!/usr/bin/env bash
# Creates an isolated virtual environment for the ML component and registers it
# as a Jupyter kernel. Run ONCE from the ml/ directory:
#   bash setup_env.sh
# Then either activate the venv for the scripts, or pick the "UFC ML (.venv)"
# kernel in the notebook.
set -euo pipefail

python -m venv .venv
VENV_PY=".venv/bin/python"

"$VENV_PY" -m pip install --upgrade pip
"$VENV_PY" -m pip install -r requirements.txt -r requirements-dev.txt
"$VENV_PY" -m ipykernel install --user --name ufc-ml --display-name "UFC ML (.venv)"

echo
echo "Done. To use the environment:"
echo "  source .venv/bin/activate                 # activate it in this shell"
echo "  python run_all.py --min-fights 5 --k 6    # run the scripts, or..."
echo "  jupyter lab notebook.ipynb               # ...and pick kernel 'UFC ML (.venv)'"

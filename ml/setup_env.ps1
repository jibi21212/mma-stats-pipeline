# Creates an isolated virtual environment for the ML component and registers it
# as a Jupyter kernel. Run ONCE from the ml/ directory:
#   ./setup_env.ps1
# Then either activate the venv for the scripts, or pick the "UFC ML (.venv)"
# kernel in the notebook.
$ErrorActionPreference = "Stop"

python -m venv .venv
$venvPy = Join-Path ".venv" "Scripts\python.exe"

& $venvPy -m pip install --upgrade pip
& $venvPy -m pip install -r requirements.txt -r requirements-dev.txt
& $venvPy -m ipykernel install --user --name ufc-ml --display-name "UFC ML (.venv)"

Write-Host ""
Write-Host "Done. To use the environment:"
Write-Host "  .\.venv\Scripts\Activate.ps1              # activate it in this shell"
Write-Host "  python run_all.py --min-fights 5 --k 6    # run the scripts, or..."
Write-Host "  jupyter lab notebook.ipynb               # ...and pick kernel 'UFC ML (.venv)'"

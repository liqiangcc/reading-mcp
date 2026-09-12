#!/usr/bin/env bash
set -euo pipefail
# Create a dedicated dependency environment; never modify system Python.
: "${PDF_LAYOUT_VENV:?set PDF_LAYOUT_VENV to a new absolute virtualenv path}"
: "${PYTHON:=python3}"
[[ "$PDF_LAYOUT_VENV" = /* ]] || { echo 'PDF_LAYOUT_VENV must be absolute' >&2; exit 1; }
[[ ! -e "$PDF_LAYOUT_VENV" ]] || { echo 'Refusing to overwrite an existing environment; choose a new path' >&2; exit 1; }
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
"$PYTHON" -c 'import sys; assert sys.version_info >= (3, 12), "Python 3.12+ is required"'
"$PYTHON" -m venv "$PDF_LAYOUT_VENV"
"$PDF_LAYOUT_VENV/bin/python" -I -m pip install --only-binary=:all: -r "$script_dir/requirements.txt"
"$PDF_LAYOUT_VENV/bin/python" -I -m pip check
"$PDF_LAYOUT_VENV/bin/python" -I -c 'import pymupdf4llm; pymupdf4llm.use_layout(True)'
printf 'Configure the service environment:\nREADING_MCP_PDF_LAYOUT_PYTHON=%s/bin/python\n' "$PDF_LAYOUT_VENV"

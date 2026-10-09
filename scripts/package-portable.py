"""Deprecated developer compatibility alias for stage-installer-payload.py.

The old arguments still create an internal payload directory. No portable ZIP is
created. End users install and update with the gloamere-image Setup EXE.
"""
import importlib.util
import sys
from pathlib import Path


def main(argv=None):
    print("Deprecated: package-portable.py only stages internal installer inputs; no ZIP is produced.", file=sys.stderr)
    spec = importlib.util.spec_from_file_location("installer_staging", Path(__file__).with_name("stage-installer-payload.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.main(argv)


if __name__ == "__main__":
    raise SystemExit(main())

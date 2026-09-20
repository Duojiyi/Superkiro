#!/usr/bin/env python3
"""Verify a locally retrieved online backup bundle. Never contacts any server."""
import argparse
import importlib.util
import json
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('manifest', type=Path)
    args = parser.parse_args()
    spec = importlib.util.spec_from_file_location('server_backup', Path(__file__).with_name('server-backup.py'))
    backup = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(backup)
    try:
        result = backup.verify_bundle(args.manifest)
    except (OSError, ValueError, KeyError, TypeError, AttributeError, backup.BackupError):
        parser.exit(1, 'FAIL: missing, unsafe, incomplete or corrupt backup bundle\n')
    print(json.dumps(result))


if __name__ == '__main__':
    main()

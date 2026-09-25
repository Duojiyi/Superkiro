"""Promote a staged, identity-checked candidate using the shared release transaction."""
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from deploy.release_candidate import ROOT, deployment_lock, keep_off_host, pinned_connection, promote


def main(credentials=None, ssh=None):
    credentials = json.load(sys.stdin) if credentials is None else credentials
    owns_connection = ssh is None
    if owns_connection:
        ssh = pinned_connection(credentials)
    try:
        with deployment_lock(ssh):
            report = json.loads((ROOT / 'deployment-candidate-results.json').read_text(encoding='utf-8'))
            promote(ssh, report)
            keep_off_host(ssh, report, credentials)
    finally:
        if owns_connection:
            ssh.close()


if __name__ == '__main__':
    main()

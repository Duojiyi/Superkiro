"""Promote a staged, identity-checked candidate using the shared release transaction."""
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from deploy.release_candidate import ROOT, deployment_lock, pinned_connection, promote


def main(ssh=None):
    owns_connection = ssh is None
    if owns_connection:
        ssh = pinned_connection(json.load(sys.stdin))
    try:
        with deployment_lock(ssh):
            report = json.loads((ROOT / 'deployment-candidate-results.json').read_text(encoding='utf-8'))
            from deploy.release_announcements import verify_public_release
            promote(ssh, report, extra_readiness=verify_public_release if 'web_sha256' in report else None)
    finally:
        if owns_connection:
            ssh.close()


if __name__ == '__main__':
    main()

"""Offline build-to-artifact mapping. This is metadata, NOT a signature/attestation."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import subprocess


def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args], text=True).strip()


def record(root, artifact, version, platform, arch):
    if artifact.is_symlink() or not artifact.is_file() or artifact.stat().st_size == 0:
        raise ValueError('Artifact must be a nonempty regular file, not a symlink')
    # Never label a dirty checkout as the contents of HEAD. Ignored build outputs
    # are intentionally excluded, but untracked source is not.
    if git(root, 'status', '--porcelain', '--untracked-files=all'):
        raise ValueError('Source checkout is dirty; commit/review source before packaging')
    digest = hashlib.sha256()
    size = 0
    with artifact.open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(block)
            size += len(block)
    return {
        'schema_version': 1, 'version': version, 'platform': platform, 'arch': arch,
        'source_commit': git(root, 'rev-parse', 'HEAD'), 'source_dirty': False,
        'artifact': artifact.name, 'sha256': digest.hexdigest(), 'size': size,
        'created_at': datetime.now(timezone.utc).isoformat(),
        'build': {key: os.environ.get(key) for key in
                  ('GITHUB_REPOSITORY', 'GITHUB_RUN_ID', 'GITHUB_RUN_ATTEMPT', 'GITHUB_WORKFLOW')},
        'signature_verification': 'not_performed',
        'publication_approved': False,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifact', type=Path, required=True)
    parser.add_argument('--version', required=True)
    parser.add_argument('--platform', choices=['windows', 'macos'], required=True)
    parser.add_argument('--arch', choices=['x64', 'arm64'], required=True)
    args = parser.parse_args()
    evidence = record(Path(__file__).resolve().parents[1], args.artifact,
                      args.version, args.platform, args.arch)
    # Never overwrite either the artifact or a previous mapping.
    with args.artifact.with_name(args.artifact.name + '.provenance.json').open('x', encoding='utf-8') as target:
        json.dump(evidence, target, ensure_ascii=False, indent=2)
        target.write('\n')


if __name__ == '__main__':
    main()

#!/usr/bin/env python3
"""Run authenticated snapshot backup with TLS validation and protected credentials."""
import os
from pathlib import Path
import subprocess
root = Path('/opt/kiro-byok')
env = dict(os.environ)
for line in Path('/etc/kiro-byok/gateway.env').read_text().splitlines():
    if '=' in line:
        k, v = line.split('=', 1)
        env[k] = v
env.update(DATA_DIR=str(root / 'data'), BACKUP_DIR=str(root / 'backups'),
    ADMIN_BASE_URL='https://160.202.47.98', HEALTH_URL='https://160.202.47.98/healthz',
    CURL_CA_BUNDLE='/etc/kiro-byok/root.crt', NO_PROXY='160.202.47.98')
subprocess.run(['bash', str(root / 'current/deploy/backup/backup.sh')], env=env, check=True)

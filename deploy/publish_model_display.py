"""Set the multipliers the customer's model list shows ("2.2x Credit"), revision-checked.

Read JSON from stdin: {"password": "SSH password"}. Display only: what a request is charged
still comes from the price versions and the charge multipliers, none of which changes. Only
`rate_multiplier` of Opus and Sonnet mappings is set; every other field and mapping is
published back exactly as read, and the live configuration is read back before success.
SSH/admin credentials stay in memory; only a summary is printed.
"""
import base64
import hashlib
import hmac
import json
import re
import sys
import time
from pathlib import Path

import requests

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from deploy.release_candidate import (PreconditionFailed, deployment_lock, pinned_connection,
                                      write_remote)

ORIGIN = 'https://kiro.rent'
# As Kiro's own model list shows them: every Opus 2.2x Credit, every Sonnet 1.3x.
FAMILY_RATES = (('claude-opus-', 2.2), ('claude-sonnet-', 1.3))
REASON = ('Model list shows Kiro-style multipliers: Opus 2.2x, Sonnet 1.3x. '
          'Display only; prices and charge multipliers unchanged.')
SETTLING = 'still settling'


class Refused(Exception):
    """The server refused a publication, which changes nothing; the text is its reason."""


def display_rate(model_id):
    return next((rate for prefix, rate in FAMILY_RATES if model_id.startswith(prefix)), None)


def with_display_rates(models):
    """The mappings with the family multipliers set, or None when none changes."""
    # A server without the field would drop the multipliers without a word.
    if any('rate_multiplier' not in m for m in models):
        raise PreconditionFailed('The live server has no model list multipliers; deploy it first')
    result, changed = [], False
    for m in models:
        rate = display_rate(m['exposed_model_id'])
        if rate is not None and m['rate_multiplier'] != rate:
            m = {**m, 'rate_multiplier': rate}
            changed = True
        result.append(m)
    return result if changed else None


def publish(call, record, wait=time.sleep, attempts=40):
    """Publish through `call(path, body=None)`, `record(before, update)` first each time.

    Returns the live configuration and whether anything was published. A publication
    refused while requests settle is retried; any other refusal changed nothing."""
    for _ in range(attempts):
        before = call('commercial-config')['config']
        models = with_display_rates(before['models'])
        if models is None:
            return before, False
        update = {'expected_revision': before['revision'], 'reason': REASON, 'models': models}
        record(before, update)
        try:
            call('commercial-config', update)
        except Refused as refusal:
            if SETTLING not in str(refusal):
                raise PreconditionFailed(f'Publication refused: {str(refusal)[:200]}') from None
            wait(3)
            continue
        after = call('commercial-config')['config']
        live = {m['id']: m for m in after['models']}
        if len(after['models']) != len(models) or any(live.get(m['id']) != m for m in models):
            raise RuntimeError('Readback differs from the intended mappings')
        return after, True
    raise PreconditionFailed('Requests kept settling; run again when the gateway is idle')


def totp_code(secret, now):
    # RFC 6238, matching the gateway: SHA-1, six digits, 30-second period.
    if not isinstance(secret, str) or not re.fullmatch(r'[A-Z2-7]{32,128}', secret):
        raise PreconditionFailed('Invalid TOTP credential')
    key = base64.b32decode(secret + '=' * ((-len(secret)) % 8))
    digest = hmac.new(key, (int(now) // 30).to_bytes(8, 'big'), hashlib.sha1).digest()
    offset = digest[-1] & 15
    value = int.from_bytes(digest[offset:offset + 4], 'big') & 0x7fffffff
    return f'{value % 1000000:06d}'


def main():
    credentials = json.load(sys.stdin)
    ssh = pinned_connection(credentials)
    try:
        with deployment_lock(ssh):
            with ssh.open_sftp() as sftp:
                access = json.loads(sftp.open('/etc/kiro-byok/admin-access.json').read())
            with requests.Session() as http:
                http.trust_env = False
                headers = {'Origin': ORIGIN}

                def call(path, body=None):
                    response = http.request('GET' if body is None else 'POST',
                                            ORIGIN + '/api/v1/admin/' + path, json=body,
                                            headers=headers, timeout=45, allow_redirects=False)
                    if response.status_code in (400, 409):
                        raise Refused(response.json().get('error', ''))
                    if response.status_code != 200:
                        raise RuntimeError(f'{path}: HTTP {response.status_code}')
                    return response.json()

                login = {'username': access['username'], 'password': access['password']}
                if 'totpSecret' in access:
                    login['totpCode'] = totp_code(access['totpSecret'], time.time())
                try:
                    call('session', login)
                except Refused:
                    raise PreconditionFailed('Administrator login failed') from None
                headers['X-CSRF-Token'] = call('session')['csrfToken']
                stamp = int(time.time())

                def record(before, update):
                    # Rollback evidence on the server before a single mutation is sent.
                    backup = f'/opt/kiro-byok/model-display-{stamp}'
                    write_remote(ssh, backup + '-before.json', json.dumps(before).encode())
                    write_remote(ssh, backup + '-publication.json', json.dumps(update).encode())

                try:
                    live, published = publish(call, record)
                finally:
                    try:
                        call('session/revoke', {})
                    except Exception:
                        pass
                print(json.dumps({
                    'status': 'published' if published else 'unchanged',
                    'revision': live['revision'],
                    'rates': {m['exposed_model_id']: m['rate_multiplier'] for m in live['models']
                              if display_rate(m['exposed_model_id']) is not None},
                }))
    finally:
        ssh.close()


if __name__ == '__main__':
    main()

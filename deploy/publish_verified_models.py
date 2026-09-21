"""Publish verified testing models; never import providers or change existing mappings.

Read JSON from stdin: {"password": "SSH password", "verified_capabilities": {
  "exact-new-model-id": {"verified": true, "context_window": 123456,
    "max_output": 12345, "supports_tools": true, "supports_vision": false,
    "supports_reasoning": false}}}.
Numbers above illustrate the schema, NOT verified model limits. Supply independently
verified capabilities for every new exact model ID; aliases are never inferred.
Existing providers must already support the new models (checked by publication).
SSH/admin credentials and configuration remain in memory; only a summary is printed.
"""
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import requests
from test_deployed_server import connect, ROOT

MODELS = ['claude-sonnet-4-6', 'claude-opus-4-6', 'claude-opus-4-7',
          'claude-opus-4-8', 'claude-opus-5', 'claude-sonnet-5']
GROUP = 'group-pro-plus'
CAPABILITIES = ('context_window', 'max_output', 'supports_tools',
                'supports_vision', 'supports_reasoning')


def merge_models(existing, verified):
    """Keep complete existing mappings; validate all additions before publication."""
    merged = list(existing)
    for i, model in enumerate(MODELS):
        if any(m['group_id'] == GROUP and m['exposed_model_id'] == model
               for m in existing):
            continue
        caps = verified.get(model) if isinstance(verified, dict) else None
        if (not isinstance(caps, dict) or caps.get('verified') is not True
                or any(k not in caps for k in CAPABILITIES)
                or any(type(caps[k]) is not int for k in CAPABILITIES[:2])
                or not 0 < caps['max_output'] <= caps['context_window'] <= 10_000_000
                or any(type(caps[k]) is not bool for k in CAPABILITIES[2:])):
            raise ValueError('Missing or invalid verified capabilities for ' + model)
        mapping_id = 'kimera-test-' + model
        if any(m['id'] == mapping_id for m in merged):
            raise ValueError('Existing mapping ID conflicts with ' + model)
        merged.append({
            'id': mapping_id, 'group_id': GROUP, 'exposed_model_id': model,
            'target_provider_id': 'kimera-primary', 'target_model': model,
            **{k: caps[k] for k in CAPABILITIES},
            'credit_multiplier': 1.0, 'visible': True, 'sort_order': i,
            'aliases': [], 'fallback_chain': [],
        })
    return merged


def publish(req, verified):
    config = req('commercial-config')['config']
    models = merge_models(config['models'], verified)
    if models == config['models']:
        return config
    # No provider import: it could overwrite manually configured provider settings.
    # The revision guard and server's atomic validation reject stale/invalid updates.
    return req('commercial-config', {
        'expected_revision': config['revision'],
        'reason': 'Add explicitly verified testing models; preserve existing configuration.',
        'models': models,
    })['config']


def main():
    inputs = json.load(sys.stdin)
    text = json.loads((ROOT / '.acceptance/upstream-model-probes.json').read_text())
    tools = json.loads((ROOT / '.acceptance/upstream-tool-probes.json').read_text())
    if not all(any(x['model'] == m and x.get('status') == 200 and x.get('has_text')
                   for x in text)
               and any(x['model'] == m and x.get('tool_use') for x in tools)
               for m in MODELS):
        raise ValueError('Required text/tool probes are missing')
    ssh = connect(inputs['password'], False)
    try:
        with ssh.open_sftp() as f:
            web = json.loads(f.open('/etc/kiro-byok/admin-access.json').read())
        with requests.Session() as s:
            s.trust_env = False
            base = 'https://kiro.rent'
            s.headers['Origin'] = base

            def req(path, data=None):
                r = s.request('GET' if data is None else 'POST',
                              base + '/api/v1/admin/' + path, json=data,
                              timeout=30, allow_redirects=False)
                if r.status_code != 200:
                    raise RuntimeError(f'{path}: HTTP {r.status_code}')
                return r.json()

            req('session', {'username': web['username'], 'password': web['password']})
            s.headers['x-csrf-token'] = req('session')['csrfToken']
            published = publish(req, inputs.get('verified_capabilities', {}))
            print(json.dumps({'revision': published['revision'], 'models': MODELS,
                              'rates_changed': False,
                              'pricing_status': 'existing testing rates; not approved for commercial sales'}))
    finally:
        ssh.close()


if __name__ == '__main__':
    main()

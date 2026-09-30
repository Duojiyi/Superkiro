"""Republish every callable model's price in force with the official price it is computed from.

Read JSON from stdin: {"password": "SSH password"}. Charging does not change: each new version
keeps the credits and costs of the one it replaces and adds the `official` block the server
checks them against, built from deploy/pricing_policy.json at the live face value (official
prices, `priced_as`, `retail_multiplier`; the primary provider's `provider_upstream_multipliers`
entry, else `upstream_multiplier`, on the `upstream_price_basis` that provider bills). Hidden
models are republished too: they can still be called by name, and a face-value change must
reprice them with the listed ones. Retired ones cannot be called and are left alone. When the
policy would not reproduce a model's credits or costs, or its group's 分组倍率 or its own 模型倍率
is not 1 (customers would pay more than the block says), the model is named and nothing is
published.

The settings gain the policy's defaults: official_usd_cny 1.0, the retail and upstream
multipliers, each provider's, and a route cost for every `upstream_price_basis` entry: the
measured basis and that provider's multiplier, which cost those routes exactly what their
versions do today. Without them a later official price for the route's model would re-cost the
route from the list price.

The official price table gains every upstream model a listed or hidden, non-retired mapping
sends requests to, first or as a fallback, at the official prices the policy prices it from
(`priced_as` applied), noted "官方价 · 迁移时写入"; an entry the server already has is kept as it
is. Every route is then costed from official prices, so a provider's 成本倍率 reaches all of
them. That must not change what any request costs today: each route of each mapping is costed
as settlement costs it, before and after, and a route whose CNY per million tokens would differ
in any class (or that is costed today at an estimate, or from a USD price) is named and nothing
is published. The summary lists the prices added and every route shown unchanged.

Revision-checked; the new versions start 30 seconds after the server's clock, and the live
configuration is read back before and after they do. The server must already run the release
that knows official prices: a configuration without plans comes from an older one, which would
drop the blocks and the settings, and is refused before anything is sent.

--dry-run reads the live configuration through the same admin session and writes the intended
publication to .acceptance/ without sending it. SSH/admin credentials stay in memory; only a
summary is printed.
"""
import contextlib
import email.utils
import json
import sys
import time
from decimal import ROUND_HALF_UP, Decimal
from pathlib import Path

import requests

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from deploy.release_candidate import (ROOT, PreconditionFailed, deployment_lock, pinned_connection,
                                      write_remote)

ORIGIN = 'https://kiro.rent'
POLICY = json.loads((ROOT / 'deploy/pricing_policy.json').read_text(encoding='utf8'))
OUT = ROOT / '.acceptance'
# Input, output, cache write and cache read, as billing adds them up.
PRICES = ('input', 'output', 'cache_creation', 'cache_read')
# CNY per official dollar: ¥1 = $1.
OFFICIAL_USD_CNY = 1.0
# Where an official price the migration adds comes from, as the console shows it.
OFFICIAL_NOTE = '官方价 · 迁移时写入'
ACTIVATION_DELAY_SECS = 30
REASON = ('Record what every callable price is computed from: official USD x retail multiplier '
          'at the face value, cost x the provider multiplier, and what the measured routes '
          'bill. Credits and costs unchanged.')


def four(values, what):
    """Input, output, cache write and cache read in USD per million, bounded as the server does."""
    if (not isinstance(values, list) or len(values) != 4
            or any(type(value) not in (int, float) or not 0 <= value <= 10_000 for value in values)):
        raise PreconditionFailed(f'Invalid pricing policy: {what}')
    return [float(value) for value in values]


def official_block(mapping, policy, face_value):
    """What the policy prices `mapping` from, at `face_value` CNY a credit, or None when it has
    no official price for the model."""
    model = mapping['exposed_model_id']
    priced_as = policy.get('priced_as', {}).get(model, model)
    if priced_as not in policy['official_prices_per_m']:
        return None
    official = four(policy['official_prices_per_m'][priced_as], priced_as)
    provider = mapping['target_provider_id']
    block = {f'{kind}_usd_per_m': usd for kind, usd in zip(PRICES, official)}
    block.update({
        'price_multiplier': float(policy.get('model_retail_multipliers', {}).get(
            model, policy['retail_multiplier'])),
        'cost_multiplier': float(policy['provider_upstream_multipliers'].get(
            provider, policy['upstream_multiplier'])),
        'usd_cny': OFFICIAL_USD_CNY,
        'credit_face_value_cny': face_value,
    })
    route = f"{provider}/{mapping['target_model']}"
    basis = policy.get('upstream_price_basis', {}).get(route)
    if basis is not None:
        block['cost_basis_usd_per_m'] = four(basis, route)
    return block


def credits(block):
    """Micro-credits per million tokens as the server checks them: official x price multiplier x
    CNY per official dollar / face value x 1,000,000, in this order in binary floating point,
    rounded half away from zero (Python's round() rounds halves to even)."""
    return [int(Decimal(block[f'{kind}_usd_per_m'] * block['price_multiplier'] * block['usd_cny']
                        / block['credit_face_value_cny'] * 1_000_000)
                .to_integral_value(ROUND_HALF_UP))
            for kind in PRICES]


def costs(block):
    """CNY per million tokens: the cost basis (the official prices unless the upstream bills
    others) x cost multiplier x CNY per official dollar."""
    basis = block.get('cost_basis_usd_per_m') or [block[f'{kind}_usd_per_m'] for kind in PRICES]
    return [usd * block['cost_multiplier'] * block['usd_cny'] for usd in basis]


def close(live, wanted):
    """Equal as the server compares costs: within 1e-9 of the larger, or 1e-12 near zero."""
    return abs(live - wanted) <= max(1e-9 * max(abs(live), abs(wanted)), 1e-12)


def in_force(versions, rate_card, names, now):
    """The version a new request is priced at, as billing resolves it: the latest from `now` or
    before of the first of `names` that has one, else the rate card's '*'."""
    def latest(name):
        found = [v for v in versions if v['rate_card_id'] == rate_card and v['model'] == name
                 and v['effective_from_secs'] <= now]
        return max(found, key=lambda v: v['effective_from_secs'], default=None)
    return next(filter(None, map(latest, [n for n in names if n != '*'])), None) or latest('*')


def migrated_version(mapping, current, scheduled, policy, face_value, stamp):
    """(`current` republished from `stamp` with its official block, None) when the policy
    reproduces its credits and costs; (None, why not) otherwise."""
    model = mapping['exposed_model_id']
    if current is None:
        return None, f'{model}: no price in force'
    if current['model'] != model:
        return None, f"{model}: charged at the price of {current['model']}"
    if scheduled:
        return None, f'{model}: a later price is scheduled'
    if (current['pricing_mode'], current['currency'], current['margin_multiplier'],
            current['per_call_credit']) != ('fixed', 'CNY', 1, 0):
        return None, f'{model}: not a fixed CNY price'
    block = official_block(mapping, policy, face_value)
    if block is None:
        return None, f'{model}: no official price in the pricing policy'
    live, wanted = [current[f'fixed_{kind}_credit_per_m'] for kind in PRICES], credits(block)
    if live != wanted:
        return None, f'{model}: credits {live}, the policy gives {wanted}'
    live, wanted = [current[f'{kind}_price_per_m'] for kind in PRICES], costs(block)
    if not all(map(close, live, wanted)):
        return None, f'{model}: costs {live}, the policy gives {wanted}'
    return {**current, 'id': f"official-{current['rate_card_id']}-{model}-{stamp}",
            'effective_from_secs': stamp, 'official': block}, None


def route_costs(policy):
    """What each route the policy measured bills: its basis, and its provider's multiplier (else
    the policy's), as the versions it serves are costed today."""
    routes = {}
    for route, basis in policy.get('upstream_price_basis', {}).items():
        provider, _, target = route.partition('/')
        if not provider or not target:
            raise PreconditionFailed(f'Invalid pricing policy: route {route}')
        multiplier = policy['provider_upstream_multipliers'].get(
            provider, policy['upstream_multiplier'])
        routes[route] = {'basis_usd_per_m': four(basis, route),
                         'cost_multiplier': float(multiplier)}
    return routes


def settings_defaults(policy, settings, official_prices=None):
    """The settings the policy gives, on top of the live `settings`: its route costs are added to
    any the server already has, and so is the official price table given."""
    defaults = {
        'legacy_credit_face_value_cny': settings.get('legacy_credit_face_value_cny') or settings['credit_face_value_cny'],
        'official_usd_cny': OFFICIAL_USD_CNY,
        'default_price_multiplier': float(policy['retail_multiplier']),
        'default_cost_multiplier': float(policy['upstream_multiplier']),
        'provider_cost_multipliers': {provider: float(multiplier) for provider, multiplier
                                      in policy['provider_upstream_multipliers'].items()},
        'route_costs': {**(settings.get('route_costs') or {}), **route_costs(policy)},
    }
    if official_prices:
        defaults['official_prices'] = official_prices
    return defaults


def routes_of(mapping):
    """Every route a mapping sends requests to, as (provider, upstream model): its target first,
    then its fallbacks."""
    return [(mapping['target_provider_id'], mapping['target_model'])] + [
        (fallback['provider_id'], fallback['target_model'])
        for fallback in mapping.get('fallback_chain') or []]


def official_price_table(config, policy):
    """(the official price table the server is to have, the names added, the upstream models the
    policy has no price for). Every upstream model a callable mapping (listed or hidden, not
    retired) sends requests to, first or as a fallback, at the official prices the policy
    prices it from; the live table's entries are kept as they are."""
    live = config['settings'].get('official_prices') or {}
    table, added, missing = dict(live), [], []
    targets = {target for mapping in config['models'] if not mapping.get('retired')
               for _, target in routes_of(mapping)}
    for target in sorted(targets - set(live)):
        priced_as = policy.get('priced_as', {}).get(target, target)
        if priced_as not in policy['official_prices_per_m']:
            missing.append(target)
            continue
        usd = four(policy['official_prices_per_m'][priced_as], priced_as)
        table[target] = {**{f'{kind}_usd_per_m': value for kind, value in zip(PRICES, usd)},
                         'note': OFFICIAL_NOTE, 'updated_at_secs': 0}
        added.append(target)
    return table, added, missing


def route_cost(settings, versions, rate_card, mapping, provider, target, at):
    """How settlement costs a request `mapping` sends to `provider`'s `target` at `at`, as
    (where from, CNY per million tokens by class): from official prices (the route's own basis,
    else the target's official price, times the route's 成本倍率, else the provider's, else the
    default, at the official dollar rate) when they give both; else from the price version for
    the route, the one the request is charged at when it is the mapping's own target, the
    target's, or the rate card's '*'; else an estimate, (estimate, None). A USD version's prices
    are its own, in dollars: settlement converts the sum, not each price."""
    route = (settings.get('route_costs') or {}).get(f'{provider}/{target}') or {}
    basis = route.get('basis_usd_per_m')
    official = (settings.get('official_prices') or {}).get(target)
    if basis is None and official is not None:
        basis = [official[f'{kind}_usd_per_m'] for kind in PRICES]
    multiplier = route.get('cost_multiplier')
    if multiplier is None:
        multiplier = (settings.get('provider_cost_multipliers') or {}).get(provider)
    if multiplier is None:
        multiplier = settings.get('default_cost_multiplier')
    if basis is not None and multiplier is not None:
        rate = settings.get('official_usd_cny')
        rate = OFFICIAL_USD_CNY if rate is None else rate
        # In the server's order: each price times the multiplier, times the dollar rate.
        return 'official', [usd * multiplier * rate for usd in basis]

    def latest(name):
        found = [v for v in versions if v['rate_card_id'] == rate_card and v['model'] == name
                 and v['effective_from_secs'] <= at]
        return max(found, key=lambda v: v['effective_from_secs'], default=None)
    version = latest(f'{provider}/{target}')
    if (version is None and mapping['target_provider_id'] == provider
            and mapping['target_model'] == target):
        version = in_force(versions, rate_card, [mapping['exposed_model_id'], target], at)
    version = version or latest(target) or latest('*')
    if version is None:
        return 'estimate', None
    prices = [version[f'{kind}_price_per_m'] for kind in PRICES]
    return ('version' if version['currency'] == 'CNY' else 'usd'), prices


def cost_changes(config, update, now, stamp):
    """(every route shown to cost exactly what it costs today, why the others would not).

    Each route of each callable mapping is costed as settlement costs it: now with the live
    settings and versions, and from the new versions' start with the settings and versions the
    publication leaves. CNY per million tokens equal in every class, in binary floating point,
    cost every request the same; a route costed today at an estimate or from a USD price
    version that the publication would cost otherwise is named too."""
    groups = {group['id']: group for group in config['groups']}
    after_settings = update.get('settings') or config['settings']
    after_versions = config['versions'] + update['versions']
    unchanged, problems = set(), []
    for mapping in config['models']:
        if mapping.get('retired'):
            continue
        rate_card = groups[mapping['group_id']]['rate_card_id']
        for provider, target in routes_of(mapping):
            route = f'{provider}/{target}'
            was = route_cost(config['settings'], config['versions'], rate_card, mapping,
                             provider, target, now)
            will = route_cost(after_settings, after_versions, rate_card, mapping, provider,
                              target, stamp)
            same = was == will or ('usd' not in (was[0], will[0]) and None not in (was[1], will[1])
                                   and was[1] == will[1])
            if same:
                unchanged.add(route)
                continue
            today = {'estimate': 'an estimate', 'usd': f'USD {was[1]}'}.get(was[0], was[1])
            problems.append(f"route {route} ({mapping['exposed_model_id']} in "
                            f"{mapping['group_id']}): costs {today} today, {will[1]} after "
                            'the official price table is filled')
    return sorted(unchanged), problems


def newer_server(config):
    """Refuses a configuration from a server older than the release: only the release that knows
    official prices sends the plan catalog, and an older one silently drops the official blocks
    and the settings a publication sends."""
    if 'plans' not in config or 'cards_by_plan' not in config:
        raise PreconditionFailed('The server is older than the release that knows official '
                                 'prices (its configuration has no plans); deploy that release '
                                 'first. Nothing was published.')


def publication(config, policy, now, stamp):
    """(the publication, or None when nothing changes; what stops it). See `plan`."""
    update, problems, _ = plan(config, policy, now, stamp)
    return update, problems


def plan(config, policy, now, stamp):
    """(the publication, or None when nothing changes; the models and routes it cannot publish;
    what it shows: the official prices it adds, the upstream models the policy has no price for,
    and every route whose cost it shows unchanged).

    Every callable model's price in force, listed or hidden, that carries no official block yet
    is republished with one, from `stamp`, keeping its credits and costs. One already computed
    from an official price is left alone, and so is a price two models share, once. A model of a
    group whose 分组倍率, or with a 模型倍率, other than 1 is refused, as publish_pricing.py
    refuses it: its block would not say what customers pay."""
    face_value = config['settings']['credit_face_value_cny']
    groups = {group['id']: group for group in config['groups']}
    versions, problems = {}, []
    for mapping in config['models']:
        if mapping.get('retired'):
            continue
        model = mapping['exposed_model_id']
        group = groups[mapping['group_id']]
        multipliers = (group.get('margin_multiplier'), mapping.get('credit_multiplier'))
        if multipliers != (1, 1):
            problems.append(f'{model}: 分组倍率 {multipliers[0]} and 模型倍率 {multipliers[1]} '
                            'must both be 1; review required')
            continue
        rate_card = group['rate_card_id']
        current = in_force(config['versions'], rate_card, [model, mapping['target_model']], now)
        if current is not None and 'official' in current:
            continue
        scheduled = [v for v in config['versions'] if v['rate_card_id'] == rate_card
                     and v['model'] == model and v['effective_from_secs'] > now]
        version, problem = migrated_version(mapping, current, scheduled, policy, face_value, stamp)
        if problem is None and versions.get(current['id'], version) != version:
            problem = f'{model}: listed on routes the policy prices differently'
        if problem is not None:
            problems.append(problem)
        else:
            versions[current['id']] = version
    update = {'expected_revision': config['revision'], 'reason': REASON,
              'versions': list(versions.values())}
    table, added, missing = official_price_table(config, policy)
    defaults = settings_defaults(policy, config['settings'], table)
    if any(config['settings'].get(key) != value for key, value in defaults.items()):
        update['settings'] = {**config['settings'], **defaults}
    unchanged, changed = cost_changes(config, update, now, stamp)
    problems.extend(changed)
    proof = {'official_prices_added': added, 'no_official_price': missing,
             'costs_unchanged_on': unchanged}
    if not update['versions'] and 'settings' not in update:
        return None, problems, proof
    return update, problems, proof


def same_settings(live, sent):
    """Whether the settings read back are the ones sent: the server stamps when each official
    price last changed, and when the settings did."""
    def unstamped(settings):
        settings = {key: value for key, value in settings.items() if key != 'rate_updated_at_secs'}
        if settings.get('official_prices') is not None:
            settings['official_prices'] = {
                name: {key: value for key, value in price.items() if key != 'updated_at_secs'}
                for name, price in settings['official_prices'].items()}
        return settings
    shown = unstamped(live)
    return all(shown.get(key) == value for key, value in unstamped(sent).items())


def migrate(call, record, dry_run=False, wait=time.sleep):
    """Publish through `call(path, body=None)`, which returns the reply and the server's clock.

    `record(before, update, problems)` sees the intended publication first. Nothing is sent on a
    dry run, when nothing changes, or when a listed model's price would change (refused, naming
    them). Returns a summary of what is live."""
    reply, now = call('commercial-config')
    before = reply['config']
    newer_server(before)
    stamp = now + ACTIVATION_DELAY_SECS
    update, problems, proof = plan(before, POLICY, now, stamp)
    record(before, update, problems)
    if problems:
        raise PreconditionFailed('The pricing policy would change these prices; nothing was '
                                 'published: ' + '; '.join(problems))
    if update is None:
        return {'status': 'unchanged', 'revision': before['revision'], **proof}
    models = sorted(version['model'] for version in update['versions'])
    if dry_run:
        return {'status': 'dry-run', 'revision': before['revision'], 'models': models,
                'settings': 'settings' in update, **proof}
    call('commercial-config', update)
    reply, now = call('commercial-config')
    after = reply['config']
    live = {version['id']: version for version in after['versions']}
    if (any(live.get(version['id']) != version for version in update['versions'])
            or not same_settings(after['settings'], update.get('settings', {}))):
        raise RuntimeError('Readback differs from the intended publication')
    wait(max(0, stamp - now + 1))
    reply, now = call('commercial-config')
    if now < stamp:
        raise RuntimeError('Activation not yet reached')
    current = reply['config']
    for version in update['versions']:
        if in_force(current['versions'], version['rate_card_id'], [version['model']], now) != version:
            raise RuntimeError(f"Unexpected price in force for {version['model']}")
    return {'status': 'active', 'effective_from_secs': stamp, 'revision': current['revision'],
            'models': models, **proof}


def main():
    dry_run = sys.argv[1:] == ['--dry-run']
    if sys.argv[1:] and not dry_run:
        raise SystemExit('usage: migrate_official_pricing.py [--dry-run] < credentials.json')
    credentials = json.load(sys.stdin)
    ssh = pinned_connection(credentials)
    try:
        # A dry run changes nothing, so it neither needs the deployment lock nor keeps it.
        with contextlib.nullcontext() if dry_run else deployment_lock(ssh):
            with ssh.open_sftp() as sftp:
                access = json.loads(sftp.open('/etc/kiro-byok/admin-access.json').read())
            with requests.Session() as http:
                http.trust_env = False
                headers = {'Origin': ORIGIN}

                def call(path, body=None):
                    response = http.request('GET' if body is None else 'POST',
                                            ORIGIN + '/api/v1/admin/' + path, json=body,
                                            headers=headers, timeout=45, allow_redirects=False)
                    if response.status_code in (400, 401, 403, 409):
                        # Refused: nothing changed.
                        raise PreconditionFailed(
                            f"{path} refused: {str(response.json().get('error', ''))[:300]}")
                    if response.status_code != 200:
                        raise RuntimeError(f'{path}: HTTP {response.status_code}')
                    server_now = email.utils.parsedate_to_datetime(response.headers['Date'])
                    return response.json(), int(server_now.timestamp())

                call('session', {'username': access['username'], 'password': access['password']})
                headers['X-CSRF-Token'] = call('session')[0]['csrfToken']
                stamp = int(time.time())

                def record(before, update, problems):
                    OUT.mkdir(exist_ok=True)
                    (OUT / 'official-pricing-before.json').write_text(
                        json.dumps(before, ensure_ascii=False, indent=2), encoding='utf8')
                    (OUT / 'official-pricing-publication.json').write_text(
                        json.dumps({'update': update, 'problems': problems}, ensure_ascii=False,
                                   indent=2), encoding='utf8')
                    if update is not None and not problems and not dry_run:
                        # Rollback evidence on the server before a single mutation is sent.
                        backup = f'/opt/kiro-byok/official-pricing-{stamp}'
                        write_remote(ssh, backup + '-before.json', json.dumps(before).encode())
                        write_remote(ssh, backup + '-policy.json', json.dumps(POLICY).encode())
                        write_remote(ssh, backup + '-publication.json', json.dumps(update).encode())

                try:
                    summary = migrate(call, record, dry_run)
                finally:
                    try:
                        http.post(ORIGIN + '/api/v1/admin/session/revoke', json={},
                                  headers=headers, timeout=15)
                    except Exception:
                        pass
        print(json.dumps(summary, ensure_ascii=False), flush=True)
        (OUT / f"official-pricing-{summary['status']}.json").write_text(
            json.dumps(summary, ensure_ascii=False, indent=2), encoding='utf8')
    finally:
        ssh.close()


if __name__ == '__main__':
    main()

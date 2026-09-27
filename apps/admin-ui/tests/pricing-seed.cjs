// Test-only: the fixture's configuration as production has it once prices come from official ones
// (deploy/migrate_official_pricing.py, plus an official price table): ¥0.03 a credit, ¥1 per
// official $1, 计费倍率 0.24, provider 成本倍率, each fixture model's price in force computed from its
// official price (gemini-pro left on a typed legacy price), and a promotion scheduled for
// claude-sonnet. Versions are built with the console's own formula module.
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require('typescript');
function load(file, imports = {}) {
  const exports = {};
  const code = ts.transpileModule(fs.readFileSync(path.join(__dirname, '../src', file), 'utf8'), {
    compilerOptions: {module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020},
  }).outputText;
  vm.runInNewContext(code, {exports, require: name => imports[name], TextEncoder});
  return exports;
}
const pricing = load('pricing.ts');
const change = load('priceChange.ts', {'./pricing': pricing});
const routes = load('routes.ts');
const official = load('officialPricing.ts', {'./priceChange': change, './routes': routes});
const plain = value => JSON.parse(JSON.stringify(value));

const OFFICIAL = {'claude-sonnet': [3, 15, 3.75, 0.3], 'gpt-5': [4, 20, 5, 0.4], 'gemini-pro': [2, 12, 2.5, 0.2], 'gpt-6-astra': [10, 50, 12.5, 1]};
const entry = usd => ({input_usd_per_m: usd[0], output_usd_per_m: usd[1], cache_creation_usd_per_m: usd[2], cache_read_usd_per_m: usd[3], note: '官网 9/20', updated_at_secs: 1789790400});

/** Seeds `fixture` (tests/fixture-api.cjs) in place; returns the IDs a test looks for. */
module.exports = function seedOfficialPricing(fixture, {face = 0.03} = {}) {
  const config = fixture.config, now = Math.floor(Date.now() / 1000);
  config.settings = {credit_face_value_cny: face, usd_cny_rate: 7.25, rate_updated_at_secs: now - 86400, official_usd_cny: 1, default_price_multiplier: 0.24, default_cost_multiplier: 0.08,
    provider_cost_multipliers: {'fixture-provider': 0.08, 'fixture-openai': 0.06},
    official_prices: Object.fromEntries(Object.entries(OFFICIAL).map(([name, usd]) => [name, entry(usd)]))};
  const version = (model, usd, costMultiplier, from, id, priceMultiplier = 0.24) => plain(official.officialVersion({official: usd, priceMultiplier, costMultiplier, basis: null, usdCny: 1, face},
    {id, rateCardId: 'fixture-rate', model, effectiveSecs: from}));
  const promo = `claude-sonnet-promo-${now}`;
  config.versions = [...config.versions,
    version('claude-sonnet', OFFICIAL['claude-sonnet'], 0.08, now - 3600, 'claude-sonnet-official'),
    version('gpt-5', OFFICIAL['gpt-5'], 0.08, now - 3600, 'gpt-5-official'),
    version('gpt-6-astra', OFFICIAL['gpt-6-astra'], 0.06, now - 3600, 'gpt-6-astra-official'),
    version('claude-sonnet', OFFICIAL['claude-sonnet'], 0.08, now + 86400, promo, 0.2)];
  config.revision = `fixture-rev-${Number(config.revision.split('-').pop()) + 1}`;
  return {promo, official, plain};
};
module.exports.OFFICIAL = OFFICIAL;

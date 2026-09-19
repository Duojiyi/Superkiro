-- ==============================================================================
-- Kiro BYOK Core Schema (Postgres 14+)
-- Spec §5 (Data Model) & Spec §6 (Billing & Metering)
-- ==============================================================================

-- 1. Tenants & Groups
CREATE TABLE IF NOT EXISTS groups (
    id VARCHAR(64) PRIMARY KEY,
    name VARCHAR(128) NOT NULL,
    provider_binding_mode VARCHAR(32) NOT NULL DEFAULT 'shared', -- 'shared' | 'dedicated'
    rate_card_id VARCHAR(64) NOT NULL DEFAULT 'default',
    margin_multiplier DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    virtual_plan_name VARCHAR(64) NOT NULL DEFAULT 'KIRO PRO+',
    virtual_usage_limit DOUBLE PRECISION NOT NULL DEFAULT 50000.0,
    system_prompt_prefix TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 2. Card Templates & Cards (Identity is Card, 2-tier tenancy)
CREATE TABLE IF NOT EXISTS card_templates (
    id VARCHAR(64) PRIMARY KEY,
    name VARCHAR(128) NOT NULL,
    duration_seconds BIGINT NOT NULL, -- e.g. 30 days = 2592000
    credit_total BIGINT NOT NULL, -- Integer micro-credits (1 credit = 1_000_000)
    max_devices INT NOT NULL DEFAULT 1 CHECK (max_devices = 1),
    max_concurrency INT NOT NULL DEFAULT 5,
    daily_credit_limit BIGINT,
    monthly_credit_limit BIGINT,
    group_id VARCHAR(64) NOT NULL REFERENCES groups(id)
);

CREATE TABLE IF NOT EXISTS cards (
    id VARCHAR(64) PRIMARY KEY,
    code_hash VARCHAR(128) NOT NULL UNIQUE, -- SHA256 of card activation code
    template_id VARCHAR(64) REFERENCES card_templates(id),
    group_id VARCHAR(64) NOT NULL REFERENCES groups(id),
    credit_total BIGINT NOT NULL, -- Micro-credits
    issued_credits BIGINT, -- Immutable entitlement; NULL for unresolved legacy records
    credit_used BIGINT NOT NULL DEFAULT 0,
    credit_reserved BIGINT NOT NULL DEFAULT 0,
    status VARCHAR(32) NOT NULL DEFAULT 'unactivated', -- 'unactivated' | 'active' | 'frozen' | 'banned' | 'expired' | 'voided'
    activated_at TIMESTAMPTZ, -- Set on first login
    valid_until TIMESTAMPTZ, -- activated_at + template.duration_seconds
    max_devices INT NOT NULL DEFAULT 1 CHECK (max_devices = 1),
    rebind_count INT NOT NULL DEFAULT 0,
    max_concurrency INT NOT NULL DEFAULT 2, -- Card-level upstream concurrency quota (Spec §14.9)
    daily_credit_limit BIGINT, -- Daily micro-credits limit
    monthly_credit_limit BIGINT, -- Monthly micro-credits limit
    token_version BIGINT NOT NULL DEFAULT 1, -- For instant token revocation (Spec §7)
    note TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_cards_group_id ON cards(group_id);
CREATE INDEX IF NOT EXISTS idx_cards_status ON cards(status);

-- 3. Providers & Multi-Key Pools (Spec §5, §14.3)
CREATE TABLE IF NOT EXISTS providers (
    id VARCHAR(64) PRIMARY KEY,
    name VARCHAR(128) NOT NULL,
    format VARCHAR(32) NOT NULL DEFAULT 'openai', -- 'openai' | 'anthropic'
    base_url VARCHAR(512) NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    weight INT NOT NULL DEFAULT 1,
    health_state VARCHAR(32) NOT NULL DEFAULT 'healthy', -- 'healthy' | 'degraded' | 'unhealthy'
    cooldown_until TIMESTAMPTZ,
    group_id VARCHAR(64) REFERENCES groups(id), -- NULL = shared pool, non-NULL = dedicated to group
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_providers_group_id ON providers(group_id);

CREATE TABLE IF NOT EXISTS provider_keys (
    id VARCHAR(64) PRIMARY KEY,
    provider_id VARCHAR(64) NOT NULL REFERENCES providers(id),
    api_key_encrypted TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    weight INT NOT NULL DEFAULT 1,
    health_state VARCHAR(32) NOT NULL DEFAULT 'healthy', -- 'healthy' | 'degraded' | 'unhealthy'
    cooldown_until TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_provider_keys_provider_id ON provider_keys(provider_id);

-- 4. Model Mapping per Group
CREATE TABLE IF NOT EXISTS model_maps (
    id VARCHAR(64) PRIMARY KEY,
    group_id VARCHAR(64) NOT NULL REFERENCES groups(id),
    exposed_model_id VARCHAR(64) NOT NULL,
    target_provider_id VARCHAR(64) NOT NULL,
    target_model VARCHAR(64) NOT NULL,
    context_window INT NOT NULL DEFAULT 200000,
    max_output INT NOT NULL DEFAULT 64000,
    supports_tools BOOLEAN NOT NULL DEFAULT TRUE,
    supports_vision BOOLEAN NOT NULL DEFAULT TRUE,
    supports_reasoning BOOLEAN NOT NULL DEFAULT FALSE,
    credit_multiplier DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    visible BOOLEAN NOT NULL DEFAULT TRUE,
    sort_order INT NOT NULL DEFAULT 0,
    UNIQUE(group_id, exposed_model_id)
);

-- 3b. Global Settings (Spec §5, §14.10.1, §14.10.5)
CREATE TABLE IF NOT EXISTS settings (
    id VARCHAR(64) PRIMARY KEY DEFAULT 'global',
    credit_face_value_cny DOUBLE PRECISION NOT NULL DEFAULT 0.01, -- 1 credit = 0.01 CNY (1分钱)
    usd_cny_rate DOUBLE PRECISION NOT NULL DEFAULT 7.25,
    rate_updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 4. Rate Cards & Pricing Versions
CREATE TABLE IF NOT EXISTS rate_cards (
    id VARCHAR(64) PRIMARY KEY,
    name VARCHAR(128) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS rate_card_versions (
    id VARCHAR(64) PRIMARY KEY,
    rate_card_id VARCHAR(64) NOT NULL REFERENCES rate_cards(id),
    model VARCHAR(64) NOT NULL,
    currency VARCHAR(16) NOT NULL DEFAULT 'USD', -- 'USD' | 'CNY'
    pricing_mode VARCHAR(32) NOT NULL DEFAULT 'cost_plus', -- 'cost_plus' | 'fixed' | 'per_call'
    input_price_per_m DOUBLE PRECISION NOT NULL DEFAULT 3.0,
    output_price_per_m DOUBLE PRECISION NOT NULL DEFAULT 15.0,
    cache_creation_price_per_m DOUBLE PRECISION NOT NULL DEFAULT 3.75,
    cache_read_price_per_m DOUBLE PRECISION NOT NULL DEFAULT 0.30,
    fixed_input_credit_per_m BIGINT NOT NULL DEFAULT 0,
    fixed_output_credit_per_m BIGINT NOT NULL DEFAULT 0,
    fixed_cache_creation_credit_per_m BIGINT NOT NULL DEFAULT 0,
    fixed_cache_read_credit_per_m BIGINT NOT NULL DEFAULT 0,
    per_call_credit BIGINT NOT NULL DEFAULT 0,
    margin_multiplier DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    effective_from TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 5. Credit Reservation (Concurrency protection, Spec §6.2)
CREATE TABLE IF NOT EXISTS credit_reservations (
    id VARCHAR(64) PRIMARY KEY,
    card_id VARCHAR(64) NOT NULL REFERENCES cards(id),
    invocation_id VARCHAR(64) NOT NULL UNIQUE, -- amz-sdk-invocation-id (Spec §4.7)
    reserved_micro_credits BIGINT NOT NULL,
    state VARCHAR(32) NOT NULL DEFAULT 'held', -- 'held' | 'settled' | 'released'
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL -- For orphan reservation janitor (Spec §6.2)
);

CREATE INDEX IF NOT EXISTS idx_reservations_card_state ON credit_reservations(card_id, state);
CREATE INDEX IF NOT EXISTS idx_reservations_expires_at ON credit_reservations(expires_at) WHERE state = 'held';

-- 6. Usage Ledger (Append-only authoritative billing, Spec §5 & §6.1)
CREATE TABLE IF NOT EXISTS usage_ledger (
    id VARCHAR(64) PRIMARY KEY,
    card_id VARCHAR(64) NOT NULL REFERENCES cards(id),
    ts TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    kind VARCHAR(32) NOT NULL DEFAULT 'usage', -- 'usage' | 'adjustment' | 'topup'
    invocation_id VARCHAR(64), -- Unique constraint for kind = 'usage' to prevent double charging
    exposed_model VARCHAR(64) NOT NULL,
    provider_id VARCHAR(64) NOT NULL,
    target_model VARCHAR(64) NOT NULL,
    input_tokens BIGINT NOT NULL DEFAULT 0,
    output_tokens BIGINT NOT NULL DEFAULT 0,
    cache_creation_tokens BIGINT NOT NULL DEFAULT 0,
    cache_read_tokens BIGINT NOT NULL DEFAULT 0,
    credits_charged BIGINT NOT NULL, -- Micro-credits
    provider_cost_micro_cny BIGINT NOT NULL DEFAULT 0,
    rate_card_version VARCHAR(64),
    request_id VARCHAR(128),
    operator_id VARCHAR(64),
    reason TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_usage_ledger_invocation ON usage_ledger(invocation_id) WHERE kind = 'usage';
CREATE INDEX IF NOT EXISTS idx_usage_ledger_card_ts ON usage_ledger(card_id, ts DESC);

-- 7. Request Traces (Performance & error metrics, NO conversation text, Spec §5 & §14.4)
CREATE TABLE IF NOT EXISTS request_traces (
    id VARCHAR(64) PRIMARY KEY,
    card_id VARCHAR(64) NOT NULL REFERENCES cards(id),
    ts TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    invocation_id VARCHAR(64) NOT NULL,
    exposed_model VARCHAR(64) NOT NULL,
    status VARCHAR(32) NOT NULL, -- 'success' | 'error' | 'client_aborted'
    ttft_ms INT,
    tokens_per_second DOUBLE PRECISION,
    error_class VARCHAR(64),
    provider_id VARCHAR(64),
    input_tokens BIGINT NOT NULL DEFAULT 0,
    output_tokens BIGINT NOT NULL DEFAULT 0,
    credits_charged BIGINT NOT NULL DEFAULT 0,
    provider_cost_micro_cny BIGINT NOT NULL DEFAULT 0,
    attempt_chain TEXT -- JSON serialized failover attempt history
);

CREATE INDEX IF NOT EXISTS idx_traces_card_ts ON request_traces(card_id, ts DESC);

-- 8. Platform Announcements & Degradation Notices (Spec §14.4)
CREATE TABLE IF NOT EXISTS announcements (
    id VARCHAR(64) PRIMARY KEY,
    title VARCHAR(256) NOT NULL,
    content TEXT NOT NULL,
    level VARCHAR(32) NOT NULL DEFAULT 'info', -- 'info' | 'warning' | 'critical'
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ
);

-- 9. Multi-Tenant Row Level Security (RLS) Policies (Spec §7)
-- Enables strict database-level isolation based on current session tenant context `app.current_group_id`.

ALTER TABLE cards ENABLE ROW LEVEL SECURITY;
ALTER TABLE usage_ledger ENABLE ROW LEVEL SECURITY;
ALTER TABLE credit_reservations ENABLE ROW LEVEL SECURITY;
ALTER TABLE model_maps ENABLE ROW LEVEL SECURITY;

-- Cards isolation: Tenant can only view/mutate cards belonging to their group
DO $$
BEGIN
    DROP POLICY IF EXISTS cards_tenant_isolation ON cards;
    CREATE POLICY cards_tenant_isolation ON cards
        FOR ALL
        USING (
            current_setting('app.is_admin', true) = 'true'
            OR (
                current_setting('app.current_group_id', true) IS NOT NULL
                AND current_setting('app.current_group_id', true) <> ''
                AND group_id = current_setting('app.current_group_id', true)
            )
        )
        WITH CHECK (
            current_setting('app.is_admin', true) = 'true'
            OR (
                current_setting('app.current_group_id', true) IS NOT NULL
                AND current_setting('app.current_group_id', true) <> ''
                AND group_id = current_setting('app.current_group_id', true)
            )
        );
END $$;

-- Ledger isolation: Tenant can only view ledger records of cards in their group
DO $$
BEGIN
    DROP POLICY IF EXISTS ledger_tenant_isolation ON usage_ledger;
    CREATE POLICY ledger_tenant_isolation ON usage_ledger
        FOR ALL
        USING (
            current_setting('app.is_admin', true) = 'true'
            OR (
                current_setting('app.current_group_id', true) IS NOT NULL
                AND current_setting('app.current_group_id', true) <> ''
                AND card_id IN (
                    SELECT id FROM cards WHERE group_id = current_setting('app.current_group_id', true)
                )
            )
        )
        WITH CHECK (
            current_setting('app.is_admin', true) = 'true'
            OR (
                current_setting('app.current_group_id', true) IS NOT NULL
                AND current_setting('app.current_group_id', true) <> ''
                AND card_id IN (
                    SELECT id FROM cards WHERE group_id = current_setting('app.current_group_id', true)
                )
            )
        );
END $$;

-- Model maps isolation: Tenant can only see model mappings assigned to their group
DO $$
BEGIN
    DROP POLICY IF EXISTS model_maps_tenant_isolation ON model_maps;
    CREATE POLICY model_maps_tenant_isolation ON model_maps
        FOR ALL
        USING (
            current_setting('app.is_admin', true) = 'true'
            OR (
                current_setting('app.current_group_id', true) IS NOT NULL
                AND current_setting('app.current_group_id', true) <> ''
                AND group_id = current_setting('app.current_group_id', true)
            )
        )
        WITH CHECK (
            current_setting('app.is_admin', true) = 'true'
            OR (
                current_setting('app.current_group_id', true) IS NOT NULL
                AND current_setting('app.current_group_id', true) <> ''
                AND group_id = current_setting('app.current_group_id', true)
            )
        );
END $$;

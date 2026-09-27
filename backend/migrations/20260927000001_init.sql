-- QuantDesk schema v1.
--
-- History tables are append-only (INV-16): a trigger rejects UPDATE and
-- DELETE, and TRUNCATE is rejected by a statement trigger. Mutable state
-- (accounts) is audited in the append-only audit_log.

CREATE FUNCTION qd_forbid_mutation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'table % is append-only (INV-16)', TG_TABLE_NAME
        USING ERRCODE = 'insufficient_privilege';
END;
$$;

-- Attaches the append-only triggers to a table.
CREATE FUNCTION qd_make_append_only(table_name text) RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
    EXECUTE format(
        'CREATE TRIGGER %I BEFORE UPDATE OR DELETE ON %I FOR EACH ROW EXECUTE FUNCTION qd_forbid_mutation()',
        table_name || '_append_only_row', table_name);
    EXECUTE format(
        'CREATE TRIGGER %I BEFORE TRUNCATE ON %I FOR EACH STATEMENT EXECUTE FUNCTION qd_forbid_mutation()',
        table_name || '_append_only_truncate', table_name);
END;
$$;

-- Decision Journal: decisions, order intents and events, fills, position events, halts.
CREATE TABLE journal (
    seq         bigserial PRIMARY KEY,
    kind        text        NOT NULL,
    entry       jsonb       NOT NULL,
    recorded_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX journal_kind_idx ON journal (kind, seq);
SELECT qd_make_append_only('journal');

-- Kill switch: every version of every halt (created, then cleared).
CREATE TABLE halt_events (
    seq         bigserial PRIMARY KEY,
    halt_id     uuid        NOT NULL,
    halt        jsonb       NOT NULL,
    recorded_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX halt_events_halt_idx ON halt_events (halt_id, seq);
SELECT qd_make_append_only('halt_events');

-- Instrument specs, versioned; a version never changes.
CREATE TABLE instrument_specs (
    instrument_id uuid        NOT NULL,
    version       integer     NOT NULL,
    symbol        text        NOT NULL,
    spec          jsonb       NOT NULL,
    recorded_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (instrument_id, version)
);
SELECT qd_make_append_only('instrument_specs');

-- Daily bars. Corrections are new rows with a later ingested_at; reads pick
-- the latest row ingested at or before the snapshot time (INV-09).
CREATE TABLE bars (
    instrument_id uuid          NOT NULL,
    trade_date    date          NOT NULL,
    ingested_at   timestamptz   NOT NULL,
    open          numeric(28, 10) NOT NULL,
    high          numeric(28, 10) NOT NULL,
    low           numeric(28, 10) NOT NULL,
    close         numeric(28, 10) NOT NULL,
    volume        numeric(38, 10) NOT NULL,
    PRIMARY KEY (instrument_id, trade_date, ingested_at),
    CHECK (low > 0 AND low <= open AND low <= close AND high >= open AND high >= close),
    CHECK (volume >= 0)
);
SELECT qd_make_append_only('bars');

-- Strategy versions: immutable parameters and logic (INV-10).
CREATE TABLE strategy_versions (
    version_id     uuid        PRIMARY KEY,
    strategy_id    uuid        NOT NULL,
    name           text        NOT NULL,
    version_number integer     NOT NULL,
    logic_version  text        NOT NULL,
    parameters     jsonb       NOT NULL,
    git_sha        text        NOT NULL,
    rr_floor       numeric(10, 4) NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    UNIQUE (strategy_id, version_number)
);
SELECT qd_make_append_only('strategy_versions');

-- Strategy stage transitions (INV-11, INV-16).
CREATE TABLE strategy_stage_events (
    seq         bigserial PRIMARY KEY,
    version_id  uuid        NOT NULL REFERENCES strategy_versions (version_id),
    event       jsonb       NOT NULL,
    stage       jsonb       NOT NULL,
    recorded_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX strategy_stage_events_version_idx ON strategy_stage_events (version_id, seq);
SELECT qd_make_append_only('strategy_stage_events');

-- Accounts: the only mutable table; every change is audited.
CREATE TABLE accounts (
    account_id uuid        PRIMARY KEY,
    name       text        NOT NULL,
    mode       text        NOT NULL CHECK (mode IN ('backtest', 'paper', 'live')),
    currency   text        NOT NULL,
    live_armed boolean     NOT NULL DEFAULT false,
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (mode = 'live' OR live_armed = false)
);

-- Audit log: who did what, when.
CREATE TABLE audit_log (
    seq         bigserial PRIMARY KEY,
    actor       text        NOT NULL,
    action      text        NOT NULL,
    detail      jsonb       NOT NULL,
    recorded_at timestamptz NOT NULL DEFAULT now()
);
SELECT qd_make_append_only('audit_log');

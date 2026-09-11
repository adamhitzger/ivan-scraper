-- Výchozí schéma. Přepis původního `init.sql`, který se pouštěl jen při vzniku
-- prázdného volume, takže běžící nasazení z něj žádnou změnu nedostalo.
--
-- Všechno je idempotentní (IF NOT EXISTS), protože tahle migrace poběží i nad
-- produkční databází, kde tabulky už existují — tam nesmí nic vytvořit, jen se
-- zapsat do `_sqlx_migrations`. Každá další změna schématu jde do nového souboru
-- přes `sqlx migrate add <nazev>` a aplikace ji spustí při startu.

CREATE TABLE IF NOT EXISTS provozovny (
    id SERIAL PRIMARY KEY,
    place_id TEXT NOT NULL UNIQUE,
    nazev TEXT NOT NULL,
    telefon TEXT,
    telefon_raw TEXT,
    web TEXT,
    adresa TEXT,
    mesto TEXT,
    psc TEXT,
    hodnoceni REAL,
    pocet_recenzi INTEGER,
    url TEXT,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    is_contacted BOOLEAN NOT NULL DEFAULT FALSE,
    is_closed BOOLEAN NOT NULL DEFAULT FALSE,
    -- Volná poznámka z detailu provozovny. Prostý text, žádný markdown ani HTML.
    poznamka TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS provozovny_emaily (
    id SERIAL PRIMARY KEY,
    provozovna_id INTEGER NOT NULL REFERENCES provozovny(id) ON DELETE CASCADE,
    email TEXT NOT NULL,
    UNIQUE (provozovna_id, email)
);

CREATE TABLE IF NOT EXISTS scraper_config (
    id SERIAL PRIMARY KEY,
    lokace TEXT NOT NULL DEFAULT 'Praha',
    vyraz1 TEXT,
    vyraz2 TEXT,
    vyraz3 TEXT,
    vyraz4 TEXT,
    vyraz5 TEXT,
    vyraz6 TEXT,
    vyraz7 TEXT,
    vyraz8 TEXT,
    vyraz9 TEXT,
    vyraz10 TEXT,
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

-- Sloupce, které do už existujících databází přibývaly ručně (`ALTER TABLE`
-- lokálně, `dorovnej_schema` při startu v produkci). Nad čerstvou databází
-- nic nedělají, nad starší je dorovnají.
ALTER TABLE provozovny ADD COLUMN IF NOT EXISTS poznamka TEXT NOT NULL DEFAULT '';
ALTER TABLE scraper_config ADD COLUMN IF NOT EXISTS vyraz6 TEXT;
ALTER TABLE scraper_config ADD COLUMN IF NOT EXISTS vyraz7 TEXT;
ALTER TABLE scraper_config ADD COLUMN IF NOT EXISTS vyraz8 TEXT;
ALTER TABLE scraper_config ADD COLUMN IF NOT EXISTS vyraz9 TEXT;
ALTER TABLE scraper_config ADD COLUMN IF NOT EXISTS vyraz10 TEXT;

CREATE TABLE provozovny (
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
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE TABLE provozovny_emaily (
    id SERIAL PRIMARY KEY,
    provozovna_id INTEGER NOT NULL REFERENCES provozovny(id) ON DELETE CASCADE,
    email TEXT NOT NULL,
    UNIQUE (provozovna_id, email)
);

CREATE TABLE scraper_config (
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
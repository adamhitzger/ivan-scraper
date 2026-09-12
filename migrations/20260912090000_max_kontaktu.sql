-- Kolik míst má Apify actor stáhnout na jeden hledaný výraz
-- (`maxCrawledPlacesPerSearch`). Dřív bylo natvrdo 30 v kódu, teď se
-- nastavuje na stránce Nastavení.
ALTER TABLE scraper_config
    ADD COLUMN IF NOT EXISTS max_kontaktu INTEGER NOT NULL DEFAULT 30;

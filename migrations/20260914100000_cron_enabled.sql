-- Vypínač denního cron jobu (stránka Nastavení). Výchozí TRUE, aby se
-- běžící nasazení po migraci chovalo stejně jako dosud.
ALTER TABLE scraper_config
    ADD COLUMN IF NOT EXISTS cron_enabled BOOLEAN NOT NULL DEFAULT TRUE;

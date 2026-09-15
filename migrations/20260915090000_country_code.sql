-- Kód země z Apify (`countryCode`, např. "PL"). Starší záznamy ho nemají,
-- doplní se až při dalším scrapu (ON CONFLICT větev v insert_place).
ALTER TABLE provozovny ADD COLUMN IF NOT EXISTS country_code TEXT;

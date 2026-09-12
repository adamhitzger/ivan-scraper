Příkazy

0. Github push
 - git add .
 - git commit -m "nazev_commitu"
 - git push -u origin master

1. Update verze
 - just code
    - docker compose up -d --build
 - code with queries (schéma se nemění)
    - docker compose up -d
    - cargo sqlx prepare
    - docker compose up -d --build
 - změna schématu (nový sloupec, tabulka…)
    - sqlx migrate add nazev_zmeny        >> vytvoří migrations/<timestamp>_nazev_zmeny.sql, napsat do něj SQL
    - docker compose up -d                >> běží postgres na localhost:5432
    - sqlx migrate run                    >> aplikuje migraci do lokální DB (čte DATABASE_URL z .env)
    - code with queries
    - cargo sqlx prepare                  >> .sqlx cache proti už zmigrované DB
    - docker compose up -d --build
    - git add migrations/ .sqlx/          >> obojí musí do commitu, jinak build v Actions spadne
 - už aplikovanou migraci nikdy neupravovat (sqlx hlídá checksum) — vždy nový soubor
 - produkce: nic ručně, app si migrace pustí sama při startu nového image
 - stav migrací: sqlx migrate info

2. GHCR push&deploy
 - Login
    - docker login ghcr.io -u adamhitzger --password-stdin >> zadat PAT z .env
 - Logout
    - docker logout ghcr.io
 - Push
    - docker buildx build --platform linux/amd64 -t ghcr.io/adamhitzger/ivan-scraper:latest --push .
 - Pull
    - docker pull ghcr.io/adamhitzger/ivan-scraper:latest
 - Stáří běžícího image
    - docker images ghcr.io/adamhitzger/ivan-scraper --format '{{.Tag}} {{.CreatedSince}} {{.ID}}'
 - Remove 
    - docker image rm ghcr.io/adamhitzger/ivan-scraper:<tag>
 
 

3. PostgreSQL
 - Sign in
    - Docker local
        - docker exec -it ivan-scraper-postgres-1 psql -U postgres -d scraper_db
    - psql installed
        - psql [DATABASE_URL]
 - Schema tabulek
    - \dt
 - Datove typy tabulky
    - \d [tablename]
 - Seznam databází
    - \l 
 - Konec
    - \q

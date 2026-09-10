Příkazy

1. Update verze
- just code
    - docker compose up -d --build
 - code with queries
    - docker compose up -d
    - cargo sqlx prepare
    - docker compose --build

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

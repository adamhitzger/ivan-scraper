# Deploy na Coolify

## 1. Swap na VPS (udělat před prvním buildem)

Release build Rustu je paměťově náročný. Na stroji s 2 GB RAM linker běžně
skončí na OOM a Coolify to ohlásí jako neúspěšný deploy — často bez zjevné
chyby, jen `signal: 9` nebo useknutý log.

Zkontroluj, co máš:

```bash
free -h
swapon --show
```

Pokud je swap prázdný, přidej 4 GB (jednorázově, přežije restart):

```bash
sudo fallocate -l 4G /swapfile
sudo chmod 600 /swapfile
sudo mkswap /swapfile
sudo swapon /swapfile
echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
```

Ověření:

```bash
free -h        # ve sloupci Swap má být 4,0Gi
```

Na SSD je swap dost pomalý, ale pro build to stačí — jde jen o to,
aby linker měl kam sáhnout ve špičce.

### Alternativa: build mimo VPS (GHCR)

Workflow `.github/workflows/docker.yml` staví image v GitHub Actions a pushne
ho do GHCR. VPS pak jen stahuje hotový image — swap ani nepotřebuješ.

Nastavení:

1. **Push do repa.** Workflow běží na `master` a jde spustit i ručně
   (Actions → workflow_dispatch). Autentizace jde přes `GITHUB_TOKEN`, žádný
   PAT není potřeba — stačí `permissions: packages: write`, které workflow má.
2. **Zkontroluj viditelnost package.** GHCR balíčky jsou po prvním pushi
   privátní: GitHub → tvůj profil → Packages → `ivan-scraper` → Package settings.
   - *Public* → Coolify stáhne bez přihlášení, nic dalšího neřešíš.
   - *Private* → v Coolify přidej registry credentials (Keys & Tokens →
     Docker registries): `ghcr.io`, uživatel = tvůj GitHub login, heslo =
     PAT (classic) s právem `read:packages`.
3. **V `docker-compose.ghcr.yml` doplň `OWNER`** — svého GitHub uživatele nebo
   organizaci, malými písmeny.
4. **V Coolify** nastav u resource "Docker Compose Location" na
   `docker-compose.ghcr.yml`. Ten místo `build:` používá `image:`, takže se na
   serveru nic nekompiluje.
5. **Volitelně auto-deploy:** v Coolify u resource → Webhooks zkopíruj deploy
   webhook URL a token, ulož je do GitHub secrets jako `COOLIFY_WEBHOOK_URL`
   a `COOLIFY_TOKEN`. Poslední krok workflow pak po pushi image sám spustí
   deploy. Bez těch secrets se krok přeskočí.

Image se tagují `latest` a `sha-<commit>`, takže se dá vrátit na konkrétní
commit. Staví se jen `linux/amd64`. Build cache jede přes GitHub Actions cache,
což s cargo-chef vrstvou drží běžný build v jednotkách minut.

**Pozor:** `.sqlx/` musí být commitnutá, jinak build v CI spadne — v runneru
žádná databáze neběží a `SQLX_OFFLINE=true` je v Dockerfile natvrdo.

### Ruční push z lokálu

Jen když nechceš čekat na CI. Mac je arm64, VPS amd64 — bez `--platform` ti
tam image nepoběží:

```bash
echo $GITHUB_PAT | docker login ghcr.io -u <tvůj-github-login> --password-stdin
docker buildx build --platform linux/amd64 \
  -t ghcr.io/<owner>/ivan-scraper:latest --push .
```

PAT (classic) potřebuje `write:packages`.

## 2. Proměnné prostředí v Coolify

`.env` je v `.gitignore`, takže na server nedojede — všechno se zadává v UI
(Coolify z toho pro compose deploy vygeneruje `.env` sám).

| Proměnná | Poznámka |
| --- | --- |
| `HOST` | `0.0.0.0` |
| `PORT` | `3000` |
| `LOGIN_KOD` | přihlašovací kód do UI |
| `APIFY_API_TOKEN` | |
| `GOOGLE_MAPS_SCRAPER_TOKEN` | ID actoru |
| `POSTGRES_PASSWORD` | **vygeneruj nové**, ne to z lokálního `.env` |
| `SMTP_HOST` | `smtp.gmail.com` |
| `SMTP_USER` | |
| `SMTP_PASSWORD` | Gmail **App Password**, ne heslo k účtu |

`DATABASE_URL` **nenastavuj** — skládá se v `docker-compose.yml` z
`POSTGRES_PASSWORD` a interního hostname `postgres`.

`SQLX_OFFLINE` taky ne — je natvrdo v Dockerfile.

## 3. Po deployi

- Doménu nastav v Coolify UI u služby `ivan-scraper` (port 3000).
- V Apify konzoli přepiš URL webhooku na `https://<doména>/apify/webhook`.
- Ověř `https://<doména>/health` → `{"status":"ok"}`.
- Zkontroluj, že kontejner je `healthy` (`docker ps`), ne jen `running`.

## Poznámky k provozu

- `init.sql` se pouští **jen při vytvoření prázdného volume**. Změny schématu
  po prvním spuštění je potřeba pustit ručně, nebo přejít na `sqlx migrate`.
- Sessions se drží v paměti — každý redeploy odhlásí přihlášené uživatele.
- Cron běží denně v 6:00 pražského času, `maxCrawledPlacesPerSearch` je 20.

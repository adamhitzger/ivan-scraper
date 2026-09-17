use rust_xlsxwriter::{Workbook, Format, XlsxError};
use axum::{
    Form, extract::{
        Json, Path, Query, State, Request
    }, http::{header, StatusCode}, response::{Html, IntoResponse, Redirect, Response as AxumResponse},
    middleware::Next
};
use tokio_cron_scheduler::Job;
use chrono_tz::Europe::Prague;
use tracing::{info, warn,error};
use std::{collections::{BTreeSet, HashMap, HashSet}, error::Error, sync::LazyLock, time::{Duration, Instant}};
use tower_sessions::Session;
use serde_json::{
    Value,
    json
};
use anyhow::Result;
use reqwest::{
    header::HeaderMap,
    RequestBuilder,
    Response,
    Method,
};
use sqlx::PgPool;
use crate::{AppState, types::{ApifyWebhook, Place, Provozovna, ProvozovnaExport}};
use tera::{
    Context, context
};
use lettre::{Message, AsyncTransport};
use lettre::message::header::ContentType;

pub async fn send_kontakty_email(
    state: &AppState,
    nove_kontakty: Vec<&Place>,
    recipient: &str,
) -> anyhow::Result<()> {
    let pocet_novych = nove_kontakty.len();

    let mut context = tera::Context::new();
    context.insert("kontakty", &nove_kontakty);

    let html = state.tera.render("email_kontakty.html", &context)?;

    info!(prijemce = recipient, pocet_novych, "Odesílám e-mail s novými kontakty");

    let email = Message::builder()
        .from("ikt2stupen@gmail.com".parse()?)
        .to(recipient.parse()?)
        .subject(format!("Nové kontakty — {} firem", nove_kontakty.len()))
        .header(ContentType::TEXT_HTML)
        .body(html)?;

    match state.smtp.send(email).await{ 
        Ok(odpoved) => {
            let hlaska: String = odpoved.message().collect::<Vec<_>>().join(" ");
            if odpoved.is_positive() {
                info!(
                    prijemce = recipient,
                    kod = %odpoved.code(),
                    hlaska,
                    "E-mail přijat SMTP serverem"
                );
            } else {
                // 3xx/4xx bez Err — vzácné, ale ať to není tiché.
                warn!(prijemce = recipient, kod = %odpoved.code(), hlaska, "SMTP server e-mail nepotvrdil");
            }
            Ok(())
        }
        Err(e) => {
            error!(prijemce = recipient, chyba = %e, "Odeslání e-mailu selhalo");
            Err(e.into())
        }
    }
}

/// Percent-encoding hodnoty do query stringu. Vlastní, ať kvůli pár řádkům
/// nepřibývá závislost — projde jen nevyhrazené znaky z RFC 3986.
fn enkoduj(hodnota: &str) -> String {
    let mut out = String::with_capacity(hodnota.len());

    for b in hodnota.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }

    out
}

/// Stav výpisu držený v query parametrech (`?page=&search=&stav=&zeme=`).
/// Posílá se dál každým odkazem i serverovou akcí, aby stránka ani filtry
/// nezmizely po přechodu na detail nebo po POSTu.
#[derive(Debug, Clone)]
pub struct Filtry {
    pub page: i64,
    /// Město; do SQL jde jako ILIKE vzor, prázdné = bez filtru.
    pub search: String,
    /// `kontaktovane` | `smluvene` | prázdné.
    pub stav: String,
    /// Kód země z Apify (`PL`, `CZ`, …), prázdné = bez filtru.
    pub zeme: String,
}

/// Kód země smí být jen dvě velká ASCII písmena — cokoli jiného se zahodí,
/// stejně jako u `stav`. Sdílí to výpis i export.
fn normalizuj_zemi(hodnota: Option<&str>) -> String {
    match hodnota.map(str::trim) {
        Some(z) if z.len() == 2 && z.bytes().all(|b| b.is_ascii_uppercase()) => z.to_string(),
        _ => String::new(),
    }
}

impl Filtry {
    pub fn z_params(params: &HashMap<String, String>) -> Self {
        let page = params
            .get("page")
            .and_then(|p| p.parse::<i64>().ok())
            .filter(|p| *p >= 1)
            .unwrap_or(1);

        let search = params.get("search").cloned().unwrap_or_default();

        // Cokoli jiného než známé hodnoty = bez filtru, ať se do SQL
        // nedostane nic neočekávaného.
        let stav = match params.get("stav").map(String::as_str) {
            Some(s @ ("kontaktovane" | "smluvene")) => s.to_string(),
            _ => String::new(),
        };

        let zeme = normalizuj_zemi(params.get("zeme").map(String::as_str));

        Self { page, search, stav, zeme }
    }

    /// `&search=…&stav=…`; `page` se do odkazů doplňuje zvlášť, protože se
    /// u stránkování mění.
    pub fn suffix(&self) -> String {
        let mut qs = String::new();

        if !self.search.is_empty() {
            qs.push_str(&format!("&search={}", enkoduj(&self.search)));
        }

        if !self.stav.is_empty() {
            qs.push_str(&format!("&stav={}", enkoduj(&self.stav)));
        }

        if !self.zeme.is_empty() {
            qs.push_str(&format!("&zeme={}", enkoduj(&self.zeme)));
        }

        qs
    }

    /// Odkaz na výpis se stejným filtrem i stránkou.
    pub fn url(&self) -> String {
        format!("/?page={}{}", self.page, self.suffix())
    }

    /// Odkaz na detail provozovny, filtry s sebou (kvůli odkazu „zpět“).
    pub fn url_detailu(&self, id: i32) -> String {
        format!("/provozovna/{}?page={}{}", id, self.page, self.suffix())
    }

    /// Jiné město, stav zůstává. Stránkování se resetuje — na páté stránce
    /// jiného města by uživatel často skončil v prázdnu.
    fn s_mestem(&self, mesto: &str) -> Self {
        Self { page: 1, search: mesto.to_string(), ..self.clone() }
    }

    /// Jiný stav, město zůstává. Stránkování se resetuje ze stejného důvodu.
    fn se_stavem(&self, stav: &str) -> Self {
        Self { page: 1, stav: stav.to_string(), ..self.clone() }
    }

    /// Jiná země. Město se resetuje — seznam měst se zemí filtruje, takže by
    /// zvolené město v jiné zemi dávalo prázdný výpis.
    fn se_zemi(&self, zeme: &str) -> Self {
        Self { page: 1, search: String::new(), zeme: zeme.to_string(), ..self.clone() }
    }
}

/// Pilulka filtru (města i stavu) předpřipravená pro šablonu. Odkazy se
/// skládají tady, protože Tera 2 přesunula `urlencode` do tera-contrib.
#[derive(serde::Serialize)]
struct Pilulka {
    popisek: String,
    url: String,
    aktivni: bool,
}

/// Kam se po serverové akci vrátit. Bere se z formuláře, takže se ověřuje:
/// musí to být relativní cesta na tenhle web, ne cizí adresa ani hlavička
/// rozbitá novým řádkem.
fn bezpecny_navrat(zpet: Option<&str>) -> String {
    let vychozi = "/".to_string();

    let Some(cil) = zpet else { return vychozi };

    let vypada_relativne = cil.starts_with('/')
        && !cil.starts_with("//")
        && !cil.starts_with("/\\")
        && !cil.contains(|c: char| c.is_control());

    if vypada_relativne { cil.to_string() } else { vychozi }
}

pub async fn html_page(State(state): State<AppState>, session: Session, params: Query<HashMap<String,String>>) -> impl IntoResponse{
    let filtry = Filtry::z_params(&params);

    // Hromadný výběr žije v session napříč stránkami i filtry. Průběžně se čistí
    // o id, která už v DB nejsou (smazání po jednom), aby „Vybráno N" nelhalo.
    let vybrane = procisti_vyber(&state.pool, &session).await;

    let limit: i64 = 10;
    let offset: i64 = (filtry.page - 1) * limit;
    let search_pattern = format!("%{}%", filtry.search);

    let kontakty = sqlx::query_as!(
    Provozovna,
    r#"
    -- Sloupce se vypisují ručně: query_as! je mapuje na pole struktury podle pořadí,
    -- takže `p.*` by při jiném fyzickém rozložení tabulky posunulo hodnoty a dekódování
    -- by spadlo panikou uvnitř bytes místo čitelné chyby.
    -- `AS "x: _"` u timestampů: sqlx-store zapíná feature `sqlx/time` a makro by
    -- jinak sáhlo po time::OffsetDateTime místo chrono podle pole struktury.
    SELECT
        p.id, p.place_id, p.nazev, p.telefon, p.telefon_raw, p.web, p.adresa, p.mesto,
        p.psc, p.hodnoceni, p.pocet_recenzi, p.url, p.country_code,
        p.created_at AS "created_at: _", p.updated_at AS "updated_at: _",
        array_remove(array_agg(e.email), NULL) AS emaily,
        p.is_contacted, p.is_closed, p.poznamka
    FROM provozovny p
    LEFT JOIN provozovny_emaily e ON e.provozovna_id = p.id
    WHERE ($3 = '' OR p.mesto ILIKE $3)
      AND ($4 = ''
           OR ($4 = 'kontaktovane' AND p.is_contacted)
           OR ($4 = 'smluvene' AND p.is_closed))
      AND ($5 = '' OR p.country_code = $5)
    GROUP BY p.id
    ORDER BY p.updated_at DESC
    LIMIT $1 OFFSET $2
    "#,
        limit,
        offset,
        search_pattern,
        filtry.stav,
        filtry.zeme
    )
    .fetch_all(&state.pool)
    .await;

    match &kontakty {
        Ok(data) => println!("Načteno: {} záznamů", data.len()),
        Err(e) => eprintln!("DB error: {:?}", e),
    }

    let data: Vec<Provozovna> = kontakty.unwrap_or_default();

    // Města jen z vybrané země, ať pilulky nenabízejí kombinace s prázdným výsledkem.
    let mesta:Vec<String> = sqlx::query!(
        "SELECT DISTINCT mesto FROM provozovny WHERE mesto IS NOT NULL AND ($1 = '' OR country_code = $1) ORDER BY mesto",
        filtry.zeme
    )
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .filter_map(|r| r.mesto)
    .collect();

    // Země se berou z dat — starší záznamy bez country_code se objeví jen pod „Vše".
    let zeme: Vec<String> = sqlx::query_scalar!(
        "SELECT DISTINCT country_code FROM provozovny WHERE country_code IS NOT NULL ORDER BY country_code"
    )
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .flatten()
    .collect();

    let mut zeme_pilulky = vec![Pilulka {
        popisek: "Vše".to_string(),
        url: filtry.se_zemi("").url(),
        aktivni: filtry.zeme.is_empty(),
    }];

    zeme_pilulky.extend(zeme.iter().map(|z| Pilulka {
        popisek: z.clone(),
        url: filtry.se_zemi(z).url(),
        aktivni: filtry.zeme == *z,
    }));

    let mut mesta_pilulky = vec![Pilulka {
        popisek: "Vše".to_string(),
        url: filtry.s_mestem("").url(),
        aktivni: filtry.search.is_empty(),
    }];

    mesta_pilulky.extend(mesta.iter().map(|mesto| Pilulka {
        popisek: mesto.clone(),
        url: filtry.s_mestem(mesto).url(),
        aktivni: filtry.search == *mesto,
    }));

    let stavy_pilulky: Vec<Pilulka> = [("", "Všechny stavy"), ("kontaktovane", "Kontaktované"), ("smluvene", "Smluvené")]
        .iter()
        .map(|(klic, popisek)| Pilulka {
            popisek: popisek.to_string(),
            url: filtry.se_stavem(klic).url(),
            aktivni: filtry.stav == *klic,
        })
        .collect();

    // Stejný filtr jako u výpisu — jinak by čísla ve stránkování neseděla
    // s tím, co je v tabulce vidět.
    let total = sqlx::query_scalar!(
        r#"
    SELECT COUNT(*) FROM provozovny
    WHERE ($1 = '' OR mesto ILIKE $1)
      AND ($2 = ''
           OR ($2 = 'kontaktovane' AND is_contacted)
           OR ($2 = 'smluvene' AND is_closed))
      AND ($3 = '' OR country_code = $3)
    "#,
    search_pattern,
    filtry.stav,
    filtry.zeme
    )
    .fetch_one(&state.pool)
    .await.unwrap_or(Some(0))
    .unwrap_or(0);
    let total_pages: i64 = (total + limit -1) / limit;

    let s_emailem_raw: Result<Option<i64>, _> = sqlx::query_scalar!(
    "SELECT COUNT(DISTINCT provozovna_id)::bigint FROM provozovny_emaily"
    )
    .fetch_one(&state.pool)
    .await;

let s_emailem: i64 = s_emailem_raw.unwrap_or(Some(0)).unwrap_or(0);

let prumer_raw: Result<Option<f64>, _> = sqlx::query_scalar!(
    "SELECT ROUND(AVG(hodnoceni)::numeric, 1)::float8 FROM provozovny WHERE hodnoceni IS NOT NULL"
)
.fetch_one(&state.pool)
.await;

let prumer: f64 = prumer_raw.unwrap_or(Some(0.0)).unwrap_or(0.0);

let nove_dnes_raw: Result<Option<i64>, _> = sqlx::query_scalar!(
    "SELECT COUNT(*)::bigint FROM provozovny WHERE created_at::date = CURRENT_DATE"
)
.fetch_one(&state.pool)
.await;

let nove_dnes: i64 = nove_dnes_raw.unwrap_or(Some(0)).unwrap_or(0);

    let context: Context = context!{
        page => &filtry.page,
        pages => &total_pages,
        total => &total,
        data => &data,
        search => &filtry.search,
        stav => &filtry.stav,
        zeme => &filtry.zeme,
        zeme_seznam => &zeme,
        zeme_pilulky => &zeme_pilulky,
        // Připravený `&search=…&stav=…` pro odkazy stránkování.
        qs => &filtry.suffix(),
        // Cesta zpět pro serverové akce (skryté pole `zpet` ve formulářích).
        zpet => &filtry.url(),
        mesta => &mesta,
        mesta_pilulky => &mesta_pilulky,
        stavy_pilulky => &stavy_pilulky,
        s_emailem => &s_emailem,
        prum_hodnoceni => &prumer,
        nove_dnes => &nove_dnes,
        // Hromadný výběr ze session — pole id kvůli `p.id in vybrane` v šabloně.
        vybrane => &vybrane,
        vybrano => &vybrane.len()
    };

    match state.tera.render("index.html", &context) {
        Ok(html) => Html(html),
        Err(err) => {
            eprintln!("Tera error: {:?}", err);
            Html(format!("Chyba: {}", err))
        }
    }
}

/// Kolik hledaných výrazů se vejde do scraper_config (sloupce vyraz1..vyraz10).
pub const MAX_VYRAZU: usize = 10;

/// V scraper_config je vždy jen jeden řádek, id = 1.
const CONFIG_ID: i32 = 1;

async fn get_data_from_popup(
    pool: &PgPool,
    limit: i64,
    mesto: Option<String>,
    stav: &str,
    zeme: &str,
    razeni: &str,
    // `Some(ids)` = omezit na tyto provozovny (hromadný výběr), `None` = bez omezení.
    ids: Option<Vec<i32>>,
) -> Result<Vec<ProvozovnaExport>, sqlx::Error> {
    // `razeni` se do SQL vkládá formátováním, proto smí přijít jen z whitelistu
    // ve `download_file`. Ostatní parametry jdou jako bindy.
    let query = format!(
        r#"
        SELECT
            p.id, p.nazev, p.mesto, p.telefon, p.web,
            COALESCE(
                array_agg(e.email ORDER BY e.email) FILTER (WHERE e.email IS NOT NULL),
                '{{}}'
            ) AS emaily
        FROM provozovny p
        LEFT JOIN provozovny_emaily e ON e.provozovna_id = p.id
        WHERE ($1::text IS NULL OR p.mesto ILIKE $1)
          AND ($2 = ''
               OR ($2 = 'kontaktovane' AND p.is_contacted)
               OR ($2 = 'smluvene' AND p.is_closed))
          AND ($4::int4[] IS NULL OR p.id = ANY($4))
          AND ($5 = '' OR p.country_code = $5)
        GROUP BY p.id
        ORDER BY {}
        LIMIT $3
        "#,
        razeni
    );

    sqlx::query_as::<_, ProvozovnaExport>(&query)
        .bind(mesto)
        .bind(stav)
        .bind(limit.clamp(1, 10_000))
        .bind(ids)
        .bind(zeme)
        .fetch_all(pool)
        .await
}

fn do_xlsx(data: &[ProvozovnaExport]) -> Result<Vec<u8>, XlsxError> {
    let mut wb = Workbook::new();
    let sheet = wb.add_worksheet().set_name("Provozovny")?;

    let hlavicka = Format::new().set_bold().set_background_color("D9D9D9");

    for (i, nazev) in ["Název", "Město", "Telefon", "Web", "E-maily"].iter().enumerate() {
        sheet.write_string_with_format(0, i as u16, *nazev, &hlavicka)?;
    }

    for (r, p) in data.iter().enumerate() {
        let r = r as u32 + 1;
        sheet.write_string(r, 0, &p.nazev)?;
        sheet.write_string(r, 1, p.mesto.as_deref().unwrap_or(""))?;
        sheet.write_string(r, 2, p.telefon.as_deref().unwrap_or(""))?;
        sheet.write_string(r, 3, p.web.as_deref().unwrap_or(""))?;
        sheet.write_string(r, 4, &p.emaily.join("; "))?;
    }

    sheet.set_freeze_panes(1, 0)?;
    sheet.autofilter(0, 0, data.len() as u32, 4)?;
    sheet.autofit();

    wb.save_to_buffer()
}

fn do_xml(data: &[ProvozovnaExport]) -> Result<Vec<u8>, quick_xml::SeError> {
    #[derive(serde::Serialize)]
    #[serde(rename = "provozovny")]
    struct Root<'a> {
        provozovna: &'a [ProvozovnaExport],
    }

    let mut out = String::from(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    out.push('\n');
    out.push_str(&quick_xml::se::to_string(&Root { provozovna: data })?);
    Ok(out.into_bytes())
}

/// Rozmezí pro `max_kontaktu`. Nula by actor spustila naprázdno, horní
/// hranice brzdí překlep, který by spálil kredit na Apify.
pub const MIN_KONTAKTU: i32 = 1;
pub const MAX_KONTAKTU: i32 = 50;

/// Konfigurace scraperu z řádku scraper_config (id = 1).
pub struct ScraperConfig {
    pub lokace: String,
    /// Jen neprázdné hledané výrazy.
    pub vyrazy: Vec<String>,
    /// `maxCrawledPlacesPerSearch` pro Apify actor.
    pub max_kontaktu: i32,
    /// Vypínač denního cron jobu (checkbox v nastavení).
    pub cron_enabled: bool,
}

/// Načte konfiguraci scraperu (řádek id = 1). `Ok(None)` = řádek neexistuje.
pub async fn nacti_config(pool: &PgPool) -> Result<Option<ScraperConfig>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        SELECT lokace, max_kontaktu, cron_enabled,
               vyraz1, vyraz2, vyraz3, vyraz4, vyraz5,
               vyraz6, vyraz7, vyraz8, vyraz9, vyraz10
        FROM scraper_config
        WHERE id = $1
        "#,
        CONFIG_ID
    )
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|config| {
        let vyrazy: Vec<String> = [
            config.vyraz1,
            config.vyraz2,
            config.vyraz3,
            config.vyraz4,
            config.vyraz5,
            config.vyraz6,
            config.vyraz7,
            config.vyraz8,
            config.vyraz9,
            config.vyraz10,
        ]
        .into_iter()
        .flatten()
        .filter(|v| !v.trim().is_empty())
        .collect();

        ScraperConfig {
            lokace: config.lokace,
            vyrazy,
            max_kontaktu: config.max_kontaktu,
            cron_enabled: config.cron_enabled,
        }
    }))
}

/// Jestli má denní cron spouštět actor. Ptá se DB těsně před během, ne při
/// startu — změna checkboxu v nastavení tak platí hned, bez restartu.
/// Bez řádku konfigurace nebo při chybě DB se neběží (actor by stejně
/// bez konfigurace nic neudělal).
pub async fn cron_povolen(pool: &PgPool) -> bool {
    match sqlx::query_scalar!(
        "SELECT cron_enabled FROM scraper_config WHERE id = $1",
        CONFIG_ID
    )
    .fetch_optional(pool)
    .await
    {
        Ok(Some(povolen)) => povolen,
        Ok(None) => {
            eprintln!("scraper_config s id = {} neexistuje, cron se nespouští", CONFIG_ID);
            false
        }
        Err(e) => {
            eprintln!("DB error (cron_enabled), cron se nespouští: {:?}", e);
            false
        }
    }
}

/// Delší lokaci nemá smysl posílat na Nominatim.
const MAX_DELKA_LOKACE: usize = 120;

/// Ověří lokaci proti Nominatimu (OpenStreetMap).
/// `Ok(true)` = místo existuje, `Ok(false)` = nenalezeno, `Err` = službu se nepodařilo zeptat.
pub async fn overit_lokaci(state: &AppState, lokace: &str) -> Result<bool> {
    let response = state
        .http
        .get("https://nominatim.openstreetmap.org/search")
        .query(&[("q", lokace), ("format", "json"), ("limit", "1")])
        // Nominatim bez User-Agentu odmítá požadavky (jejich usage policy).
        .header(
            "User-Agent",
            "ivan-scraper/0.1 (kontakt: adam.hitzger@icloud.com)",
        )
        .timeout(Duration::from_secs(10))
        .send()
        .await?
        .error_for_status()?;

    let mista: Vec<Value> = response.json().await?;

    Ok(!mista.is_empty())
}

/// Vykreslí stránku nastavení. Používá ji GET i chybová větev POSTu,
/// aby po neúspěšném ověření zůstaly ve formuláři vyplněné hodnoty.
fn render_nastaveni(
    state: &AppState,
    lokace: &str,
    vyrazy: &[String],
    max_kontaktu: i32,
    cron_enabled: bool,
    ulozeno: bool,
    chyba: &str,
) -> Html<String> {
    let mut vyrazy = vyrazy.to_vec();

    // Ať je vidět aspoň jedno prázdné pole, i když nic není.
    if vyrazy.is_empty() {
        vyrazy.push(String::new());
    }

    let context = context! {
        lokace => &lokace,
        vyrazy => &vyrazy,
        max_vyrazu => &MAX_VYRAZU,
        max_kontaktu => &max_kontaktu,
        min_kontaktu_limit => &MIN_KONTAKTU,
        max_kontaktu_limit => &MAX_KONTAKTU,
        cron_enabled => &cron_enabled,
        ulozeno => &ulozeno,
        chyba => &chyba
    };

    match state.tera.render("nastaveni.html", &context) {
        Ok(html) => Html(html),
        Err(err) => {
            eprintln!("Tera error: {:?}", err);
            Html(format!("Chyba: {}", err))
        }
    }
}

pub async fn nastaveni_page(
    State(state): State<AppState>,
    params: Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let config = match nacti_config(&state.pool).await {
        Ok(Some(config)) => Some(config),
        Ok(None) => {
            eprintln!("scraper_config s id = {} neexistuje", CONFIG_ID);
            None
        }
        Err(e) => {
            eprintln!("DB error (scraper_config): {:?}", e);
            None
        }
    };

    // Bez řádku v DB se stránka vykreslí prázdná, ať se dá aspoň zobrazit.
    let (lokace, vyrazy, max_kontaktu, cron_enabled) = match config {
        Some(c) => (c.lokace, c.vyrazy, c.max_kontaktu, c.cron_enabled),
        None => (String::new(), Vec::new(), 30, true),
    };

    render_nastaveni(
        &state,
        &lokace,
        &vyrazy,
        max_kontaktu,
        cron_enabled,
        params.contains_key("ulozeno"),
        params.get("chyba").map(String::as_str).unwrap_or_default(),
    )
}

pub async fn nastaveni_save(
    State(state): State<AppState>,
    // Vec<(String, String)> místo HashMap — `vyraz` chodí opakovaně a mapa by nechala jen poslední.
    Form(pole): Form<Vec<(String, String)>>,
) -> impl IntoResponse {
    let mut lokace = String::new();
    let mut vyrazy: Vec<String> = Vec::new();
    // None = pole chybí nebo není číslo; rozsah se kontroluje níž.
    let mut max_kontaktu: Option<i32> = None;
    // Nezaškrtnutý checkbox prohlížeč vůbec nepošle, proto výchozí false.
    let mut cron_enabled = false;

    for (klic, hodnota) in pole {
        match klic.as_str() {
            "lokace" => lokace = hodnota.trim().to_string(),
            "max_kontaktu" => max_kontaktu = hodnota.trim().parse().ok(),
            "cron_enabled" => cron_enabled = true,
            "vyraz" => {
                let vyraz = hodnota.trim().to_string();
                if !vyraz.is_empty() && vyrazy.len() < MAX_VYRAZU {
                    vyrazy.push(vyraz);
                }
            }
            _ => {}
        }
    }

    // Počet kontaktů musí být v rozsahu — mimo něj se nic neuloží a formulář
    // se vykreslí znovu s hodnotou, kterou uživatel zadal (nebo s výchozí).
    let max_kontaktu = match max_kontaktu {
        Some(n) if (MIN_KONTAKTU..=MAX_KONTAKTU).contains(&n) => n,
        jiny => {
            let zobraz = jiny.unwrap_or(30);
            return render_nastaveni(&state, &lokace, &vyrazy, zobraz, cron_enabled, false, "pocet").into_response();
        }
    };

    // Ověření lokace proti Nominatimu. Prázdnou neověřujeme — ta uloženou hodnotu nepřepíše.
    // Při neúspěchu se stránka vykreslí rovnou (ne redirect), aby uživatel nepřišel o rozepsané hodnoty.
    if !lokace.is_empty() {
        if lokace.chars().count() > MAX_DELKA_LOKACE {
            return render_nastaveni(&state, &lokace, &vyrazy, max_kontaktu, cron_enabled, false, "lokace").into_response();
        }

        match overit_lokaci(&state, &lokace).await {
            Ok(true) => {}
            Ok(false) => {
                println!("Lokace '{}' nebyla na Nominatimu nalezena", lokace);
                return render_nastaveni(&state, &lokace, &vyrazy, max_kontaktu, cron_enabled, false, "lokace").into_response();
            }
            Err(e) => {
                eprintln!("Ověření lokace '{}' selhalo: {:?}", lokace, e);
                return render_nastaveni(&state, &lokace, &vyrazy, max_kontaktu, cron_enabled, false, "overeni").into_response();
            }
        }
    }

    let vyraz = |i: usize| vyrazy.get(i).cloned();

    // Prázdná lokace nepřepíše uloženou — sloupec je NOT NULL.
    let result = sqlx::query!(
        r#"
        UPDATE scraper_config
        SET lokace  = COALESCE(NULLIF($2, ''), lokace),
            vyraz1  = $3,
            vyraz2  = $4,
            vyraz3  = $5,
            vyraz4  = $6,
            vyraz5  = $7,
            vyraz6  = $8,
            vyraz7  = $9,
            vyraz8  = $10,
            vyraz9  = $11,
            vyraz10 = $12,
            max_kontaktu = $13,
            cron_enabled = $14,
            updated_at = NOW()
        WHERE id = $1
        "#,
        CONFIG_ID,
        lokace,
        vyraz(0),
        vyraz(1),
        vyraz(2),
        vyraz(3),
        vyraz(4),
        vyraz(5),
        vyraz(6),
        vyraz(7),
        vyraz(8),
        vyraz(9),
        max_kontaktu,
        cron_enabled
    )
    .execute(&state.pool)
    .await;

    match result {
        Ok(r) if r.rows_affected() == 1 => Redirect::to("/nastaveni?ulozeno=1").into_response(),
        Ok(_) => {
            eprintln!("scraper_config s id = {} neexistuje, nic se neuložilo", CONFIG_ID);
            Redirect::to("/nastaveni?chyba=ulozeni").into_response()
        }
        Err(e) => {
            eprintln!("DB error (uložení nastavení): {:?}", e);
            Redirect::to("/nastaveni?chyba=ulozeni").into_response()
        }
    }
}

pub fn headers(state: &AppState) -> Result<HeaderMap>{
    let mut headers: HeaderMap = HeaderMap::new();
    headers.insert("Content-Type", "application/json".parse()?);
    headers.insert("Accept", "application/json".parse()?);
    headers.insert("Authorization", format!("Bearer {}", state.config.apify_token).parse()?);
    
    Ok(headers)
}

pub async fn health() -> impl IntoResponse {
    Json(json!({ "status": "ok" }))
}

pub async fn apify_webhook(
    State(state): State<AppState>,
    Json(payload): Json<ApifyWebhook>
) -> StatusCode{ 
    println!("Webhook data");
    println!(
        "webhook: {} | run {} | status {} | dataset {}",
        payload.event_type,
        payload.resource.id,
        payload.resource.status,
        payload.resource.default_dataset_id,
    );

    let dataset_id = payload.resource.default_dataset_id;
    tokio::spawn(async move {
        if let Err(e) = fetch_dataset(&state, &dataset_id).await {
            tracing::error!(dataset_id, chyba = ?e, "Fetch datasetu ve webhooku selhal");
        }
    });
    StatusCode::OK
}

pub async fn fetch_dataset(state: &AppState, dataset_id: &str) -> Result<Vec<Place>> {
    let url = format!("https://api.apify.com/v2/datasets/{}/items",dataset_id);

    let request = state.http.request(Method::GET, url)
        .headers(headers(state)?)
        .query(&[("fields", "title,phone,phoneUnformatted,website,address,city,postalCode,emails,totalScore,reviewsCount,placeId,url,countryCode"), ("clean", "true"), ("format", "json")]);

    let response: Response = request.send().await?;
    let data: Vec<Place> = response.json().await?;

    println!("Vyscrapovano dat: {}", data.len());
    let (nove, _existujici) = insert_place(&state.pool, &data).await?;

    // Bez nových kontaktů nemá smysl posílat prázdný mail.
    if nove.is_empty() {
        println!("Žádné nové kontakty, e-mail se neodesílá");
        return Ok(data);
    }

    send_kontakty_email(state, nove, "adam.hitzger@icloud.com").await?;
    Ok(data)
}

pub async fn insert_place<'a>(pool: &PgPool, places: &'a[Place]) -> Result<(Vec<&'a Place>, Vec<&'a Place>)> {
   let mut tx = pool.begin().await?;
    let mut nove = Vec::new();
    let mut existujici = Vec::new();
for place in places {
    // ON CONFLICT DO UPDATE vrací řádek i při konfliktu, takže samotné
    // RETURNING id nerozliší nový záznam od existujícího. `xmax = 0` platí
    // jen pro čerstvě vložený řádek — u UPDATE větve je xmax id transakce.
    let row = sqlx::query!(
        r#"
        INSERT INTO provozovny (place_id, nazev, telefon, web, adresa, mesto, psc, hodnoceni, country_code)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        -- country_code se u známých záznamů doplní, pokud ho ještě nemají
        -- (sloupec přibyl později); jinou hodnotou se nepřepisuje.
        ON CONFLICT (place_id) DO UPDATE
            SET updated_at = NOW(),
                country_code = COALESCE(provozovny.country_code, EXCLUDED.country_code)
        RETURNING id, (xmax = 0) AS "vlozeno!"
        "#,
        place.place_id, place.title, place.phone, place.website,
        place.address, place.city, place.postal_code,
       place.total_score.map(|s| s as f32),
        place.country_code,
    )
    .fetch_one(&mut *tx)
    .await?;

    // E-maily doplňujeme i u známých provozoven — mohly přibýt.
    for email in &place.emails {
        sqlx::query!(
            r#"
            INSERT INTO provozovny_emaily (provozovna_id, email)
            VALUES ($1, $2)
            ON CONFLICT DO NOTHING
            "#,
            row.id, email
        )
        .execute(&mut *tx)
        .await?;
    }

    if row.vlozeno {
        nove.push(place);
    } else {
        existujici.push(place);
    }
}

    tx.commit().await?;  
    println!("Nove {}, existujici {}", nove.len(), existujici.len());
    Ok((nove, existujici))
}

pub fn run_cron_job(state: AppState) -> Result<Job> {
    // Denně v 6:00 pražského času.
    let job: Job = Job::new_async_tz("0 0 6 * * *", Prague,move |_uuid, _l| {
        let job_state = state.clone();
        Box::pin(async move {
            if !cron_povolen(&job_state.pool).await {
                println!("Cron je v nastavení vypnutý, actor se nespouští");
                return;
            }
            let _ = run_actor(&job_state).await;
        })
    })?;
    Ok(job)
} 

pub async fn run_actor(state: &AppState) -> Result<(), Box<dyn Error>> {

    println!("Spuštění cron jobu, načtení výrazů a lokace");

    let ScraperConfig { lokace, vyrazy, max_kontaktu, .. } = match nacti_config(&state.pool).await {
        Ok(Some(config)) => config,
        Ok(None) => {
            eprintln!("scraper_config s id = {} neexistuje, actor se nespouští", CONFIG_ID);
            return Ok(());
        }
        Err(e) => {
            eprintln!("DB error (scraper_config), actor se nespouští: {:?}", e);
            return Ok(());
        }
    };

    // Bez výrazů by actor projel naprázdno a jen spálil kredit.
    if vyrazy.is_empty() {
        eprintln!("V nastavení nejsou žádné hledané výrazy, actor se nespouští");
        return Ok(());
    }

    println!("Scrapuji {:?} v lokaci '{}', max {} míst na výraz", vyrazy, lokace, max_kontaktu);

    let url: String = format!("https://api.apify.com/v2/actors/{}/runs", state.config.apify_actor_id);

    let json_data: Value = json!({
        "includeWebResults": true,
        "language": "cs",
        "locationQuery": lokace,
        "maxCrawledPlacesPerSearch": max_kontaktu,
        "maximumLeadsEnrichmentRecords": 0,
        "scrapeContacts": true,
        "scrapeDirectories": false,
        "scrapeImageAuthors": false,
        "scrapeOrderOnline": false,
        "scrapePlaceDetailPage": true,
        "scrapeReviewsPersonalData": true,
        "scrapeSocialMediaProfiles": {
            "facebooks": false,
            "instagrams": false,
            "tiktoks": false,
            "twitters": false,
            "youtubes": false
        },
        "scrapeTableReservationProvider": false,
        "searchStringsArray": vyrazy,
        "skipClosedPlaces": true,
        "verifyLeadsEnrichmentEmails": false,
        "website": "withWebsite",
        "categoryFilterWords": [
            "cannabis club",
            "cannabis store",
            "vaporizer store"
        ],
        "placeMinimumStars": "threeAndHalf",
    });

    let request: RequestBuilder = state.http.request(Method::POST, url)
        .headers(headers(state)?)
        .json(&json_data);

    let response: Response = request.send().await?;
    let body: String = response.text().await?;

    println!("{}", body);

    Ok(())
}

pub async fn login(
    State(state): State<AppState>,
    Form(params): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    let kod = params.get("kod").cloned().unwrap_or_default();
    
    if kod != std::env::var("LOGIN_KOD").unwrap_or_default() {
        return Redirect::to("/login?error=1").into_response();
    }

    let session_id = uuid::Uuid::new_v4().to_string();
    state.sessions.insert(session_id.clone(), Instant::now());

    // Secure funguje i na http://localhost — prohlížeče ho berou jako bezpečný kontext.
    let cookie = format!(
        "session={}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=86400",
        session_id
    );
    
    (
        [(axum::http::header::SET_COOKIE, cookie)],
        Redirect::to("/"),
    ).into_response()
}

pub async fn auth_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> impl IntoResponse {
    let session_id = req.headers()
        .get("cookie")
        .and_then(|c| c.to_str().ok())
        .and_then(|c| {
            c.split(';')
                .find(|s| s.trim().starts_with("session="))
                .map(|s| s.trim().trim_start_matches("session=").to_string())
        });

    let valid = session_id.as_ref().map(|id| {
        state.sessions.get(id)
            .map(|t| t.elapsed().as_secs() < 86400) // 24h
            .unwrap_or(false)
    }).unwrap_or(false);

    if !valid {
        return Redirect::to("/login").into_response();
    }

    next.run(req).await.into_response()
}

pub async fn login_page(State(state): State<AppState>, params: Query<HashMap<String,String>>) -> impl IntoResponse {
    let error = params.get("error").is_some();
    let mut context = Context::new();
    context.insert("error", &error);
    Html(state.tera.render("login.html", &context).unwrap())
}

// ───────────── Hromadný výběr (session) ─────────────

/// Klíč v session, pod kterým leží id provozoven vybraných napříč stránkami.
const VYBER_KEY: &str = "vyber";

async fn nacti_vyber(session: &Session) -> BTreeSet<i32> {
    match session.get::<BTreeSet<i32>>(VYBER_KEY).await {
        Ok(v) => v.unwrap_or_default(),
        Err(e) => {
            eprintln!("Session (čtení výběru): {e:?}");
            BTreeSet::new()
        }
    }
}

async fn uloz_vyber(session: &Session, vyber: &BTreeSet<i32>) {
    // Prázdný výběr se maže úplně, ať se v PG nedrží session jen kvůli `[]`.
    let vysledek = if vyber.is_empty() {
        session.remove::<BTreeSet<i32>>(VYBER_KEY).await.map(|_| ())
    } else {
        session.insert(VYBER_KEY, vyber).await
    };
    if let Err(e) = vysledek {
        eprintln!("Session (zápis výběru): {e:?}");
    }
}

/// Vrátí výběr zbavený id, která už v DB neexistují; když se něco odstranilo,
/// rovnou to zapíše zpět do session.
async fn procisti_vyber(pool: &PgPool, session: &Session) -> Vec<i32> {
    let vyber = nacti_vyber(session).await;
    if vyber.is_empty() {
        return Vec::new();
    }

    let ids: Vec<i32> = vyber.iter().copied().collect();
    let existujici: BTreeSet<i32> = match sqlx::query_scalar!("SELECT id FROM provozovny WHERE id = ANY($1)", &ids)
        .fetch_all(pool)
        .await
    {
        Ok(v) => v.into_iter().collect(),
        // Při chybě DB výběr raději nechat, než ho uživateli tiše smazat.
        Err(e) => {
            eprintln!("DB chyba (kontrola výběru): {e:?}");
            return ids;
        }
    };

    if existujici.len() != vyber.len() {
        uloz_vyber(session, &existujici).await;
    }
    existujici.into_iter().collect()
}

/// POST /vyber — upraví hromadný výběr v session.
/// Formulář: `akce` = pridat | odebrat | zrusit, `id` (může se opakovat), `zpet`.
/// Checkboxy volají přes fetch s `Accept: application/json` a dostanou `{"pocet": N}`,
/// aby se stránka nepřekreslovala; bez toho se odpovídá redirectem na `zpet`.
pub async fn vyber_zmena(
    session: Session,
    hlavicky: axum::http::HeaderMap,
    Form(pole): Form<Vec<(String, String)>>,
) -> impl IntoResponse {
    let mut akce = String::new();
    let mut ids: Vec<i32> = Vec::new();
    let mut zpet: Option<String> = None;

    for (klic, hodnota) in pole {
        match klic.as_str() {
            "akce" => akce = hodnota,
            "id" => if let Ok(id) = hodnota.trim().parse::<i32>() { ids.push(id) },
            "zpet" => zpet = Some(hodnota),
            _ => {}
        }
    }

    let mut vyber = nacti_vyber(&session).await;
    match akce.as_str() {
        "pridat" => vyber.extend(ids),
        "odebrat" => vyber.retain(|id| !ids.contains(id)),
        "zrusit" => vyber.clear(),
        _ => return (StatusCode::BAD_REQUEST, "neznámá akce").into_response(),
    }
    uloz_vyber(&session, &vyber).await;

    let chce_json = hlavicky
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("application/json"));

    if chce_json {
        Json(json!({ "pocet": vyber.len() })).into_response()
    } else {
        Redirect::to(&bezpecny_navrat(zpet.as_deref())).into_response()
    }
}

/// POST /hromadne/smazat — smaže všechny provozovny z výběru a výběr vyprázdní.
pub async fn hromadne_smazat(
    State(state): State<AppState>,
    session: Session,
    Form(params): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    let zpet = bezpecny_navrat(params.get("zpet").map(String::as_str));

    let ids: Vec<i32> = nacti_vyber(&session).await.into_iter().collect();
    if ids.is_empty() {
        return Redirect::to(&zpet).into_response();
    }

    // E-maily odejdou přes ON DELETE CASCADE.
    match sqlx::query!("DELETE FROM provozovny WHERE id = ANY($1)", &ids)
        .execute(&state.pool)
        .await
    {
        Ok(r) => {
            info!("Hromadně smazáno {} provozoven", r.rows_affected());
            uloz_vyber(&session, &BTreeSet::new()).await;
        }
        Err(e) => eprintln!("DB chyba (hromadné mazání, {} id): {e:?}", ids.len()),
    }

    Redirect::to(&zpet).into_response()
}

pub async fn delete_record(State(state): State<AppState>, Form(params): Form<HashMap<String, String>>) -> impl IntoResponse {
    // Formulář posílá `zpet` = výpis se stránkou i filtry, aby mazání
    // neodhodilo uživatele na první stránku bez filtru.
    let zpet = bezpecny_navrat(params.get("zpet").map(String::as_str));

    let id: i32 = match params.get("id").and_then(|id| id.parse().ok()) {
        Some(id) => id,
        None => return Redirect::to(&zpet).into_response(),
    };

    match sqlx::query!("DELETE FROM provozovny WHERE id = $1", id)
        .execute(&state.pool)
        .await
    {
        Ok(_) => Redirect::to(&zpet).into_response(),
        Err(e) => {
            eprintln!("Delete error: {:?}", e);
            Redirect::to(&zpet).into_response()
        }
    }
}

pub async fn download_file(State(state): State<AppState>, session: Session, Form(pole): Form<Vec<(String, String)>>) -> impl IntoResponse {
    println!("Start serverové akce POST /download");

    let mut format: Option<String> = None;
    let mut razeni: Option<&'static str> = None;
    let mut limit: Option<i64> = None;
    let mut mesto: Option<String> = None;
    let mut stav = String::new();
    let mut zeme = String::new();
    let mut jen_vybrane = false;

    for (klic, hodnota) in pole {
        let h = hodnota.trim();
        match klic.as_str() {
            "format" => format = Some(h.to_string()),
            "razeni" => razeni = match h {
                "nazev_asc"  => Some("p.nazev ASC"),
                "nazev_desc" => Some("p.nazev DESC"),
                "datum_asc"  => Some("p.created_at ASC"),
                "datum_desc" => Some("p.created_at DESC"),
                _ => None,
            },
            "limit" => limit = h.parse::<i64>().ok(),
            "mesto" => mesto = Some(h.to_string()),
            // Stejný whitelist jako u výpisu — neznámá hodnota = bez filtru.
            "stav" => stav = match h {
                "kontaktovane" | "smluvene" => h.to_string(),
                _ => String::new(),
            },
            "zeme" => zeme = normalizuj_zemi(Some(h)),
            "jen_vybrane" => jen_vybrane = matches!(h, "1" | "true" | "on"),
            _ => {}
        }
    }

    let format = format.unwrap_or_else(|| "json".to_string());
    let razeni = razeni.unwrap_or("p.nazev ASC");
    let mut limit = limit.unwrap_or(100).clamp(1, 10_000);
    let mut mesto = mesto.filter(|s| !s.is_empty());

    // Export hromadného výběru: bere přesně to, co si uživatel naklikal, takže
    // město/stav/limit z popupu se ignorují — jen řazení zůstává.
    let mut ids: Option<Vec<i32>> = None;
    if jen_vybrane {
        let vyber = nacti_vyber(&session).await;
        if !vyber.is_empty() {
            limit = 10_000;
            mesto = None;
            stav.clear();
            zeme.clear();
            ids = Some(vyber.into_iter().collect());
        }
    }

    let data = match get_data_from_popup(&state.pool, limit, mesto, &stav, &zeme, razeni, ids).await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("DB chyba: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "chyba databáze").into_response();
        }
    };

    match format.as_str(){
        "json" =>{
                let telo = match serde_json::to_vec_pretty(&data) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("Chyba serializace jsonu: {e}");
                    return (StatusCode::INTERNAL_SERVER_ERROR, "chyba exportu jsonu").into_response();
                }
            };

            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, "application/json; charset=utf-8"),
                    (header::CONTENT_DISPOSITION, "attachment; filename=\"provozovny.json\""),
                ],
                telo,
            ).into_response()
        }
        "xml" => {
            let telo = match do_xml(&data) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("xml: {e}");
                    return (StatusCode::INTERNAL_SERVER_ERROR, "chyba exportu xml").into_response();
                }
            };

            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, "application/xml; charset=utf-8"),
                    (header::CONTENT_DISPOSITION, "attachment; filename=\"provozovny.xml\""),
                ],
                telo,
            ).into_response()
        }
        "excel" => {
        let telo = match do_xlsx(&data) { 
            Ok(t)=> t,
            Err(e) => {
                eprintln!("xlsx: {e}");
                return (StatusCode::INTERNAL_SERVER_ERROR, "chyba exportu xlsx").into_response();
            }
         };
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
                    (header::CONTENT_DISPOSITION, "attachment; filename=\"provozovny.xlsx\""),
                ],
                telo,
            ).into_response()
        }
    _ => (StatusCode::BAD_REQUEST, "neznámý formát").into_response(),
    }
}


/// Tělo formuláře z dropdownu v tabulce provozoven.
/// `hodnota` je nová hodnota příznaku, ne přepínač — o tom, co se posílá, rozhoduje šablona.
#[derive(serde::Deserialize)]
pub struct StavForm {
    pub id: i32,
    pub hodnota: bool,
    /// Kam se vrátit — výpis se stránkou a filtry, nebo detail provozovny.
    pub zpet: Option<String>,
}

/// POST /kontaktovane — nastaví u jedné provozovny příznak „kontaktované".
pub async fn set_kontaktovane(
    State(state): State<AppState>,
    Form(form): Form<StavForm>,
) -> impl IntoResponse {
    // updated_at se schválně nemění — řadí se podle něj výpis a záznam by přeskočil nahoru.
    let zpet = bezpecny_navrat(form.zpet.as_deref());

    match sqlx::query!(
        "UPDATE provozovny SET is_contacted = $2 WHERE id = $1",
        form.id,
        form.hodnota
    )
    .execute(&state.pool)
    .await
    {
        Ok(_) => Redirect::to(&zpet).into_response(),
        Err(e) => {
            eprintln!("DB chyba (is_contacted, id = {}): {:?}", form.id, e);
            Redirect::to(&zpet).into_response()
        }
    }
}

/// POST /smluvene — nastaví u jedné provozovny příznak „smluvené".
pub async fn set_smluvene(
    State(state): State<AppState>,
    Form(form): Form<StavForm>,
) -> impl IntoResponse {
    let zpet = bezpecny_navrat(form.zpet.as_deref());

    match sqlx::query!(
        "UPDATE provozovny SET is_closed = $2 WHERE id = $1",
        form.id,
        form.hodnota
    )
    .execute(&state.pool)
    .await
    {
        Ok(_) => Redirect::to(&zpet).into_response(),
        Err(e) => {
            eprintln!("DB chyba (is_closed, id = {}): {:?}", form.id, e);
            Redirect::to(&zpet).into_response()
        }
    }
}

/// GET /provozovna/:id — detail jedné provozovny.
///
/// Query parametry (`page`, `search`, `stav`) nesou filtr z výpisu, aby odkaz
/// „zpět" vrátil uživatele tam, odkud přišel. Nečíselné `id` se bere jako
/// neexistující záznam (stránka 404), ne jako chyba parsování z axumu.
pub async fn provozovna_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
    params: Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let filtry = Filtry::z_params(&params);

    let Ok(id) = id.parse::<i32>() else {
        return render_detail(&state, None, &filtry);
    };

    let provozovna = sqlx::query_as!(
        Provozovna,
        r#"
        SELECT
            p.id, p.place_id, p.nazev, p.telefon, p.telefon_raw, p.web, p.adresa, p.mesto,
            p.psc, p.hodnoceni, p.pocet_recenzi, p.url, p.country_code,
        p.created_at AS "created_at: _", p.updated_at AS "updated_at: _",
            array_remove(array_agg(e.email), NULL) AS emaily,
            p.is_contacted, p.is_closed, p.poznamka
        FROM provozovny p
        LEFT JOIN provozovny_emaily e ON e.provozovna_id = p.id
        WHERE p.id = $1
        GROUP BY p.id
        "#,
        id
    )
    .fetch_optional(&state.pool)
    .await;

    let provozovna = match provozovna {
        Ok(Some(p)) => p,
        Ok(None) => return render_detail(&state, None, &filtry),
        Err(e) => {
            eprintln!("DB chyba (detail provozovny, id = {}): {:?}", id, e);
            return render_detail(&state, None, &filtry);
        }
    };

    render_detail(&state, Some(&provozovna), &filtry)
}

/// Vykreslí detail. `provozovna == None` znamená neexistující záznam —
/// vrací se 404, ať se chybný odkaz nechová jako platná stránka.
fn render_detail(state: &AppState, provozovna: Option<&Provozovna>, filtry: &Filtry) -> AxumResponse {
    let context = context! {
        p => &provozovna,
        mapa_url => &provozovna.map(|p| mapa_url(state, p)).unwrap_or_default(),
        max_emailu => &MAX_EMAILU,
        // Odkaz zpět do výpisu i cíl přesměrování po akcích na této stránce.
        zpet => &filtry.url(),
        detail_url => &provozovna.map(|p| filtry.url_detailu(p.id)).unwrap_or_default(),
    };

    let status = if provozovna.is_some() {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    };

    match state.tera.render("provozovna.html", &context) {
        Ok(html) => (status, Html(html)).into_response(),
        Err(err) => {
            eprintln!("Tera error: {:?}", err);
            (StatusCode::INTERNAL_SERVER_ERROR, Html(format!("Chyba: {}", err))).into_response()
        }
    }
}

/// Adresa pro `<iframe>` s Google mapou na detailu.
///
/// S klíčem Embed API (`GOOGLE_MAPS_EMBED_KEY`) se mapa vkládá přesně podle
/// `place_id`. Bez klíče Google `place_id` v embedu neumí (ověřeno — vykreslí
/// prázdnou mapu světa), takže se hledá podle názvu a adresy; to je pro
/// zobrazení jedné provozovny dostatečné. Skládá se tady, ne v šabloně —
/// Tera 2 nemá `urlencode`.
fn mapa_url(state: &AppState, p: &Provozovna) -> String {
    match &state.config.google_maps_embed_key {
        Some(klic) => format!(
            "https://www.google.com/maps/embed/v1/place?key={}&q=place_id:{}&language=cs",
            enkoduj(klic),
            enkoduj(&p.place_id)
        ),
        None => {
            let dotaz = match p.adresa.as_deref().filter(|a| !a.is_empty()) {
                Some(adresa) => format!("{}, {}", p.nazev, adresa),
                None => p.nazev.clone(),
            };
            format!(
                "https://maps.google.com/maps?q={}&z=15&hl=cs&output=embed",
                enkoduj(&dotaz)
            )
        }
    }
}

/// Nejdelší poznámka, kterou přijmeme. Delší text je skoro jistě omyl
/// (vložený dokument) a nemá smysl ho ukládat celý.
const MAX_DELKA_POZNAMKY: usize = 50_000;

/// Tělo formuláře s poznámkou z detailu provozovny.
#[derive(serde::Deserialize)]
pub struct PoznamkaForm {
    pub id: i32,
    pub poznamka: String,
    pub zpet: Option<String>,
}

static SANITIZER: LazyLock<ammonia::Builder<'static>> = LazyLock::new(|| {
    let mut b = ammonia::Builder::default();
    b.tags(HashSet::from([
        "div", "br", "strong", "em", "del", "a",
        "h1", "blockquote", "pre", "ul", "ol", "li",
    ]))
    .tag_attributes(std::collections::HashMap::from([
        ("a", HashSet::from(["href"])),
    ]))
    .url_schemes(HashSet::from(["http", "https", "mailto", "tel"]))
    .link_rel(Some("noopener noreferrer nofollow"));
    b
});

pub async fn set_poznamka(
    State(state): State<AppState>,
    Form(form): Form<PoznamkaForm>,
) -> impl IntoResponse {
    let zpet = bezpecny_navrat(form.zpet.as_deref());

    let surove: String = form
        .poznamka
        .trim()
        .chars()
        .take(MAX_DELKA_POZNAMKY)
        .collect();

    let poznamka = SANITIZER.clean(&surove).to_string();

    // updated_at se schválně nemění — řadí se podle něj výpis a záznam
    // by kvůli poznámce přeskočil nahoru.
    match sqlx::query!(
        "UPDATE provozovny SET poznamka = $2 WHERE id = $1",
        form.id,
        poznamka
    )
    .execute(&state.pool)
    .await
    {
        Ok(_) => Redirect::to(&zpet).into_response(),
        Err(e) => {
            eprintln!("DB chyba (poznamka, id = {}): {:?}", form.id, e);
            Redirect::to(&zpet).into_response()
        }
    }
}

/// Horní hranice počtu e-mailů u jedné provozovny — víc je skoro jistě omyl.
const MAX_EMAILU: usize = 20;

/// POST /kontakt — uloží telefon a e-maily z detailu provozovny.
///
/// `email` chodí opakovaně (jedno pole na adresu), proto `Vec<(String, String)>`
/// a ne struktura. Prázdná pole se zahodí — smazání e-mailu = vymazat input.
/// E-maily se nahrazují celé (smazat + vložit), aby to byla jedna operace
/// bez porovnávání starého a nového seznamu.
pub async fn set_kontakt(
    State(state): State<AppState>,
    Form(pole): Form<Vec<(String, String)>>,
) -> impl IntoResponse {
    let mut id: Option<i32> = None;
    let mut zpet: Option<String> = None;
    let mut telefon = String::new();
    let mut emaily: Vec<String> = Vec::new();

    for (klic, hodnota) in pole {
        match klic.as_str() {
            "id" => id = hodnota.trim().parse().ok(),
            "zpet" => zpet = Some(hodnota),
            "telefon" => telefon = hodnota.trim().to_string(),
            "email" => {
                let email = hodnota.trim().to_lowercase();
                // Jen hrubá kontrola tvaru — ať se neuloží zjevný nesmysl,
                // ale ani neodmítne neobvyklá, přesto platná adresa.
                let vypada_jako_email = email.contains('@')
                    && !email.starts_with('@')
                    && !email.ends_with('@')
                    && !email.contains(char::is_whitespace);
                if vypada_jako_email && !emaily.contains(&email) && emaily.len() < MAX_EMAILU {
                    emaily.push(email);
                }
            }
            _ => {}
        }
    }

    let zpet = bezpecny_navrat(zpet.as_deref());

    let Some(id) = id else {
        return Redirect::to(&zpet).into_response();
    };

    // Prázdný telefon = NULL, stejně jako u záznamů z Apify bez telefonu.
    let telefon = Some(telefon).filter(|t| !t.is_empty());

    // updated_at se schválně nemění — řadí se podle něj výpis a záznam
    // by kvůli opravě kontaktu přeskočil nahoru.
    let vysledek: Result<(), sqlx::Error> = async {
        let mut tx = state.pool.begin().await?;

        sqlx::query!(
            "UPDATE provozovny SET telefon = $2 WHERE id = $1",
            id,
            telefon
        )
        .execute(&mut *tx)
        .await?;

        sqlx::query!("DELETE FROM provozovny_emaily WHERE provozovna_id = $1", id)
            .execute(&mut *tx)
            .await?;

        for email in &emaily {
            sqlx::query!(
                "INSERT INTO provozovny_emaily (provozovna_id, email) VALUES ($1, $2)",
                id,
                email
            )
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await
    }
    .await;

    if let Err(e) = vysledek {
        eprintln!("DB chyba (kontakt, id = {}): {:?}", id, e);
    }

    Redirect::to(&zpet).into_response()
}

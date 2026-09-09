use rust_xlsxwriter::{Workbook, Format, XlsxError};
use axum::{
    Form, extract::{
        Json, Query, State, Request
    }, http::{header, StatusCode}, response::{Html, IntoResponse, Redirect},
    middleware::Next
};
use tokio_cron_scheduler::Job;
use chrono_tz::Europe::Prague;
use std::{collections::HashMap, error::Error, time::{Duration, Instant}};
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
    let mut context = tera::Context::new();
    context.insert("kontakty", &nove_kontakty);

    let html = state.tera.render("email_kontakty.html", &context)?;

    let email = Message::builder()
        .from("ikt2stupen@gmail.com".parse()?)
        .to(recipient.parse()?)
        .subject(format!("Nové kontakty — {} firem", nove_kontakty.len()))
        .header(ContentType::TEXT_HTML)
        .body(html)?;

    state.smtp.send(email).await?;

    Ok(())
}

pub async fn html_page(State(state): State<AppState>, params: Query<HashMap<String,String>>) -> impl IntoResponse{
    let mut context: Context = Context::new();
    
    let limit: i64 = 10;
    let page: i64 = params.get("page")
        .and_then(|p| p.parse::<i64>().ok())
        .unwrap_or(1);

    let offset: i64 = (page - 1) * limit;

    let search = params.get("search")
    .cloned()
    .unwrap_or_default();
    
    let search_pattern = format!("%{}%", search);   

    // Filtr stavu z pilulek pod městy. Cokoli jiného než známé hodnoty = bez filtru,
    // ať se do SQL nedostane nic neočekávaného.
    let stav = match params.get("stav").map(String::as_str) {
        Some(s @ ("kontaktovane" | "smluvene")) => s.to_string(),
        _ => String::new(),
    };

    let kontakty = sqlx::query_as!(
    Provozovna,
    r#"
    -- Sloupce se vypisují ručně: query_as! je mapuje na pole struktury podle pořadí,
    -- takže `p.*` by při jiném fyzickém rozložení tabulky posunulo hodnoty a dekódování
    -- by spadlo panikou uvnitř bytes místo čitelné chyby.
    SELECT
        p.id, p.place_id, p.nazev, p.telefon, p.telefon_raw, p.web, p.adresa, p.mesto,
        p.psc, p.hodnoceni, p.pocet_recenzi, p.url, p.created_at, p.updated_at,
        array_remove(array_agg(e.email), NULL) AS emaily,
        p.is_contacted, p.is_closed
    FROM provozovny p
    LEFT JOIN provozovny_emaily e ON e.provozovna_id = p.id
    WHERE ($3 = '' OR p.mesto ILIKE $3)
      AND ($4 = ''
           OR ($4 = 'kontaktovane' AND p.is_contacted)
           OR ($4 = 'smluvene' AND p.is_closed))
    GROUP BY p.id
    ORDER BY p.updated_at DESC
    LIMIT $1 OFFSET $2
    "#,
        limit,
        offset,
        search_pattern,
        stav
    )
    .fetch_all(&state.pool)
    .await;

    match &kontakty {
        Ok(data) => println!("Načteno: {} záznamů", data.len()),
        Err(e) => eprintln!("DB error: {:?}", e),
    }

    let mesta:Vec<String> = sqlx::query!(
        "SELECT DISTINCT mesto FROM provozovny WHERE mesto IS NOT NULL ORDER BY mesto"
    )
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .filter_map(|r| r.mesto)
    .collect();

    

    
    let kontakty = kontakty.unwrap_or_default();
    let total = sqlx::query_scalar!(
        r#"
    SELECT COUNT(*) FROM provozovny
    WHERE ($1 = '' OR mesto ILIKE $1)
      AND ($2 = ''
           OR ($2 = 'kontaktovane' AND is_contacted)
           OR ($2 = 'smluvene' AND is_closed))
    "#,
    search,
    stav
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

    context = context!{
        page => &page,
        pages => &total_pages,
        total => &total,
        data => &kontakty,
        search => &search,
        stav => &stav,
        mesta => &mesta,
        s_emailem => &s_emailem,
        prum_hodnoceni => &prumer,
        nove_dnes => &nove_dnes
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

/// Načte konfiguraci scraperu (řádek id = 1): lokaci a neprázdné hledané výrazy.
/// `Ok(None)` = řádek neexistuje.

async fn get_data_from_popup(
    pool: &PgPool,
    limit: i64,
    mesto: Option<String>,
    razeni: &str,
) -> Result<Vec<ProvozovnaExport>, sqlx::Error> {
    
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
        GROUP BY p.id
        ORDER BY {}
        LIMIT $2
        "#,
        razeni
    );

    sqlx::query_as::<_, ProvozovnaExport>(&query)
        .bind(mesto)
        .bind(limit.clamp(1, 10_000))
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

pub async fn nacti_config(pool: &PgPool) -> Result<Option<(String, Vec<String>)>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        SELECT lokace,
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

        (config.lokace, vyrazy)
    }))
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
    let (lokace, vyrazy) = match nacti_config(&state.pool).await {
        Ok(Some(config)) => config,
        Ok(None) => {
            eprintln!("scraper_config s id = {} neexistuje", CONFIG_ID);
            (String::new(), Vec::new())
        }
        Err(e) => {
            eprintln!("DB error (scraper_config): {:?}", e);
            (String::new(), Vec::new())
        }
    };

    render_nastaveni(
        &state,
        &lokace,
        &vyrazy,
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

    for (klic, hodnota) in pole {
        match klic.as_str() {
            "lokace" => lokace = hodnota.trim().to_string(),
            "vyraz" => {
                let vyraz = hodnota.trim().to_string();
                if !vyraz.is_empty() && vyrazy.len() < MAX_VYRAZU {
                    vyrazy.push(vyraz);
                }
            }
            _ => {}
        }
    }

    // Ověření lokace proti Nominatimu. Prázdnou neověřujeme — ta uloženou hodnotu nepřepíše.
    // Při neúspěchu se stránka vykreslí rovnou (ne redirect), aby uživatel nepřišel o rozepsané hodnoty.
    if !lokace.is_empty() {
        if lokace.chars().count() > MAX_DELKA_LOKACE {
            return render_nastaveni(&state, &lokace, &vyrazy, false, "lokace").into_response();
        }

        match overit_lokaci(&state, &lokace).await {
            Ok(true) => {}
            Ok(false) => {
                println!("Lokace '{}' nebyla na Nominatimu nalezena", lokace);
                return render_nastaveni(&state, &lokace, &vyrazy, false, "lokace").into_response();
            }
            Err(e) => {
                eprintln!("Ověření lokace '{}' selhalo: {:?}", lokace, e);
                return render_nastaveni(&state, &lokace, &vyrazy, false, "overeni").into_response();
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
        vyraz(9)
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
            eprintln!("Fetch datasetu ve webhooku selhal: {e:?}");
        }
    });
    StatusCode::OK
}

pub async fn fetch_dataset(state: &AppState, dataset_id: &str) -> Result<Vec<Place>> {
    let url = format!("https://api.apify.com/v2/datasets/{}/items",dataset_id);

    let request = state.http.request(Method::GET, url)
        .headers(headers(state)?)
        .query(&[("fields", "title,phone,phoneUnformatted,website,address,city,postalCode,emails,totalScore,reviewsCount,placeId,url"), ("clean", "true"), ("format", "json")]);

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
        INSERT INTO provozovny (place_id, nazev, telefon, web, adresa, mesto, psc, hodnoceni)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        ON CONFLICT (place_id) DO UPDATE SET updated_at = NOW()
        RETURNING id, (xmax = 0) AS "vlozeno!"
        "#,
        place.place_id, place.title, place.phone, place.website,
        place.address, place.city, place.postal_code,
       place.total_score.map(|s| s as f32),
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
            let _ = run_actor(&job_state).await;
        })
    })?;
    Ok(job)
} 

pub async fn run_actor(state: &AppState) -> Result<(), Box<dyn Error>> {

    println!("Spuštění cron jobu, načtení výrazů a lokace");

    let (lokace, vyrazy) = match nacti_config(&state.pool).await {
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

    println!("Scrapuji {:?} v lokaci '{}'", vyrazy, lokace);

    let url: String = format!("https://api.apify.com/v2/actors/{}/runs", state.config.apify_actor_id);

    let json_data: Value = json!({
        "includeWebResults": true,
        "language": "cs",
        "locationQuery": lokace,
        "maxCrawledPlacesPerSearch": 30,
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

pub async fn delete_record(State(state): State<AppState>, Form(params): Form<HashMap<String, String>>) -> impl IntoResponse {
    let id: i32 = match params.get("id").and_then(|id| id.parse().ok()) {
        Some(id) => id,
        None => return Redirect::to("/").into_response(),
    };

    match sqlx::query!("DELETE FROM provozovny WHERE id = $1", id)
        .execute(&state.pool)
        .await
    {
        Ok(_) => Redirect::to("/").into_response(),
        Err(e) => {
            eprintln!("Delete error: {:?}", e);
            Redirect::to("/").into_response()
        }
    }
}

pub async fn download_file(State(state): State<AppState>, Form(pole): Form<Vec<(String, String)>>) -> impl IntoResponse {
    println!("Start serverové akce POST /download");

    let mut format: Option<String> = None;
    let mut razeni: Option<&'static str> = None;
    let mut limit: Option<i64> = None;
    let mut mesto: Option<String> = None;

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
            _ => {}
        }
    }

    let format = format.unwrap_or_else(|| "json".to_string());
    let razeni = razeni.unwrap_or("p.nazev ASC");
    let limit  = limit.unwrap_or(100).clamp(1, 10_000);
    let mesto  = mesto.filter(|s| !s.is_empty());

    let data = match get_data_from_popup(&state.pool, limit, mesto, razeni).await {
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
}

/// POST /kontaktovane — nastaví u jedné provozovny příznak „kontaktované".
pub async fn set_kontaktovane(
    State(state): State<AppState>,
    Form(form): Form<StavForm>,
) -> impl IntoResponse {
    // updated_at se schválně nemění — řadí se podle něj výpis a záznam by přeskočil nahoru.
    match sqlx::query!(
        "UPDATE provozovny SET is_contacted = $2 WHERE id = $1",
        form.id,
        form.hodnota
    )
    .execute(&state.pool)
    .await
    {
        Ok(_) => Redirect::to("/").into_response(),
        Err(e) => {
            eprintln!("DB chyba (is_contacted, id = {}): {:?}", form.id, e);
            Redirect::to("/").into_response()
        }
    }
}

/// POST /smluvene — nastaví u jedné provozovny příznak „smluvené".
pub async fn set_smluvene(
    State(state): State<AppState>,
    Form(form): Form<StavForm>,
) -> impl IntoResponse {
    match sqlx::query!(
        "UPDATE provozovny SET is_closed = $2 WHERE id = $1",
        form.id,
        form.hodnota
    )
    .execute(&state.pool)
    .await
    {
        Ok(_) => Redirect::to("/").into_response(),
        Err(e) => {
            eprintln!("DB chyba (is_closed, id = {}): {:?}", form.id, e);
            Redirect::to("/").into_response()
        }
    }
}

mod config;
mod routes;
mod types; 
use crate::{
    config::Config, routes::{
        apify_webhook, auth_middleware, delete_record, download_file, health, hromadne_smazat,
        html_page, login, login_page, nastaveni_page, nastaveni_save, provozovna_detail,
        run_cron_job, set_kontakt, set_kontaktovane, set_poznamka, set_smluvene, vyber_zmena,
    }
};
use anyhow::Result;
use axum::{Router, middleware, routing::{get, post}};
use reqwest::Client;
use tokio_cron_scheduler::JobScheduler;
use tokio::net::TcpListener;
use sqlx::PgPool;
use tera::Tera;
use lettre::{AsyncSmtpTransport, Tokio1Executor, transport::smtp::authentication::Credentials};
use dashmap::DashMap;
use std::time::Instant;
use std::sync::Arc;
use tower_sessions::{Expiry, SessionManagerLayer, cookie::time::Duration, session_store::ExpiredDeletion};
use tower_sessions_sqlx_store::PostgresStore;

#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub http: Client,
    pub pool: PgPool,
    pub tera: Tera,
    pub smtp: AsyncSmtpTransport<Tokio1Executor>,
    pub sessions: Arc<DashMap<String, Instant>>
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    // Bez inicializace by se logy z axum/sqlx/lettre nikde neobjevily.
    // Úroveň se dá přebít proměnnou RUST_LOG.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=debug".into()),
        )
        .init();

    let config: Config = Config::from_env()?;
    let bind_addr: String = config.bind_addr();

    let http: Client = Client::builder().build()?;

    let pool: PgPool = PgPool::connect(&config.database_url).await?;

    // Migrace jsou zapečené v binárce (`migrations/`), takže nový image si schéma
    // dorovná sám při startu — na produkci se nic nespouští ručně.
    sqlx::migrate!().run(&pool).await?;

    let mut tera: Tera = Tera::default();
    tera.load_from_glob("frontend/**/*")?;
    
    let creds = Credentials::new(
        std::env::var("SMTP_USER")?,
        std::env::var("SMTP_PASSWORD")?,
    );

    let smtp = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&std::env::var("SMTP_HOST").expect("Chybí SMTP_HOST v .env"))? 
        .credentials(creds)
        .build();
    // Sessions žijí v PG (schéma `tower_sessions`), takže přežijí restart/redeploy
    // kontejneru. Tabulku si store založí sám, mimo `sqlx::migrate!`.
    let session_store = PostgresStore::new(pool.clone());
    session_store.migrate().await?;
    tokio::task::spawn(
        session_store
            .clone()
            .continuously_delete_expired(tokio::time::Duration::from_secs(60 * 10)),
    );
    // Přihlášení řeší vlastní cookie `session` + DashMap; tohle je jen úložiště
    // pro stav UI (hromadný výběr), proto jiný název cookie.
    let session_layer = SessionManagerLayer::new(session_store)
        .with_name("vyber")
        .with_secure(true)
        .with_expiry(Expiry::OnInactivity(Duration::hours(24)));

    let sessions = Arc::new(DashMap::new());
    let state: AppState = AppState { config, http, pool, tera, smtp,  sessions};

    let sched: JobScheduler = JobScheduler::new().await?;


    sched.add(run_cron_job(state.clone())?).await?;
    sched.start().await?;

    let protected = Router::new()
    .route("/", get(html_page))
    .route("/provozovna/:id", get(provozovna_detail))
    .route("/delete", post(delete_record))
    .route("/download", post(download_file))
    .route("/vyber", post(vyber_zmena))
    .route("/hromadne/smazat", post(hromadne_smazat))
    .route("/kontaktovane", post(set_kontaktovane))
    .route("/smluvene", post(set_smluvene))
    .route("/poznamka", post(set_poznamka))
    .route("/kontakt", post(set_kontakt))
    .route("/nastaveni", get(nastaveni_page))
    .route("/nastaveni", post(nastaveni_save))
    .route_layer(middleware::from_fn_with_state(state.clone(), auth_middleware));

    let app: axum::Router = axum::Router::new()
        .merge(protected)
        .route("/health", get(health))
        .route("/apify/webhook", post(apify_webhook))
        .route("/login", get(login_page))
        .route("/login", post(login))
        .with_state(state)
        // Musí obalit i /login, jinak by handler neměl kam session zapsat.
        .layer(session_layer);

    let listener: TcpListener = TcpListener::bind(&bind_addr).await?;
    
    println!("Server běží na adrese http://{}", bind_addr);
    
    axum::serve(listener, app).await?;
    
    Ok(())
}

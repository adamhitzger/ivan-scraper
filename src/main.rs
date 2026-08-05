mod config;
mod routes;
mod types; 
use crate::{
    config::Config, routes::{
        apify_webhook, auth_middleware, delete_record, health, html_page, login, login_page,
        nastaveni_page, nastaveni_save, run_cron_job,
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

    let mut tera: Tera = Tera::default();
    tera.load_from_glob("frontend/**/*")?;
    
    let creds = Credentials::new(
        std::env::var("SMTP_USER")?,
        std::env::var("SMTP_PASSWORD")?,
    );

    let smtp = AsyncSmtpTransport::<Tokio1Executor>::relay(&std::env::var("SMTP_HOST")?)? 
        .credentials(creds)
        .build();
    let sessions = Arc::new(DashMap::new());
    let state: AppState = AppState { config, http, pool, tera, smtp,  sessions};

    let sched: JobScheduler = JobScheduler::new().await?;


    sched.add(run_cron_job(state.clone())?).await?;
    sched.start().await?;

    let protected = Router::new()
    .route("/", get(html_page))
    .route("/delete", post(delete_record))
    .route("/nastaveni", get(nastaveni_page))
    .route("/nastaveni", post(nastaveni_save))
    .route_layer(middleware::from_fn_with_state(state.clone(), auth_middleware));

    let app: axum::Router = axum::Router::new()
        .merge(protected)
        .route("/health", get(health))
        .route("/apify/webhook", post(apify_webhook))
        .route("/login", get(login_page))
        .route("/login", post(login))
        .with_state(state);

    let listener: TcpListener = TcpListener::bind(&bind_addr).await?;
    
    println!("Server běží na adrese http://{}", bind_addr);
    
    axum::serve(listener, app).await?;
    
    Ok(())
}

use anyhow::{Context, Result};
use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub apify_token: String,
    pub apify_actor_id: String,
    pub database_url: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            host: env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into()),
            port: env::var("PORT")
                .unwrap_or_else(|_| "3000".into())
                .parse()
                .context("PORT musí být číslo")?,
            apify_token: env::var("APIFY_API_TOKEN").context("APIFY_API_TOKEN chybí")?,
            apify_actor_id: env::var("GOOGLE_MAPS_SCRAPER_TOKEN").context("GOOGLE_MAPS_SCRAPER_TOKEN chybí")?,
            database_url: env::var("DATABASE_URL").context("DATABASE_URL chybí")?
        })
    }
    
    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}
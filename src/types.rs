use serde::{Serialize,Deserialize};
use sqlx::{ FromRow};
#[derive(Debug, Deserialize)]
pub struct ApifyWebhook {
    #[serde(rename = "eventType")]
    pub event_type: String,
    pub resource: ApifyResource,
}

#[derive(Debug, Deserialize)]
pub struct ApifyResource {
    pub id: String,                          
    pub status: String,                      
    #[serde(rename = "defaultDatasetId")]
    pub default_dataset_id: String,          
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Place {
    pub title: String,
    pub phone: Option<String>,
    pub phone_unformatted: Option<String>,
    pub website: Option<String>,
    pub address: Option<String>,
    pub city: Option<String>,
    pub postal_code: Option<String>,
    #[serde(default)]
    pub emails: Vec<String>,
    pub total_score: Option<f64>,
    pub reviews_count: Option<u32>,
    pub place_id: String,
    pub url: Option<String>,
}

#[derive(serde::Serialize, Debug)]
pub struct Provozovna {
    pub id: i32,
    pub place_id: String,
    pub nazev: String,
    pub telefon: Option<String>,
    pub telefon_raw: Option<String>,
    pub web: Option<String>,
    pub adresa: Option<String>,
    pub mesto: Option<String>,
    pub psc: Option<String>,
    pub hodnoceni: Option<f32>,
    pub pocet_recenzi: Option<i32>,
    pub url: Option<String>,
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
    pub emaily: Option<Vec<String>>,
    pub is_contacted: bool,
    pub is_closed: bool,
    /// Volná poznámka z detailu. Prostý text — šablona ho escapuje, nikde se
    /// nerenderuje jako HTML ani markdown.
    pub poznamka: String,
}

#[derive(Debug, FromRow, serde::Serialize)]
pub struct ProvozovnaExport {
    pub id: i32,
    pub nazev: String,
    pub mesto: Option<String>,
    pub telefon: Option<String>,
    pub web: Option<String>,
    pub emaily: Vec<String>,
}

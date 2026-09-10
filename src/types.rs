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
/// Provozovna doplněná o slug pro odkaz na detail. Slug se nepočítá v SQL —
/// převod diakritiky by v Postgresu znamenal rozšíření `unaccent`.
#[derive(Debug, serde::Serialize)]
pub struct ProvozovnaView {
    #[serde(flatten)]
    pub provozovna: Provozovna,
    pub slug: String,
}

impl From<Provozovna> for ProvozovnaView {
    fn from(provozovna: Provozovna) -> Self {
        let slug = slug_provozovny(&provozovna.nazev, provozovna.id);
        Self { provozovna, slug }
    }
}

/// Kanonický slug: `nazev-id`. Závazné je jen `id` na konci — podle něj se
/// řádek dohledává, takže přejmenovaná provozovna nerozbije staré odkazy.
pub fn slug_provozovny(nazev: &str, id: i32) -> String {
    let mut slug = String::with_capacity(nazev.len() + 8);

    // Nejdřív ASCII, pak přepis diakritiky; cokoli zbylo (mezery, interpunkce,
    // emoji) je oddělovač. Pomlčky se nezdvojují a slug jimi nezačíná.
    for znak in nazev.chars() {
        if znak.is_ascii_alphanumeric() {
            slug.push(znak.to_ascii_lowercase());
            continue;
        }

        let prepis = bez_diakritiky(znak);

        if !prepis.is_empty() {
            slug.push_str(&prepis.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }

    let zaklad = slug.trim_matches('-');

    // Název bez písmen a číslic by dal prázdný základ, a tím slug „-12“.
    if zaklad.is_empty() {
        format!("provozovna-{}", id)
    } else {
        format!("{}-{}", zaklad, id)
    }
}

/// ID z konce slugu (`nazev-12` → 12). `None` = slug nemá číselný ocas.
pub fn id_ze_slugu(slug: &str) -> Option<i32> {
    slug.rsplit('-').next()?.parse::<i32>().ok()
}

/// Přepis znaků, které `is_ascii_alphanumeric` zahodí, ale ve slugu mají zůstat.
/// Pokrývá češtinu a slovenštinu, u ostatních znaků se spoléhá na oddělovač.
fn bez_diakritiky(znak: char) -> &'static str {
    match znak {
        'á' | 'à' | 'â' | 'ä' | 'ą' | 'ā' => "a",
        'Á' | 'À' | 'Â' | 'Ä' | 'Ą' | 'Ā' => "A",
        'č' | 'ć' | 'ç' => "c",
        'Č' | 'Ć' | 'Ç' => "C",
        'ď' | 'đ' => "d",
        'Ď' | 'Đ' => "D",
        'é' | 'ě' | 'è' | 'ê' | 'ë' | 'ę' => "e",
        'É' | 'Ě' | 'È' | 'Ê' | 'Ë' | 'Ę' => "E",
        'í' | 'ì' | 'î' | 'ï' => "i",
        'Í' | 'Ì' | 'Î' | 'Ï' => "I",
        'ĺ' | 'ľ' | 'ł' => "l",
        'Ĺ' | 'Ľ' | 'Ł' => "L",
        'ň' | 'ń' => "n",
        'Ň' | 'Ń' => "N",
        'ó' | 'ô' | 'ö' | 'ò' | 'ő' => "o",
        'Ó' | 'Ô' | 'Ö' | 'Ò' | 'Ő' => "O",
        'ř' => "r",
        'Ř' => "R",
        'š' | 'ś' => "s",
        'Š' | 'Ś' => "S",
        'ť' => "t",
        'Ť' => "T",
        'ú' | 'ů' | 'ü' | 'ù' | 'û' | 'ű' => "u",
        'Ú' | 'Ů' | 'Ü' | 'Ù' | 'Û' | 'Ű' => "U",
        'ý' | 'ÿ' => "y",
        'Ý' => "Y",
        'ž' | 'ź' | 'ż' => "z",
        'Ž' | 'Ź' | 'Ż' => "Z",
        'ß' => "ss",
        _ => "",
    }
}

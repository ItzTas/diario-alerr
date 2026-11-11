use chrono::{DateTime, Utc};

pub const WELCOME_ALERT_ID: &str = "welcome";
pub(crate) const SITE_URL: &str = "https://diarios.al.rr.leg.br";
pub const GAZETTE_NAME: &str = "ALE/RR Official Gazette";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edition {
    pub id: i64,
    pub published: DateTime<Utc>,
    pub url: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    pub id: String,
    pub title: String,
    pub link: String,
    pub body: String,
    pub published: DateTime<Utc>,
}

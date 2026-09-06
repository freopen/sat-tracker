use anyhow::Result;
use figment::{
    Figment,
    providers::{Format, Yaml},
};
use regex::Regex;
use serde::{Deserialize, Deserializer, de::Error};

#[derive(Clone, Deserialize)]
pub struct Config {
    #[serde(deserialize_with = "deserialize_regex")]
    pub ok_regex: Regex,
    #[serde(deserialize_with = "deserialize_regex")]
    pub finished_regex: Regex,
    pub owner_chat_id: i64,
    pub safety_chat_id: i64,
    #[serde(default = "default_telegram_api_url")]
    pub telegram_api_url: String,
    #[serde(default)]
    pub telegram_webhook_url: String,
    pub telegram_bot_token: String,
}

impl Config {
    pub fn load() -> Result<Self> {
        let config: Self = Figment::new()
            .merge(Yaml::file("./config.yaml"))
            .extract()?;

        anyhow::ensure!(
            !config.ok_regex.as_str().is_empty(),
            "ok_regex must not be empty"
        );
        anyhow::ensure!(
            !config.finished_regex.as_str().is_empty(),
            "finished_regex must not be empty"
        );

        Ok(config)
    }
}

fn default_telegram_api_url() -> String {
    "https://api.telegram.org".to_owned()
}

fn deserialize_regex<'de, D>(deserializer: D) -> std::result::Result<Regex, D::Error>
where
    D: Deserializer<'de>,
{
    let pattern = String::deserialize(deserializer)?;
    Regex::new(&pattern).map_err(D::Error::custom)
}

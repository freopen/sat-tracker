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

        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.ok_regex.as_str().is_empty(),
            "ok_regex must not be empty"
        );
        anyhow::ensure!(
            !self.finished_regex.as_str().is_empty(),
            "finished_regex must not be empty"
        );
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn value(ok_regex: &str, finished_regex: &str) -> serde_json::Value {
        serde_json::json!({
            "ok_regex": ok_regex,
            "finished_regex": finished_regex,
            "owner_chat_id": 10,
            "safety_chat_id": 20,
            "telegram_bot_token": "test"
        })
    }

    #[test]
    fn config_deserialization_applies_transport_defaults() {
        let config: Config = serde_json::from_value(value("OK", "FINISHED")).unwrap();
        assert_eq!(config.telegram_api_url, "https://api.telegram.org");
        assert!(config.telegram_webhook_url.is_empty());
    }

    #[test]
    fn config_rejects_invalid_and_empty_mail_patterns() {
        assert!(serde_json::from_value::<Config>(value("[", "FINISHED")).is_err());
        let mut empty = Config {
            ok_regex: Regex::new("").unwrap(),
            finished_regex: Regex::new("FINISHED").unwrap(),
            owner_chat_id: 10,
            safety_chat_id: 20,
            telegram_api_url: String::new(),
            telegram_webhook_url: String::new(),
            telegram_bot_token: "test".to_owned(),
        };
        assert!(empty.validate().is_err());
        empty.ok_regex = Regex::new("OK").unwrap();
        empty.finished_regex = Regex::new("").unwrap();
        assert!(empty.validate().is_err());
    }
}

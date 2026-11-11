use std::time::Duration;

use async_trait::async_trait;

use crate::sys::Http;

pub struct ReqwestHttp {
    client: reqwest::Client,
}

impl ReqwestHttp {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) Mika/0.1")
                .timeout(Duration::from_secs(15))
                .build()
                .expect("reqwest client"),
        }
    }
}

#[async_trait]
impl Http for ReqwestHttp {
    async fn get(&self, url: &str) -> Result<String, String> {
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("нет связи: {}", e.without_url()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("сайт ответил {status}"));
        }
        resp.text()
            .await
            .map_err(|e| format!("ответ не прочитан: {}", e.without_url()))
    }
}

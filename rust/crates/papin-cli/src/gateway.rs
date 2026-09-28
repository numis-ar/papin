use crate::config::CliConfig;
use crate::http::{error_message, HttpClient, Result};
use serde_json::json;

#[derive(Debug, Clone)]
pub struct AgentEntry {
    pub id: String,
    pub name: String,
    pub state: String,
}

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub bases: Vec<String>,
    pub seeds: Vec<String>,
}

/// Gateway REST client (`/api/v1/*`).
pub struct Gateway {
    http: HttpClient,
}

impl Gateway {
    pub fn new(config: &CliConfig) -> Result<Gateway> {
        Ok(Gateway {
            http: HttpClient::new(&config.url, &config.token)?,
        })
    }

    pub async fn list_agents(&self) -> Result<Vec<AgentEntry>> {
        let (status, body) = self.http.get("/api/v1/agents").await?;
        if status != 200 {
            return Err(error_message(status, &body).into());
        }
        Ok(body
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|a| AgentEntry {
                        id: a["id"].as_str().unwrap_or("?").to_string(),
                        name: a["name"].as_str().unwrap_or("").to_string(),
                        state: a["state"].as_str().unwrap_or("stopped").to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn config_catalog(&self) -> Result<Catalog> {
        let (status, body) = self.http.get("/api/v1/config-catalog").await?;
        if status != 200 {
            return Err(error_message(status, &body).into());
        }
        let names = |key: &str| -> Vec<String> {
            body[key]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v["name"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };
        Ok(Catalog {
            bases: names("bases"),
            seeds: names("seeds"),
        })
    }

    pub async fn create_agent(
        &self,
        name: &str,
        base: &str,
        seed: Option<&str>,
    ) -> Result<AgentEntry> {
        let mut config = json!({"base": base});
        if let Some(seed) = seed {
            config["seed"] = json!(seed);
        }
        let (status, body) = self
            .http
            .post("/api/v1/agents", &json!({"name": name, "config": config}))
            .await?;
        if !(200..300).contains(&status) {
            return Err(error_message(status, &body).into());
        }
        Ok(AgentEntry {
            id: body["id"].as_str().unwrap_or("?").to_string(),
            name: body["name"].as_str().unwrap_or(name).to_string(),
            state: "stopped".into(),
        })
    }

    pub async fn delete_agent(&self, id: &str, force: bool) -> Result<()> {
        let path = if force {
            format!("/api/v1/agents/{id}?force=true")
        } else {
            format!("/api/v1/agents/{id}")
        };
        let (status, body) = self.http.delete(&path).await?;
        if status == 404 {
            return Err(error_message(status, &body).into());
        }
        if !(200..300).contains(&status) {
            return Err(error_message(status, &body).into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_client_parses_host_port() {
        let c = HttpClient::new("http://10.77.0.1:8080", "tok").unwrap();
        assert_eq!(c.host(), "10.77.0.1");
        assert_eq!(c.port(), 8080);
        let c = HttpClient::new("http://example.com", "tok").unwrap();
        assert_eq!(c.port(), 80);
    }

    #[test]
    fn response_parsing() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";
        let (status, body) = crate::http::parse_response(raw).unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, json!({}));
        let raw = b"HTTP/1.1 401 Unauthorized\r\n\r\n";
        let (status, body) = crate::http::parse_response(raw).unwrap();
        assert_eq!(status, 401);
        assert_eq!(body, serde_json::Value::Null);
    }
}

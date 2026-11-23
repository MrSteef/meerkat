//! Outbound HTTP transport. Scheduling and database state live elsewhere.
use std::net::IpAddr;
use std::time::Duration;

use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, redirect::Policy};
use serde_json::Value;
use url::Url;

#[derive(Debug, Clone)]
pub struct SendOutcome {
    pub status: Option<u16>,
    pub error: Option<String>,
}

/// V1 supports HTTPS or literal-loopback HTTP, JSON bodies, and user headers.
/// Reject reserved headers to avoid ambiguous idempotency/content type.
///
/// # Errors
/// Returns a validation message for an invalid endpoint, header or payload.
pub fn validate_endpoint(url: &str, headers: &str, body: &str) -> Result<(), String> {
    let address = Url::parse(url).map_err(|e| format!("Invalid webhook URL: {e}"))?;
    if address.username() != "" || address.password().is_some() {
        return Err("Do not put credentials in the URL; configure headers instead".to_owned());
    }
    match address.scheme() {
        "https" => (),
        "http" if address.host_str().is_some_and(is_loopback) => (),
        _ => {
            return Err(
                "Webhook must use HTTPS, or HTTP with literal 127.0.0.1 / [::1]".to_owned(),
            );
        }
    }
    let raw: Value =
        serde_json::from_str(headers).map_err(|e| format!("Invalid headers JSON: {e}"))?;
    let object = raw.as_object().ok_or("Headers must be a JSON object")?;
    for (name, value) in object {
        if matches!(
            name.to_ascii_lowercase().as_str(),
            "content-type" | "idempotency-key" | "x-meerkat-message-id" | "host" | "content-length"
        ) {
            return Err(format!("Header {name} is managed by Meerkat"));
        }
        let _ = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| format!("Invalid HTTP header name {name}"))?;
        let text = value
            .as_str()
            .ok_or_else(|| format!("Header {name} must be a string"))?;
        let _ =
            HeaderValue::from_str(text).map_err(|_| format!("Invalid value for header {name}"))?;
    }
    serde_json::from_str::<Value>(body).map_err(|e| format!("Invalid JSON payload: {e}"))?;
    Ok(())
}

fn is_loopback(host: &str) -> bool {
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

/// Async, stateless request sender. Never follows redirects or uses proxies.
pub struct Sender {
    client: Client,
}

impl Sender {
    /// # Errors
    /// Returns HTTP client construction errors.
    pub fn new(timeout: Duration) -> Result<Self, reqwest::Error> {
        let client = Client::builder()
            .timeout(timeout)
            .redirect(Policy::none())
            .no_proxy()
            .build()?;
        Ok(Self { client })
    }

    /// Sends the immutable request supplied by the outbox.
    /// Network failure is represented as an outcome rather than a panic.
    pub async fn send(
        &self,
        message_id: &str,
        url: &str,
        headers: &str,
        body: &str,
    ) -> SendOutcome {
        if let Err(error) = validate_endpoint(url, headers, body) {
            return SendOutcome {
                status: None,
                error: Some(error),
            };
        }
        let parsed = serde_json::from_str::<serde_json::Map<String, Value>>(headers);
        let Ok(header_values) = parsed else {
            return SendOutcome {
                status: None,
                error: Some("Invalid saved header data".to_owned()),
            };
        };
        let mut request_headers = HeaderMap::new();
        request_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Ok(value) = HeaderValue::from_str(message_id) {
            request_headers.insert(HeaderName::from_static("idempotency-key"), value);
        }
        for (name, value) in &header_values {
            let (Ok(header_name), Some(text)) =
                (HeaderName::from_bytes(name.as_bytes()), value.as_str())
            else {
                return SendOutcome {
                    status: None,
                    error: Some("Invalid saved headers".to_owned()),
                };
            };
            if let Ok(header_value) = HeaderValue::from_str(text) {
                request_headers.insert(header_name, header_value);
            }
        }
        match self
            .client
            .post(url)
            .headers(request_headers)
            .body(body.to_owned())
            .send()
            .await
        {
            Ok(reply) => {
                let status = reply.status().as_u16();
                let error = if (200..300).contains(&status) {
                    None
                } else {
                    Some(format!("HTTP {status}"))
                };
                SendOutcome {
                    status: Some(status),
                    error,
                }
            }
            Err(error) => SendOutcome {
                status: None,
                error: Some(error.without_url().to_string()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_local_and_https() {
        assert!(
            validate_endpoint("http://127.0.0.1:5000/hook", "{}", "{\"text\":\"Hello\"}").is_ok()
        );
        assert!(validate_endpoint("https://example.org/hook", "{}", "{}").is_ok());
    }
    #[test]
    fn refuses_nonlocal_http_and_reserved_headers() {
        assert!(validate_endpoint("http://192.168.1.10/hook", "{}", "{}").is_err());
        assert!(
            validate_endpoint(
                "http://127.0.0.1/hook",
                "{\"Idempotency-Key\":\"bogus\"}",
                "{}"
            )
            .is_err()
        );
    }
}

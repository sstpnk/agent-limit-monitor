use std::io::Read;
use std::sync::OnceLock;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::models::PollError;

fn agent() -> Result<&'static ureq::Agent, PollError> {
    static AGENT: OnceLock<Option<ureq::Agent>> = OnceLock::new();
    let cell = AGENT.get_or_init(|| {
        let tls = native_tls::TlsConnector::new().ok()?;
        Some(
            ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(20))
                .tls_connector(std::sync::Arc::new(tls))
                .build(),
        )
    });
    cell.as_ref().ok_or(PollError::Request)
}

fn map_error(error: ureq::Error) -> PollError {
    match error {
        ureq::Error::Status(code, _) if code == 401 || code == 403 => PollError::Auth,
        _ => PollError::Request,
    }
}

pub fn get_json<T: DeserializeOwned>(
    url: &str,
    headers: &[(&str, &str)],
) -> Result<T, PollError> {
    let agent = agent()?;
    let mut request = agent.get(url);
    for (name, value) in headers {
        request = request.set(name, value);
    }
    let response = request.call().map_err(map_error)?;
    response.into_json::<T>().map_err(|_| PollError::Parse)
}

pub fn get_bytes(url: &str) -> Result<Vec<u8>, PollError> {
    let agent = agent()?;
    let response = agent
        .get(url)
        .set("User-Agent", "agent-limit-monitor")
        .set("Accept", "application/octet-stream")
        .call()
        .map_err(map_error)?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(32 * 1024 * 1024)
        .read_to_end(&mut bytes)
        .map_err(|_| PollError::Request)?;
    Ok(bytes)
}

pub fn post_json<B: Serialize, T: DeserializeOwned>(
    url: &str,
    headers: &[(&str, &str)],
    body: &B,
) -> Result<T, PollError> {
    let agent = agent()?;
    let mut request = agent.post(url);
    for (name, value) in headers {
        request = request.set(name, value);
    }
    let response = request.send_json(body).map_err(map_error)?;
    response.into_json::<T>().map_err(|_| PollError::Parse)
}

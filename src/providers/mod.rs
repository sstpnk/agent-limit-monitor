pub mod antigravity;
pub mod chatgpt;
pub mod claude;
pub mod copilot;
pub mod deepseek;
pub mod zai;

use crate::config::Config;
use crate::models::{ProviderId, Reading};

pub fn poll_all(config: &Config) -> Vec<Reading> {
    let mut readings = Vec::with_capacity(ProviderId::ALL.len());
    for provider in ProviderId::ALL {
        if !is_enabled(config, provider) {
            continue;
        }
        let reading = match provider {
            ProviderId::ChatGpt => chatgpt::poll(config),
            ProviderId::Zai => zai::poll(config),
            ProviderId::DeepSeek => deepseek::poll(config),
            ProviderId::Claude => claude::poll(config),
            ProviderId::Antigravity => antigravity::poll(config),
            ProviderId::Copilot => copilot::poll(config),
        };
        readings.push(reading);
    }
    if readings.is_empty() {
        readings.push(Reading::error(
            ProviderId::ChatGpt,
            crate::models::State::Missing,
            "Нет включённых моделей",
        ));
    }
    readings
}

pub fn is_enabled(config: &Config, provider: ProviderId) -> bool {
    match provider {
        ProviderId::ChatGpt => config.providers.chatgpt.enabled,
        ProviderId::Zai => config.providers.zai.enabled,
        ProviderId::DeepSeek => config.providers.deepseek.enabled,
        ProviderId::Claude => config.providers.claude.enabled,
        ProviderId::Antigravity => config.providers.antigravity.enabled,
        ProviderId::Copilot => config.providers.copilot.enabled,
    }
}

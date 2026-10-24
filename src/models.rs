use std::time::SystemTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderId {
    ChatGpt = 0,
    Zai = 1,
    DeepSeek = 2,
    Claude = 3,
    Antigravity = 4,
    Copilot = 5,
}

impl ProviderId {
    pub const ALL: [ProviderId; 6] = [
        Self::ChatGpt,
        Self::Zai,
        Self::DeepSeek,
        Self::Claude,
        Self::Antigravity,
        Self::Copilot,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::ChatGpt => "ChatGPT",
            Self::Zai => "Z.AI",
            Self::DeepSeek => "DeepSeek",
            Self::Claude => "Claude",
            Self::Antigravity => "Antigravity",
            Self::Copilot => "Copilot",
        }
    }

    pub const fn short_label(self) -> &'static str {
        match self {
            Self::ChatGpt => "GPT",
            Self::Zai => "Z.AI",
            Self::DeepSeek => "DS",
            Self::Claude => "Claude",
            Self::Antigravity => "AG",
            Self::Copilot => "GH",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Percent(f64),
    Credits(u64),
    Money { cents: i64, currency: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Loading,
    Ok,
    Warning,
    Critical,
    Stale,
    Auth,
    Missing,
    Error,
}

impl State {
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Auth | Self::Missing | Self::Error)
    }

    pub fn is_missing(&self) -> bool {
        matches!(self, Self::Auth | Self::Missing)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WindowInfo {
    pub label: String,
    pub remaining: f64,
    pub reset_at: Option<SystemTime>,
    pub limit_reached: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DetailLine {
    pub label: String,
    pub value: String,
    pub reset: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    pub provider: ProviderId,
    pub value: Value,
    pub reset_at: Option<SystemTime>,
    pub state: State,
    pub title: String,
    pub windows: Vec<WindowInfo>,
    pub detail: Vec<DetailLine>,
}

impl Reading {
    pub fn placeholder(provider: ProviderId, state: State) -> Self {
        Self {
            provider,
            value: Value::Percent(0.0),
            reset_at: None,
            state,
            title: provider.label().to_string(),
            windows: Vec::new(),
            detail: Vec::new(),
        }
    }

    pub fn error(provider: ProviderId, state: State, message: impl Into<String>) -> Self {
        Self {
            provider,
            value: Value::Percent(0.0),
            reset_at: None,
            state,
            title: provider.label().to_string(),
            windows: Vec::new(),
            detail: vec![DetailLine {
                label: String::new(),
                value: message.into(),
                reset: None,
            }],
        }
    }

    pub fn has_value(&self) -> bool {
        matches!(
            self.state,
            State::Ok | State::Warning | State::Critical | State::Stale
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollError {
    Auth,
    Request,
    Parse,
}

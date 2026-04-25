use thiserror::Error;

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("timeout")]
    Timeout,

    #[error("browser not available: {0}")]
    BrowserUnavailable(String),

    #[error("browser error: {0}")]
    Browser(String),

    #[error("cloudflare bypass error: {0}")]
    CloudflareBypass(String),

    #[error("cloudflare challenge detected and not bypassed")]
    CloudflareChallenge,

    #[error("readability error: {0}")]
    Readability(String),

    #[error("non-success status: {0}")]
    Status(u16),

    #[error("invalid url: {0}")]
    InvalidUrl(String),

    #[error("ssrf blocked: {0}")]
    SsrfBlocked(String),

    #[error("other: {0}")]
    Other(String),
}

pub type FetchResult<T> = Result<T, FetchError>;

# tokimo-package-web-fetch

Unified web page fetcher for Rust — HTTP, headless browser (Chrome / Chromium), and Cloudflare bypass (FlareSolverr), with optional Readability denoising.

## Features

- **Three fetch channels** — choose per request or let the fetcher auto-degrade gracefully:
  - `HTTP` — plain `reqwest` GET with custom UA
  - `Browser` — calls a local headless Chrome / Chromium (`--headless=new --dump-dom`) to execute JavaScript before reading HTML.
  - `CloudflareBypass` — routes through [FlareSolverr](https://github.com/FlareSolverr/FlareSolverr) to solve Cloudflare challenges
- **Readability denoising** — strips ads / nav / boilerplate from HTML via [`dom_smoothie`](https://crates.io/crates/dom_smoothie), returning clean `DenoisedArticle { title, text_content, content, … }`
- **SSRF protection** — blocks fetches to private / loopback / link-local address ranges
- **Graceful fallback** — if `Browser` mode is requested but no Chrome is available, falls back to HTTP with a `WARN` log

## Usage

```rust
use tokimo_web_fetch::{WebFetcher, FetchMode, FetchOptions, Denoise};

let fetcher = WebFetcher::builder().build();

let response = fetcher
    .fetch_with(
        "https://example.com",
        FetchOptions {
            mode: FetchMode::Http,
            denoise: Denoise::Readability,
            ..Default::default()
        },
    )
    .await?;

println!("{}", response.denoised.unwrap().text_content);
```

## License

MIT

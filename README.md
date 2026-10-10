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

## Raw responses behind Cloudflare

`CloudflareBypassClient::fetch_raw(url, headers)` first sends a normal HTTP GET. Only a response marked `cf-mitigated: challenge` invokes FlareSolverr. The solver receives `returnOnlyCookies: true` with a 60-second solve budget and a 65-second HTTP request timeout; its scoped cookies and User-Agent are then used to replay the original URL. The returned `reqwest::Response` contains the actual status, headers and body, including XML, conditional `304` responses and `Retry-After`. Browser-rendered HTML and FlareSolverr's synthetic status are never substituted for the target response. A second challenge is returned to the caller without another solve attempt.

Inject a client using `CloudflareBypassClient::with_client`. Redirects, proxies, TLS and target HTTP timeouts remain controlled by that client. Callers validating each redirect must disable automatic redirects. Callers also enforce an overall timeout and streamed body limit. Clearance cookies are scoped with a cookie jar for each solve and are not persisted across calls.

Pass an explicitly configured FlareSolverr URL to the constructor. A missing URL causes an explicit error when a challenge is encountered; ordinary responses continue to work. The solver and HTTP client must use the same public egress IP. Cookie replay does not guarantee access when a site additionally binds clearance to browser transport fingerprints. The solver's browser follows redirects itself; its network access must be restricted by deployment policy.

## License

MIT

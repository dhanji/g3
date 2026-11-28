//! Web research via Anthropic's server-side `web_search` / `web_fetch` tools.
//!
//! WHY THIS EXISTS
//! ---------------
//! The original `research` tool spawned a browser-driving `scout` agent:
//! p50 ~10 minutes a question, bot-walled, and killed outright if the parent's
//! turn ended (measured 2026-09-08, see butler `skills/g3-maintenance`). The
//! Messages API has search and fetch built in: Anthropic runs the search and
//! reads the pages on its side, so there is no browser and no bot wall. One
//! HTTP call, 5–60 seconds, with citations.
//!
//! This is a port of butler's `tools/ask_web.py`, which ran in production for
//! two weeks before being moved here. That script was deleted 2026-11-28: this
//! is now the only engine.
//!
//! DESIGN: a SEPARATE API call, not web search enabled on the main
//! conversation. Each search drops ~10k tokens of results into the context;
//! on the main conversation the agent would re-read them every turn at its own
//! model's price. Here only the compact answer + sources come back.
//!
//! CREDENTIALS: whatever Anthropic provider the agent already uses, sent to the
//! same endpoint (`ANTHROPIC_BASE_URL` → e.g. a local key proxy). No new key.
//!
//! HARD CAPS (so a runaway question can't run up a bill): `MAX_SEARCHES_CAP`
//! searches and `MAX_CONTINUATIONS` resumes after `pause_turn`. The search
//! budget is counted across resumes; each resume gets only what is left.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

use g3_config::{Config, ProviderConfigRef};

pub const QUICK_MODEL: &str = "claude-sonnet-5-5";
pub const DEEP_MODEL: &str = "claude-opus-5-5";

pub const MAX_SEARCHES_CAP: u32 = 25;
pub const MAX_FETCHES_CAP: u32 = 10;
pub const MAX_CONTINUATIONS: usize = 4;
const SEARCH_USD: f64 = 0.01;

const API_VERSION: &str = "2023-06-01";
const FETCH_BETA: &str = "web-fetch-2025-09-10";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// USD per million tokens (input, output). Kept in step with butler's
/// `tools/token_cost.py` PRICES. An unknown model is priced at the MOST expensive
/// row: under-reporting a bill is the error nobody notices.
const PRICES: &[(&str, f64, f64)] = &[
    ("claude-opus-5", 5.00, 25.00),
    ("claude-opus-5-5", 4.00, 20.00),
    ("claude-sonnet-5", 2.00, 10.00),
    ("claude-sonnet-5-5", 2.00, 10.00),
    ("claude-sonnet-4-5", 3.00, 15.00),
    ("claude-haiku-4-5", 1.00, 5.00),
    ("claude-haiku-5-5", 0.50, 2.50),
    ("claude-fable-5-1", 10.00, 50.00),
    ("claude-fable-5-2", 8.00, 40.00),
];
const FALLBACK_PRICE: (f64, f64) = (10.00, 50.00);

const TOOL_BLOCK_TYPES: &[&str] = &["server_tool_use", "web_search_tool_result", "web_fetch_tool_result"];

fn system_prompt() -> String {
    format!(
        "You are a web research assistant answering a question for a software agent. \
         Today's date is {}. Search the web and prefer primary/official sources \
         (official docs, specs, changelogs, source repositories, government sites). \
         Answer concisely: lead with the direct answer, then supporting detail, with \
         concrete specifics (versions, dates, names, figures, URLs) where they matter. \
         If sources conflict or the answer is uncertain, say so plainly. Never pad.",
        chrono::Local::now().format("%Y-%m-%d")
    )
}

/// One research question, fully resolved.
#[derive(Debug, Clone)]
pub struct WebQuery {
    pub question: String,
    pub model: String,
    pub max_searches: u32,
    pub max_fetches: u32,
    pub max_tokens: u32,
    pub domains: Vec<String>,
}

impl WebQuery {
    /// Build from the tool's arguments: `query` (required), `deep`, `url`, `domains`.
    pub fn from_args(args: &Value) -> Result<Self> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("Missing required 'query' parameter"))?;
        let deep = args.get("deep").and_then(|v| v.as_bool()).unwrap_or(false);
        let question = match args.get("url").and_then(|v| v.as_str()).map(str::trim) {
            Some(url) if !url.is_empty() => format!("Fetch {} and answer from it: {}", url, query),
            _ => query.to_string(),
        };
        // Accept a JSON array or a comma-separated string — models produce both.
        let domains: Vec<String> = match args.get("domains") {
            Some(Value::Array(a)) => a.iter().filter_map(|d| d.as_str()).map(String::from).collect(),
            Some(Value::String(s)) => s.split(',').map(String::from).collect(),
            _ => Vec::new(),
        }
        .into_iter()
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
        .collect();
        Ok(Self {
            question,
            model: if deep { DEEP_MODEL } else { QUICK_MODEL }.to_string(),
            max_searches: clamp(if deep { 15 } else { 5 }, MAX_SEARCHES_CAP),
            max_fetches: clamp(if deep { 6 } else { 3 }, MAX_FETCHES_CAP),
            max_tokens: if deep { 16000 } else { 6000 },
            domains,
        })
    }
}

/// Clamp a requested tool budget into [1, cap].
pub fn clamp(n: u32, cap: u32) -> u32 {
    n.clamp(1, cap)
}

/// The API key of the Anthropic provider the agent already uses: the default
/// provider if it is Anthropic, else the first `[providers.anthropic.*]` entry
/// (by name, so the choice is deterministic). `None` = no Anthropic access;
/// the caller falls back to the browser scout.
pub fn resolve_api_key(config: &Config) -> Option<String> {
    if let Ok(ProviderConfigRef::Anthropic(c)) = config.get_default_provider_config() {
        return Some(c.api_key.clone());
    }
    let mut names: Vec<&String> = config.providers.anthropic.keys().collect();
    names.sort();
    names
        .first()
        .and_then(|n| config.providers.anthropic.get(*n))
        .map(|c| c.api_key.clone())
}

fn build_tools(max_searches: u32, max_fetches: u32, domains: &[String]) -> Value {
    let mut search = json!({"type": "web_search_20250305", "name": "web_search", "max_uses": max_searches});
    if !domains.is_empty() {
        search["allowed_domains"] = json!(domains);
    }
    let mut tools = vec![search];
    if max_fetches > 0 {
        tools.push(json!({"type": "web_fetch_20250910", "name": "web_fetch",
                          "max_uses": max_fetches, "citations": {"enabled": true}}));
    }
    Value::Array(tools)
}

/// Sends one Messages API payload. A trait so tests can inject responses.
#[async_trait]
pub trait Poster: Send + Sync {
    async fn post(&self, payload: &Value) -> Result<Value>;
}

/// The real poster: same endpoint as the Anthropic provider (honours
/// `ANTHROPIC_BASE_URL`, which is how proxied agents reach the API).
pub struct HttpPoster {
    client: reqwest::Client,
    url: String,
    api_key: String,
}

impl HttpPoster {
    pub fn new(api_key: String) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| anyhow!("Failed to create HTTP client: {}", e))?;
        Ok(Self { client, url: g3_providers::anthropic::anthropic_api_url(), api_key })
    }
}

#[async_trait]
impl Poster for HttpPoster {
    async fn post(&self, payload: &Value) -> Result<Value> {
        let resp = self
            .client
            .post(&self.url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .header("anthropic-beta", FETCH_BETA)
            .header("content-type", "application/json")
            .json(payload)
            .send()
            .await
            .map_err(|e| {
                let proxied = std::env::var("ANTHROPIC_BASE_URL").map(|v| !v.trim().is_empty()).unwrap_or(false);
                if e.is_connect() && proxied {
                    anyhow!("endpoint {} not reachable (ANTHROPIC_BASE_URL is set — is the key proxy running?): {}", self.url, e)
                } else if e.is_timeout() {
                    anyhow!("web research request to {} timed out after {}s", self.url, REQUEST_TIMEOUT.as_secs())
                } else {
                    anyhow!("network error calling {}: {}", self.url, e)
                }
            })?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| anyhow!("failed reading response: {}", e))?;
        if !status.is_success() {
            let body: String = text.chars().take(500).collect();
            return Err(anyhow!("HTTP {} from {}: {}", status.as_u16(), self.url, body));
        }
        let data: Value = serde_json::from_str(&text)
            .map_err(|e| anyhow!("unparseable API response ({}): {}", e, text.chars().take(200).collect::<String>()))?;
        Ok(data)
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Totals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub searches: u32,
    pub fetches: u32,
}

#[derive(Debug, Clone)]
pub struct WebResult {
    pub content: Vec<Value>,
    pub totals: Totals,
    pub stop_reason: Option<String>,
    pub model: String,
}

fn u64_at(v: &Value, path: &[&str]) -> u64 {
    let mut cur = v;
    for p in path {
        match cur.get(p) {
            Some(n) => cur = n,
            None => return 0,
        }
    }
    cur.as_u64().unwrap_or(0)
}

/// Run the question, resuming after `pause_turn` within the caps.
pub async fn run(q: &WebQuery, poster: &dyn Poster) -> Result<WebResult> {
    let system = system_prompt();
    let user = json!({"role": "user", "content": q.question});
    let mut content: Vec<Value> = Vec::new();
    let mut totals = Totals::default();
    let mut stop = None;
    for _ in 0..=MAX_CONTINUATIONS {
        let remaining = q.max_searches.saturating_sub(totals.searches);
        if remaining == 0 && !content.is_empty() {
            break;
        }
        let mut messages = vec![user.clone()];
        if !content.is_empty() {
            // Resume: hand back EVERYTHING the assistant has produced so far,
            // unchanged, as one trailing assistant turn. (Only the latest
            // response would lose the first round's work on a second pause.)
            messages.push(json!({"role": "assistant", "content": content}));
        }
        let payload = json!({
            "model": q.model, "max_tokens": q.max_tokens, "system": system,
            "messages": messages,
            "tools": build_tools(remaining.max(1), q.max_fetches, &q.domains),
        });
        let data = poster.post(&payload).await?;
        if data.get("type").and_then(|t| t.as_str()) == Some("error") {
            return Err(anyhow!("API error: {}", data.get("error").cloned().unwrap_or(Value::Null)));
        }
        if let Some(blocks) = data.get("content").and_then(|c| c.as_array()) {
            content.extend(blocks.iter().cloned());
        }
        let u = data.get("usage").cloned().unwrap_or(Value::Null);
        totals.input_tokens += u64_at(&u, &["input_tokens"])
            + u64_at(&u, &["cache_read_input_tokens"])
            + u64_at(&u, &["cache_creation_input_tokens"]);
        totals.output_tokens += u64_at(&u, &["output_tokens"]);
        totals.searches += u64_at(&u, &["server_tool_use", "web_search_requests"]) as u32;
        totals.fetches += u64_at(&u, &["server_tool_use", "web_fetch_requests"]) as u32;
        stop = data.get("stop_reason").and_then(|s| s.as_str()).map(String::from);
        if stop.as_deref() != Some("pause_turn") {
            break;
        }
    }
    Ok(WebResult { content, totals, stop_reason: stop, model: q.model.clone() })
}

fn block_type(b: &Value) -> &str {
    b.get("type").and_then(|t| t.as_str()).unwrap_or("")
}

fn texts<'a>(blocks: impl Iterator<Item = &'a Value>) -> String {
    blocks
        .filter(|b| block_type(b) == "text")
        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
        .collect()
}

/// Text written AFTER the last tool block — the model's conclusion. Text in
/// between tool calls is narration ("let me check…") and is dropped. If
/// nothing follows the last tool block, fall back to all text.
pub fn final_answer(content: &[Value]) -> String {
    let start = content
        .iter()
        .rposition(|b| TOOL_BLOCK_TYPES.contains(&block_type(b)))
        .map(|i| i + 1)
        .unwrap_or(0);
    let tail = texts(content[start..].iter());
    if tail.trim().is_empty() {
        texts(content.iter()).trim().to_string()
    } else {
        tail.trim().to_string()
    }
}

/// Ordered, de-duplicated (url, title) pairs: cited first, then every search/fetch hit.
pub fn sources(content: &[Value]) -> (Vec<(String, String)>, Vec<(String, String)>) {
    let mut seen = std::collections::HashSet::new();
    let mut cited = Vec::new();
    let mut hits = Vec::new();
    let mut add = |bucket: &mut Vec<(String, String)>, url: Option<&str>, title: Option<&str>| {
        if let Some(u) = url.filter(|u| !u.is_empty()) {
            if seen.insert(u.to_string()) {
                bucket.push((u.to_string(), title.unwrap_or("").trim().to_string()));
            }
        }
    };
    let s = |v: &Value, k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
    for b in content.iter().filter(|b| block_type(b) == "text") {
        for c in b.get("citations").and_then(|c| c.as_array()).into_iter().flatten() {
            add(&mut cited, s(c, "url").as_deref(), s(c, "title").as_deref());
        }
    }
    for b in content {
        match block_type(b) {
            "web_search_tool_result" => {
                for r in b.get("content").and_then(|c| c.as_array()).into_iter().flatten() {
                    add(&mut hits, s(r, "url").as_deref(), s(r, "title").as_deref());
                }
            }
            "web_fetch_tool_result" => {
                if let Some(c) = b.get("content").filter(|c| c.is_object()) {
                    let title = c.get("content").and_then(|cc| s(cc, "title"));
                    add(&mut hits, s(c, "url").as_deref(), title.as_deref());
                }
            }
            _ => {}
        }
    }
    (cited, hits)
}

/// Error codes from failed searches/fetches (e.g. `url_not_accessible`).
pub fn tool_errors(content: &[Value]) -> Vec<String> {
    content
        .iter()
        .filter(|b| matches!(block_type(b), "web_search_tool_result" | "web_fetch_tool_result"))
        .filter_map(|b| b.get("content").filter(|c| c.is_object()))
        .filter(|c| c.get("type").and_then(|t| t.as_str()).map_or(false, |t| t.ends_with("_error")))
        .map(|c| c.get("error_code").and_then(|e| e.as_str()).unwrap_or("unknown").to_string())
        .collect()
}

pub fn cost_usd(t: &Totals, model: &str) -> f64 {
    let (pin, pout) = PRICES
        .iter()
        .find(|(m, _, _)| *m == model)
        .map(|(_, i, o)| (*i, *o))
        .unwrap_or(FALLBACK_PRICE);
    (t.input_tokens as f64 * pin + t.output_tokens as f64 * pout) / 1e6 + t.searches as f64 * SEARCH_USD
}

/// Render the tool result the agent sees: answer, numbered sources, cost line.
pub fn format_result(r: &WebResult, elapsed: Duration) -> String {
    let answer = final_answer(&r.content);
    let (cited, hits) = sources(&r.content);
    let errs = tool_errors(&r.content);
    let mut out = if answer.is_empty() { "(no answer text returned)".to_string() } else { answer };
    let extra = 8usize.saturating_sub(cited.len());
    let listed: Vec<(&(String, String), bool)> =
        cited.iter().map(|c| (c, true)).chain(hits.iter().take(extra).map(|h| (h, false))).collect();
    if !listed.is_empty() {
        out.push_str("\n\nSources:");
        for (i, ((url, title), is_cited)) in listed.iter().enumerate() {
            let t = if title.is_empty() { String::new() } else { format!("{} — ", title) };
            let mark = if *is_cited { "" } else { " (searched, not cited)" };
            out.push_str(&format!("\n  [{}] {}{}{}", i + 1, t, url, mark));
        }
    }
    let t = &r.totals;
    let stop = match r.stop_reason.as_deref() {
        Some("end_turn") | None => String::new(),
        Some(s) => format!(" · stop={}", s),
    };
    let errnote = if errs.is_empty() { String::new() } else { format!(" · tool_errors={:?}", errs) };
    out.push_str(&format!(
        "\n\n[web research · {} · {} searches · {} fetches · {}+{} tok · ${:.3} · {:.1}s{}{}]",
        r.model, t.searches, t.fetches, t.input_tokens, t.output_tokens,
        cost_usd(t, &r.model), elapsed.as_secs_f64(), stop, errnote
    ));
    out
}

/// Answer one question end-to-end, calling `on_heartbeat(elapsed_secs)` every
/// `heartbeat` while waiting (a 60s silent API call is otherwise
/// indistinguishable from a wedged process to anything watching the agent).
pub async fn answer(
    q: &WebQuery,
    poster: &dyn Poster,
    heartbeat: Duration,
    on_heartbeat: &(dyn Fn(u64) + Send + Sync),
) -> Result<String> {
    let t0 = Instant::now();
    let fut = run(q, poster);
    tokio::pin!(fut);
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + heartbeat, heartbeat);
    let result = loop {
        tokio::select! {
            r = &mut fut => break r?,
            _ = tick.tick() => on_heartbeat(t0.elapsed().as_secs()),
        }
    };
    Ok(format_result(&result, t0.elapsed()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn resp(blocks: Value, stop: &str, searches: u64, fetches: u64) -> Value {
        json!({"content": blocks, "stop_reason": stop,
               "usage": {"input_tokens": 100, "output_tokens": 10,
                         "server_tool_use": {"web_search_requests": searches,
                                             "web_fetch_requests": fetches}}})
    }

    fn search_block() -> Value {
        json!({"type": "web_search_tool_result", "content": [
            {"url": "https://a.gov.au", "title": "A"}, {"url": "https://b.com", "title": "B"}]})
    }

    struct FakePoster {
        responses: Mutex<Vec<Value>>,
        payloads: Mutex<Vec<Value>>,
    }

    impl FakePoster {
        fn new(responses: Vec<Value>) -> Self {
            Self { responses: Mutex::new(responses), payloads: Mutex::new(Vec::new()) }
        }
        fn payloads(&self) -> Vec<Value> {
            self.payloads.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Poster for FakePoster {
        async fn post(&self, payload: &Value) -> Result<Value> {
            self.payloads.lock().unwrap().push(payload.clone());
            let mut r = self.responses.lock().unwrap();
            if r.is_empty() {
                return Err(anyhow!("FakePoster: no more responses"));
            }
            Ok(r.remove(0))
        }
    }

    struct FailPoster;
    #[async_trait]
    impl Poster for FailPoster {
        async fn post(&self, _payload: &Value) -> Result<Value> {
            Err(anyhow!("HTTP 529 from http://x/v1/messages: overloaded"))
        }
    }

    fn q(args: Value) -> WebQuery {
        WebQuery::from_args(&args).unwrap()
    }

    // ── args ───────────────────────────────────────────────────────────

    #[test]
    fn quick_and_deep_budgets() {
        let quick = q(json!({"query": "x"}));
        assert_eq!((quick.model.as_str(), quick.max_searches, quick.max_tokens), (QUICK_MODEL, 5, 6000));
        let deep = q(json!({"query": "x", "deep": true}));
        assert_eq!((deep.model.as_str(), deep.max_searches, deep.max_tokens), (DEEP_MODEL, 15, 16000));
    }

    #[test]
    fn missing_or_blank_query_is_an_error() {
        assert!(WebQuery::from_args(&json!({})).is_err());
        assert!(WebQuery::from_args(&json!({"query": "   "})).is_err());
    }

    #[test]
    fn url_becomes_a_fetch_instruction() {
        let w = q(json!({"query": "what version", "url": "https://x.dev/changelog"}));
        assert_eq!(w.question, "Fetch https://x.dev/changelog and answer from it: what version");
    }

    #[test]
    fn domains_accept_array_or_comma_string() {
        assert_eq!(q(json!({"query": "x", "domains": ["a.com", " b.org "]})).domains, vec!["a.com", "b.org"]);
        assert_eq!(q(json!({"query": "x", "domains": "a.com, ,b.org"})).domains, vec!["a.com", "b.org"]);
    }

    #[test]
    fn clamp_caps_and_floors() {
        assert_eq!(clamp(999, MAX_SEARCHES_CAP), MAX_SEARCHES_CAP);
        assert_eq!(clamp(0, 25), 1);
        assert_eq!(clamp(7, 25), 7);
    }

    #[test]
    fn domains_reach_the_search_tool_and_fetch_is_offered() {
        let tools = build_tools(5, 3, &["gov.au".to_string()]);
        assert_eq!(tools[0]["allowed_domains"], json!(["gov.au"]));
        assert_eq!(tools[0]["max_uses"], 5);
        assert_eq!(tools[1]["type"], "web_fetch_20250910");
        assert!(build_tools(5, 3, &[])[0].get("allowed_domains").is_none());
    }

    // ── answer extraction ──────────────────────────────────────────────

    #[test]
    fn final_answer_drops_narration_before_last_tool() {
        let content = vec![
            json!({"type": "text", "text": "Let me check."}),
            json!({"type": "server_tool_use"}),
            search_block(),
            json!({"type": "thinking", "thinking": "hmm"}),
            json!({"type": "text", "text": "Answer: "}),
            json!({"type": "text", "text": "42"}),
        ];
        assert_eq!(final_answer(&content), "Answer: 42");
    }

    #[test]
    fn final_answer_falls_back_to_all_text_when_nothing_after_tools() {
        let content = vec![json!({"type": "text", "text": "Only this"}), search_block()];
        assert_eq!(final_answer(&content), "Only this");
    }

    #[test]
    fn sources_cited_first_then_hits_deduped() {
        let content = vec![
            search_block(),
            json!({"type": "text", "text": "x", "citations": [
                {"url": "https://b.com", "title": "B"}]}),
            json!({"type": "web_fetch_tool_result", "content": {
                "url": "https://c.io", "content": {"title": "C"}}}),
        ];
        let (cited, hits) = sources(&content);
        assert_eq!(cited, vec![("https://b.com".to_string(), "B".to_string())]);
        assert_eq!(hits, vec![("https://a.gov.au".to_string(), "A".to_string()),
                              ("https://c.io".to_string(), "C".to_string())]);
    }

    #[test]
    fn tool_errors_are_reported() {
        let content = vec![json!({"type": "web_fetch_tool_result", "content": {
            "type": "web_fetch_tool_result_error", "error_code": "url_not_accessible"}})];
        assert_eq!(tool_errors(&content), vec!["url_not_accessible"]);
        assert!(tool_errors(&[search_block()]).is_empty());
    }

    #[test]
    fn cost_counts_tokens_and_searches_and_overprices_unknown_models() {
        let t = Totals { input_tokens: 1_000_000, output_tokens: 0, searches: 3, fetches: 0 };
        assert!((cost_usd(&t, QUICK_MODEL) - 2.03).abs() < 1e-9);
        assert!((cost_usd(&t, "claude-mystery-9") - 10.03).abs() < 1e-9);
    }

    // ── the loop ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn single_round_answer_with_sources_and_cost_line() {
        let poster = FakePoster::new(vec![resp(json!([
            {"type": "server_tool_use"}, search_block(),
            {"type": "text", "text": "It is 42.", "citations": [{"url": "https://a.gov.au", "title": "A"}]}
        ]), "end_turn", 1, 0)]);
        let out = answer(&q(json!({"query": "x"})), &poster, Duration::from_secs(30), &|_| {}).await.unwrap();
        assert!(out.starts_with("It is 42."), "{}", out);
        assert!(out.contains("[1] A — https://a.gov.au\n"), "cited source must be first: {}", out);
        assert!(out.contains("[2] B — https://b.com (searched, not cited)"), "{}", out);
        assert!(out.contains("1 searches"), "{}", out);
        assert!(out.contains("$0.010"), "100 in + 10 out + 1 search ≈ $0.0103: {}", out);
        let p = &poster.payloads()[0];
        assert_eq!(p["model"], QUICK_MODEL);
        assert!(p["system"].as_str().unwrap().contains("Today's date is"));
    }

    #[tokio::test]
    async fn pause_turn_resumes_with_all_prior_content_and_remaining_budget() {
        let poster = FakePoster::new(vec![
            resp(json!([{"type": "text", "text": "r1"}, search_block()]), "pause_turn", 3, 0),
            resp(json!([{"type": "text", "text": "r2"}, {"type": "server_tool_use"}]), "pause_turn", 1, 0),
            resp(json!([{"type": "text", "text": "done"}]), "end_turn", 0, 0),
        ]);
        let r = run(&q(json!({"query": "x"})), &poster).await.unwrap();
        let p = poster.payloads();
        assert_eq!(p.len(), 3);
        assert_eq!(p[0]["tools"][0]["max_uses"], 5);
        assert_eq!(p[1]["tools"][0]["max_uses"], 2, "resume gets only the remaining search budget");
        assert_eq!(p[2]["tools"][0]["max_uses"], 1);
        let resumed = p[2]["messages"][1]["content"].as_array().unwrap();
        assert_eq!(resumed.len(), 4, "second resume must carry BOTH earlier rounds: {:?}", resumed);
        assert_eq!(p[2]["messages"][1]["role"], "assistant");
        assert_eq!(r.totals.searches, 4);
        assert_eq!(r.totals.input_tokens, 300);
        assert_eq!(final_answer(&r.content), "done");
    }

    #[tokio::test]
    async fn exhausted_budget_stops_resuming() {
        let poster = FakePoster::new(vec![
            resp(json!([{"type": "text", "text": "r1"}]), "pause_turn", 5, 0),
            resp(json!([{"type": "text", "text": "never"}]), "end_turn", 0, 0),
        ]);
        let r = run(&q(json!({"query": "x"})), &poster).await.unwrap();
        assert_eq!(poster.payloads().len(), 1, "no budget left ⇒ no further request");
        assert_eq!(r.stop_reason.as_deref(), Some("pause_turn"));
    }

    #[tokio::test]
    async fn continuations_are_capped() {
        let pauses: Vec<Value> = (0..10)
            .map(|_| resp(json!([{"type": "text", "text": "."}]), "pause_turn", 0, 0))
            .collect();
        let poster = FakePoster::new(pauses);
        run(&q(json!({"query": "x", "deep": true})), &poster).await.unwrap();
        assert_eq!(poster.payloads().len(), MAX_CONTINUATIONS + 1);
    }

    #[tokio::test]
    async fn api_error_body_is_an_error() {
        let poster = FakePoster::new(vec![json!({"type": "error", "error": {"type": "overloaded_error"}})]);
        let e = run(&q(json!({"query": "x"})), &poster).await.unwrap_err().to_string();
        assert!(e.contains("overloaded_error"), "{}", e);
    }

    #[tokio::test]
    async fn transport_error_propagates() {
        let e = answer(&q(json!({"query": "x"})), &FailPoster, Duration::from_secs(30), &|_| {})
            .await.unwrap_err().to_string();
        assert!(e.contains("HTTP 529"), "{}", e);
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeat_fires_while_waiting() {
        struct SlowPoster;
        #[async_trait]
        impl Poster for SlowPoster {
            async fn post(&self, _p: &Value) -> Result<Value> {
                tokio::time::sleep(Duration::from_secs(65)).await;
                Ok(json!({"content": [{"type": "text", "text": "ok"}], "stop_reason": "end_turn"}))
            }
        }
        let beats = Mutex::new(Vec::new());
        let out = answer(&q(json!({"query": "x"})), &SlowPoster, Duration::from_secs(30),
                         &|s| beats.lock().unwrap().push(s)).await.unwrap();
        assert!(out.starts_with("ok"));
        assert_eq!(beats.lock().unwrap().len(), 2, "65s wait at 30s cadence = 2 beats");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn unreachable_proxy_names_the_key_proxy() {
        // Port 1 is never listening: a connect error, fast, no network.
        let prev = std::env::var("ANTHROPIC_BASE_URL").ok();
        std::env::set_var("ANTHROPIC_BASE_URL", "http://127.0.0.1:1");
        let poster = HttpPoster::new("k".into()).unwrap();
        let e = poster.post(&json!({})).await.unwrap_err().to_string();
        match prev {
            Some(v) => std::env::set_var("ANTHROPIC_BASE_URL", v),
            None => std::env::remove_var("ANTHROPIC_BASE_URL"),
        }
        assert!(e.contains("http://127.0.0.1:1/v1/messages"), "{}", e);
        assert!(e.contains("key proxy"), "{}", e);
    }

    // ── credentials ────────────────────────────────────────────────────

    fn anthropic(key: &str) -> g3_config::AnthropicConfig {
        g3_config::AnthropicConfig {
            api_key: key.into(), model: "m".into(), max_tokens: None, temperature: None,
            cache_config: None, enable_1m_context: None, thinking_budget_tokens: None, effort: None,
        }
    }

    #[test]
    fn credentials_from_default_anthropic_provider() {
        let mut c = Config::default();
        c.providers.anthropic.clear();
        c.providers.anthropic.insert("aaa".into(), anthropic("first-by-name"));
        c.providers.anthropic.insert("main".into(), anthropic("the-default"));
        c.providers.default_provider = "anthropic.main".into();
        assert_eq!(resolve_api_key(&c).as_deref(), Some("the-default"));
    }

    #[test]
    fn credentials_fall_back_to_first_anthropic_entry_when_default_is_not_anthropic() {
        let mut c = Config::default();
        c.providers.anthropic.clear();
        c.providers.anthropic.insert("zed".into(), anthropic("z"));
        c.providers.anthropic.insert("alpha".into(), anthropic("a"));
        c.providers.default_provider = "openai.default".into();
        assert_eq!(resolve_api_key(&c).as_deref(), Some("a"));
    }

    #[test]
    fn no_anthropic_config_means_no_credentials() {
        let mut c = Config::default();
        c.providers.anthropic.clear();
        c.providers.default_provider = "openai.default".into();
        assert_eq!(resolve_api_key(&c), None);
    }
}

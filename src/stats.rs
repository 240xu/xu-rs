//! Token usage statistics: append-only per-day JSONL files plus period
//! aggregation (24h / 48h / 7d / 30d) with cache hit rate, success rate and a
//! passthrough approximation marker.
//!
//! Records are written to `<home>/.codex/stats/tokens-YYYY-MM-DD.jsonl`, one
//! JSON object per line. Aggregation is a pure read of those files, so the
//! runtime only pays for the write side.
//!
//! Scheme (mirrors CC Switch): four token buckets (input / output / cache
//! read / cache creation), a per-record `semantics` marker describing what
//! `input` actually contains, and aggregation that normalizes every record
//! back to fresh input before computing the hit-rate denominator
//! (`fresh_input + cache_creation + cache_read`).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

/// Input-semantics marker: what the recorded `input` number contains.
///
/// - FRESH: input excludes all cache tokens (Anthropic-style `input_tokens`).
/// - TOTAL: input includes both cache read and cache write (bridge IR input,
///   and OpenAI-style `prompt_tokens` from providers that report cache write).
/// - LEGACY: input includes cache read but not cache write (OpenAI-style
///   `prompt_tokens` without a write figure, and pre-T-E2 rows).
pub const SEMANTICS_FRESH: &str = "fresh";
pub const SEMANTICS_TOTAL: &str = "total";
pub const SEMANTICS_LEGACY: &str = "legacy";

/// One recorded completion's usage. `source` marks which runtime path
/// produced the numbers: converted / converted_stream / passthrough /
/// passthrough_stream. Optional fields default to their zero values for old
/// rows, so existing JSONL stays readable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UsageRecord {
    pub ts: i64,
    pub path: String,
    pub model: String,
    pub input: u64,
    pub output: u64,
    /// Cache-read tokens (`cache_read_input_tokens` / `cached_tokens`).
    pub cached: u64,
    /// Cache-creation tokens (`cache_creation_input_tokens` /
    /// `cache_write_tokens`).
    pub cache_creation: u64,
    /// What `input` contains: "fresh" | "total" | "legacy" (see constants).
    pub semantics: &'static str,
    /// Request duration in milliseconds; `None` on rows written before
    /// latency tracking existed.
    pub latency_ms: Option<u64>,
    /// Upstream HTTP status; `None` (treated as success) for old rows.
    pub status_code: Option<u16>,
    /// Whether the request was a stream.
    pub is_streaming: bool,
    /// Relayed error text for failed requests; `None` on success.
    pub error: Option<String>,
    pub source: &'static str,
}

pub const SOURCE_CONVERTED: &str = "converted";
pub const SOURCE_CONVERTED_STREAM: &str = "converted_stream";
pub const SOURCE_PASSTHROUGH: &str = "passthrough";
pub const SOURCE_PASSTHROUGH_STREAM: &str = "passthrough_stream";

pub const PERIOD_24H: &str = "24h";
pub const PERIOD_48H: &str = "48h";
pub const PERIOD_7D: &str = "7d";
pub const PERIOD_30D: &str = "30d";

/// Aggregated token usage for one time window.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct PeriodStat {
    /// Raw recorded input sum (semantics not applied; kept for backward
    /// compatibility with rows and callers that predate normalization).
    pub input: u64,
    /// Input sum normalized to fresh semantics (see `fresh_input_of`).
    pub fresh_input: u64,
    pub output: u64,
    pub cached: u64,
    pub cache_creation: u64,
    pub requests: u64,
    /// cached / (fresh_input + cache_creation + cached); 0.0 when the
    /// denominator is zero. The cache-write bucket is part of the
    /// denominator: tokens written now are what later requests can hit.
    pub cache_hit_rate: f64,
    /// Fraction of requests that succeeded (2xx, or no status recorded).
    pub success_rate: f64,
    /// Number of requests that went through the raw passthrough stream path
    /// (approximate by nature: derived from the merged `usage` SSE frames).
    pub passthrough_approx: u64,
}

/// Width in seconds of each aggregation window.
pub fn period_seconds(period: &str) -> Option<i64> {
    match period {
        PERIOD_24H => Some(86_400),
        PERIOD_48H => Some(172_800),
        PERIOD_7D => Some(604_800),
        PERIOD_30D => Some(2_592_000),
        _ => None,
    }
}

fn stats_dir(home: &Path) -> std::path::PathBuf {
    home.join(".codex").join("stats")
}

/// Appends one record to the per-day JSONL file
/// `<home>/.codex/stats/tokens-YYYY-MM-DD.jsonl`, creating the directory and
/// file as needed. The day is derived from `record.ts` (UTC).
pub fn record_usage(home: &Path, record: &UsageRecord) -> Result<(), String> {
    let date = chrono::DateTime::from_timestamp(record.ts, 0)
        .map(|stamp| stamp.date_naive())
        .unwrap_or_else(|| chrono::Utc::now().date_naive())
        .format("%Y-%m-%d")
        .to_string();
    let dir = stats_dir(home);
    std::fs::create_dir_all(&dir).map_err(|error| format!("create {}: {error}", dir.display()))?;
    let path = dir.join(format!("tokens-{date}.jsonl"));
    let mut line = serde_json::to_string(record).map_err(|error| error.to_string())?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| format!("open {}: {error}", path.display()))?;
    file.write_all(line.as_bytes())
        .map_err(|error| format!("write {}: {error}", path.display()))
}

/// Parses one JSONL line into a record. Lines that do not parse or carry an
/// unknown `source` are skipped (they never contribute to aggregates).
/// Rows written before the T-E2 field set get sensible defaults: legacy
/// semantics (input includes cache read, write untracked — which is exactly
/// what old rows recorded), no status (counted as success), no latency.
pub fn parse_record(line: &str) -> Option<UsageRecord> {
    let value: Value = serde_json::from_str(line).ok()?;
    let source = match value.get("source").and_then(Value::as_str) {
        Some(SOURCE_CONVERTED) => SOURCE_CONVERTED,
        Some(SOURCE_CONVERTED_STREAM) => SOURCE_CONVERTED_STREAM,
        Some(SOURCE_PASSTHROUGH) => SOURCE_PASSTHROUGH,
        Some(SOURCE_PASSTHROUGH_STREAM) => SOURCE_PASSTHROUGH_STREAM,
        _ => return None,
    };
    let semantics = match value.get("semantics").and_then(Value::as_str) {
        Some(SEMANTICS_FRESH) => SEMANTICS_FRESH,
        Some(SEMANTICS_TOTAL) => SEMANTICS_TOTAL,
        Some(SEMANTICS_LEGACY) => SEMANTICS_LEGACY,
        _ => SEMANTICS_LEGACY,
    };
    Some(UsageRecord {
        ts: value.get("ts").and_then(Value::as_i64)?,
        path: value.get("path").and_then(Value::as_str)?.to_string(),
        model: value.get("model").and_then(Value::as_str)?.to_string(),
        input: value.get("input").and_then(Value::as_u64).unwrap_or(0),
        output: value.get("output").and_then(Value::as_u64).unwrap_or(0),
        cached: value.get("cached").and_then(Value::as_u64).unwrap_or(0),
        cache_creation: value
            .get("cache_creation")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        semantics,
        latency_ms: value.get("latency_ms").and_then(Value::as_u64),
        status_code: value
            .get("status_code")
            .and_then(Value::as_u64)
            .and_then(|code| u16::try_from(code).ok()),
        is_streaming: value
            .get("is_streaming")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        error: value
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_string),
        source,
    })
}

/// Normalizes one record's `input` to fresh (cache-excluded) tokens, per its
/// `semantics` marker. Defensive: never underflows below zero.
pub fn fresh_input_of(record: &UsageRecord) -> u64 {
    match record.semantics {
        // Input includes both cache read and cache write.
        SEMANTICS_TOTAL => record
            .input
            .saturating_sub(record.cached)
            .saturating_sub(record.cache_creation),
        // Input includes cache read; cache write was never tracked.
        SEMANTICS_LEGACY => record.input.saturating_sub(record.cached),
        // Input already excludes cache tokens (and unknown values are
        // treated as-is, which can only understate the hit rate).
        _ => record.input,
    }
}

/// Whether a record counts as a successful request: a recorded 2xx status,
/// or no status at all (rows that predate status tracking are treated as
/// successes).
pub fn record_succeeded(record: &UsageRecord) -> bool {
    match record.status_code {
        Some(status) => (200..=299).contains(&status),
        None => true,
    }
}

/// Aggregates every recorded usage into the four windows "24h" | "48h" |
/// "7d" | "30d", keyed by period name. Records outside all windows still
/// count toward none of them; every window is always present in the map.
pub fn aggregate(home: &Path) -> BTreeMap<String, PeriodStat> {
    let mut periods: BTreeMap<String, PeriodStat> = [PERIOD_24H, PERIOD_48H, PERIOD_7D, PERIOD_30D]
        .into_iter()
        .map(|key| (key.to_string(), PeriodStat::default()))
        .collect();

    let Ok(entries) = std::fs::read_dir(stats_dir(home)) else {
        return periods;
    };
    let now = chrono::Utc::now().timestamp();
    for entry in entries.flatten() {
        let Ok(content) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        for line in content.lines() {
            let Some(record) = parse_record(line) else {
                continue;
            };
            let fresh_input = fresh_input_of(&record);
            let succeeded = record_succeeded(&record);
            for key in [PERIOD_24H, PERIOD_48H, PERIOD_7D, PERIOD_30D] {
                let Some(seconds) = period_seconds(key) else {
                    continue;
                };
                if record.ts < now - seconds {
                    continue;
                }
                let stat = periods.get_mut(key).expect("period key pre-seeded");
                stat.input = stat.input.saturating_add(record.input);
                stat.fresh_input = stat.fresh_input.saturating_add(fresh_input);
                stat.output = stat.output.saturating_add(record.output);
                stat.cached = stat.cached.saturating_add(record.cached);
                stat.cache_creation = stat.cache_creation.saturating_add(record.cache_creation);
                stat.requests = stat.requests.saturating_add(1);
                if succeeded {
                    stat.success_rate += 1.0;
                }
                if record.source == SOURCE_PASSTHROUGH_STREAM {
                    stat.passthrough_approx = stat.passthrough_approx.saturating_add(1);
                }
            }
        }
    }
    for stat in periods.values_mut() {
        let cacheable = stat
            .fresh_input
            .saturating_add(stat.cache_creation)
            .saturating_add(stat.cached);
        stat.cache_hit_rate = if cacheable > 0 {
            stat.cached as f64 / cacheable as f64
        } else {
            0.0
        };
        stat.success_rate = if stat.requests > 0 {
            stat.success_rate / stat.requests as f64
        } else {
            0.0
        };
    }
    periods
}

/// Extracts (input, output, cached, cache_creation) counts from an upstream
/// usage object. Handles the Anthropic shape (`input_tokens` /
/// `output_tokens` / `cache_read_input_tokens` /
/// `cache_creation_input_tokens`) and the OpenAI shape (`prompt_tokens` /
/// `completion_tokens` / `cached_tokens` or
/// `prompt_tokens_details.cached_tokens` / `cache_write_tokens`). Returns
/// None when the object has no input count at all (i.e. it is not a usage
/// object).
pub fn usage_from_json(usage: &Value) -> Option<(u64, u64, u64, u64)> {
    let input = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(Value::as_u64)?;
    let output = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cached = usage
        .get("cache_read_input_tokens")
        .or_else(|| usage.get("cached_tokens"))
        .and_then(Value::as_u64)
        .or_else(|| {
            usage
                .get("input_tokens_details")
                .and_then(|details| details.get("cached_tokens"))
                .and_then(Value::as_u64)
        })
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(|details| details.get("cached_tokens"))
                .and_then(Value::as_u64)
        })
        // DeepSeek shape (prompt_cache_hit_tokens): cached prefix reported
        // explicitly, prompt_tokens is cache-inclusive — same semantics as
        // OpenAI's cached_tokens (LEGACY: input - cached = miss).
        .or_else(|| usage.get("prompt_cache_hit_tokens").and_then(Value::as_u64))
        .unwrap_or(0);
    let cache_creation = usage
        .get("cache_creation_input_tokens")
        .or_else(|| usage.get("cache_write_tokens"))
        .and_then(Value::as_u64)
        .or_else(|| {
            usage
                .get("input_tokens_details")
                .and_then(|details| details.get("cache_write_tokens"))
                .and_then(Value::as_u64)
        })
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(|details| details.get("cache_write_tokens"))
                .and_then(Value::as_u64)
        })
        .unwrap_or(0);
    Some((input, output, cached, cache_creation))
}

/// Extracts usage counts from a full SSE `data:` frame object. The usage
/// object sits at the top level for OpenAI chat chunks / responses events and
/// Anthropic message_delta frames, and inside `message` for Anthropic
/// message_start frames.
pub fn usage_from_frame(frame: &Value) -> Option<(u64, u64, u64, u64)> {
    frame
        .get("usage")
        .or_else(|| {
            frame
                .get("message")
                .and_then(|message| message.get("usage"))
        })
        .and_then(usage_from_json)
}

/// Decides the input `semantics` marker for a raw upstream usage object,
/// based on its shape:
///
/// - Anthropic family (`input_tokens`): the field is the billed fresh figure
///   with cached prefixes reported separately, so the recorded input is
///   already fresh.
/// - OpenAI family (`prompt_tokens`): the count includes the cached prefix,
///   so the recorded input is cache-inclusive; mark TOTAL when a cache-write
///   figure is also reported, LEGACY otherwise (read inside input, write
///   untracked).
/// - Unknown shape: keep the input as-is (FRESH). This can only understate
///   the hit rate, never double-count tokens.
pub fn semantics_for_usage(usage: &Value) -> &'static str {
    if usage.get("input_tokens").is_some() {
        // Anthropic-style input_tokens is fresh (cache read/write reported
        // separately), UNLESS input_tokens_details.cached_tokens is present —
        // the OpenAI Responses API reports cache-inclusive input there.
        if usage
            .get("input_tokens_details")
            .and_then(|details| details.get("cached_tokens"))
            .is_some()
        {
            if usage_has_cache_write(usage) {
                SEMANTICS_TOTAL
            } else {
                SEMANTICS_LEGACY
            }
        } else {
            SEMANTICS_FRESH
        }
    } else if usage.get("prompt_tokens").is_some() {
        if usage_has_cache_write(usage) {
            SEMANTICS_TOTAL
        } else {
            SEMANTICS_LEGACY
        }
    } else {
        SEMANTICS_FRESH
    }
}

fn usage_has_cache_write(usage: &Value) -> bool {
    usage.get("cache_creation_input_tokens").is_some()
        || usage.get("cache_write_tokens").is_some()
        || usage
            .get("input_tokens_details")
            .and_then(|details| details.get("cache_write_tokens"))
            .is_some()
        || usage
            .get("prompt_tokens_details")
            .and_then(|details| details.get("cache_write_tokens"))
            .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record(ts: i64, input: u64, output: u64, cached: u64, source: &'static str) -> UsageRecord {
        UsageRecord {
            ts,
            path: "/v1/messages".to_string(),
            model: "fixture-model".to_string(),
            input,
            output,
            cached,
            cache_creation: 0,
            semantics: SEMANTICS_LEGACY,
            latency_ms: Some(12),
            status_code: Some(200),
            is_streaming: false,
            error: None,
            source,
        }
    }

    fn now_minus(hours: i64) -> i64 {
        chrono::Utc::now().timestamp() - hours * 3600
    }

    #[test]
    fn record_and_aggregate_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        record_usage(
            dir.path(),
            &record(now_minus(1), 100, 20, 30, SOURCE_CONVERTED),
        )
        .unwrap();
        record_usage(
            dir.path(),
            &record(now_minus(2), 50, 10, 5, SOURCE_CONVERTED_STREAM),
        )
        .unwrap();

        let periods = aggregate(dir.path());
        for key in [PERIOD_24H, PERIOD_48H, PERIOD_7D, PERIOD_30D] {
            let stat = &periods[key];
            assert_eq!(stat.input, 150);
            assert_eq!(stat.output, 30);
            assert_eq!(stat.cached, 35);
            assert_eq!(stat.cache_creation, 0);
            assert_eq!(stat.requests, 2);
            assert_eq!(stat.passthrough_approx, 0);
            // Legacy normalization: fresh = input - cached, so the cacheable
            // denominator collapses back to the raw input (150) and the hit
            // rate is unchanged from the pre-normalization formula.
            assert_eq!(stat.fresh_input, 150 - 35);
            assert!((stat.cache_hit_rate - 35.0 / 150.0).abs() < 1e-9);
            assert!((stat.success_rate - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn records_roll_to_daily_files_by_ts() {
        let dir = tempfile::tempdir().unwrap();
        let now = chrono::Utc::now().timestamp();
        let today_ts = now - 3600; // 1 小时前（24h 窗口内）
        let yesterday_ts = now - 25 * 3600; // 25 小时前（24h 窗口外、48h 内）
        let today = chrono::DateTime::from_timestamp(today_ts, 0)
            .unwrap()
            .date_naive();
        let yesterday = chrono::DateTime::from_timestamp(yesterday_ts, 0)
            .unwrap()
            .date_naive();
        record_usage(dir.path(), &record(today_ts, 1, 1, 0, SOURCE_PASSTHROUGH)).unwrap();
        record_usage(
            dir.path(),
            &record(yesterday_ts, 2, 2, 0, SOURCE_PASSTHROUGH),
        )
        .unwrap();

        let stats = dir.path().join(".codex").join("stats");
        let mut files: Vec<String> = std::fs::read_dir(&stats)
            .unwrap()
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        files.sort();
        assert_eq!(
            files,
            vec![
                format!("tokens-{yesterday}.jsonl"),
                format!("tokens-{today}.jsonl")
            ]
        );
        assert_eq!(
            aggregate(dir.path())[PERIOD_24H].requests,
            1,
            "yesterday 12:00 UTC is outside the rolling 24h window"
        );
        assert_eq!(aggregate(dir.path())[PERIOD_48H].requests, 2);
    }

    #[test]
    fn aggregate_four_periods_include_only_in_window_samples() {
        let dir = tempfile::tempdir().unwrap();
        let samples = [
            (now_minus(2), "2h", 1),
            (now_minus(30), "30h", 10),
            (now_minus(72), "3d", 100),
            (now_minus(360), "15d", 1000),
        ];
        for (ts, _label, weight) in samples {
            record_usage(
                dir.path(),
                &record(ts, weight, weight, weight, SOURCE_PASSTHROUGH),
            )
            .unwrap();
        }

        let periods = aggregate(dir.path());
        assert_eq!(
            periods[PERIOD_24H].requests, 1,
            "24h holds only the 2h sample"
        );
        assert_eq!(periods[PERIOD_48H].requests, 2, "48h holds 2h + 30h");
        assert_eq!(periods[PERIOD_7D].requests, 3, "7d holds 2h + 30h + 3d");
        assert_eq!(periods[PERIOD_30D].requests, 4, "30d holds all samples");
        assert_eq!(periods[PERIOD_30D].input, 1 + 10 + 100 + 1000);
        assert_eq!(periods[PERIOD_24H].input, 1);
    }

    #[test]
    fn cache_only_record_still_counts_and_hits_fully() {
        let dir = tempfile::tempdir().unwrap();
        // A cache-only request (input 0 but cached > 0) must survive into
        // the aggregate; every recorded token was served from cache, so the
        // hit rate is 100%.
        record_usage(dir.path(), &record(now_minus(1), 0, 5, 3, SOURCE_CONVERTED)).unwrap();
        let stat = &aggregate(dir.path())[PERIOD_24H];
        assert_eq!(stat.requests, 1);
        assert_eq!(stat.cached, 3);
        assert_eq!(stat.fresh_input, 0);
        assert!((stat.cache_hit_rate - 1.0).abs() < 1e-9);
    }

    #[test]
    fn passthrough_approx_counts_only_passthrough_stream_requests() {
        let dir = tempfile::tempdir().unwrap();
        for (source, expected) in [
            (SOURCE_CONVERTED, 0),
            (SOURCE_CONVERTED_STREAM, 0),
            (SOURCE_PASSTHROUGH, 0),
            (SOURCE_PASSTHROUGH_STREAM, 1),
            (SOURCE_PASSTHROUGH_STREAM, 2),
        ] {
            record_usage(dir.path(), &record(now_minus(1), 1, 1, 0, source)).unwrap();
            assert_eq!(
                aggregate(dir.path())[PERIOD_24H].passthrough_approx,
                expected
            );
        }
    }

    #[test]
    fn unknown_source_lines_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        record_usage(dir.path(), &record(now_minus(1), 1, 1, 0, SOURCE_CONVERTED)).unwrap();
        let stats = dir.path().join(".codex").join("stats");
        let file = std::fs::read_dir(&stats).unwrap().flatten().next().unwrap();
        let path = file.path();
        let mut content = std::fs::read_to_string(&path).unwrap();
        content.push_str("{\"ts\":1,\"path\":\"/v1\",\"model\":\"m\",\"input\":9,\"output\":9,\"cached\":9,\"source\":\"mystery\"}\n");
        std::fs::write(&path, content).unwrap();
        let stat = &aggregate(dir.path())[PERIOD_24H];
        assert_eq!(stat.requests, 1);
        assert_eq!(stat.input, 1);
    }

    #[test]
    fn semantics_normalization_yields_correct_fresh_input() {
        // TOTAL: input = fresh + cache read + cache creation (bridge IR and
        // cache-inclusive OpenAI providers) -> fresh = input - cr - cc.
        let total = UsageRecord {
            ts: now_minus(1),
            path: "/v1/messages".to_string(),
            model: "m".to_string(),
            input: 1000,
            output: 40,
            cached: 300,
            cache_creation: 100,
            semantics: SEMANTICS_TOTAL,
            latency_ms: None,
            status_code: Some(200),
            is_streaming: true,
            error: None,
            source: SOURCE_CONVERTED_STREAM,
        };
        assert_eq!(fresh_input_of(&total), 1000 - 300 - 100);

        // LEGACY: input = fresh + cache read, write untracked.
        let legacy = UsageRecord {
            semantics: SEMANTICS_LEGACY,
            input: 700,
            cached: 200,
            cache_creation: 0,
            ..total.clone()
        };
        assert_eq!(fresh_input_of(&legacy), 700 - 200);

        // FRESH: input already excludes cache tokens.
        let fresh = UsageRecord {
            semantics: SEMANTICS_FRESH,
            input: 400,
            cached: 300,
            cache_creation: 100,
            ..total.clone()
        };
        assert_eq!(fresh_input_of(&fresh), 400);

        // TOTAL normalization must never underflow.
        let tiny = UsageRecord {
            semantics: SEMANTICS_TOTAL,
            input: 5,
            cached: 300,
            cache_creation: 100,
            ..total.clone()
        };
        assert_eq!(fresh_input_of(&tiny), 0);
    }

    #[test]
    fn aggregate_uses_fresh_denominator_including_cache_write() {
        let dir = tempfile::tempdir().unwrap();
        let mut total = record(now_minus(1), 0, 0, 0, SOURCE_CONVERTED);
        total.input = 1000;
        total.cached = 300;
        total.cache_creation = 100;
        total.semantics = SEMANTICS_TOTAL;
        record_usage(dir.path(), &total).unwrap();

        let stat = &aggregate(dir.path())[PERIOD_24H];
        // Denominator = fresh (600) + cache_creation (100) + cache_read (300).
        assert_eq!(stat.fresh_input, 600);
        assert_eq!(stat.cache_creation, 100);
        assert!((stat.cache_hit_rate - 300.0 / 1000.0).abs() < 1e-9);
    }

    #[test]
    fn success_rate_counts_2xx_only_and_failures_still_count_requests() {
        let dir = tempfile::tempdir().unwrap();
        let mut ok = record(now_minus(1), 10, 2, 0, SOURCE_CONVERTED);
        ok.status_code = Some(204); // 2xx non-200 still succeeds
        record_usage(dir.path(), &ok).unwrap();

        let mut failed = record(now_minus(1), 0, 0, 0, SOURCE_CONVERTED);
        failed.status_code = Some(500);
        failed.error = Some("upstream exploded".to_string());
        record_usage(dir.path(), &failed).unwrap();

        let stat = &aggregate(dir.path())[PERIOD_24H];
        assert_eq!(stat.requests, 2);
        assert!((stat.success_rate - 0.5).abs() < 1e-9);
    }

    #[test]
    fn old_rows_without_new_fields_parse_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let stats = dir.path().join(".codex").join("stats");
        std::fs::create_dir_all(&stats).unwrap();
        let path = stats.join("tokens-2026-08-12.jsonl");
        // A row as written before T-E2: no cache_creation / semantics /
        // latency / status / streaming / error fields.
        std::fs::write(
            &path,
            "{\"ts\":1000,\"path\":\"/v1/messages\",\"model\":\"m\",\"input\":100,\"output\":10,\"cached\":40,\"source\":\"converted\"}\n",
        )
        .unwrap();

        let record = parse_record("{\"ts\":1000,\"path\":\"/v1/messages\",\"model\":\"m\",\"input\":100,\"output\":10,\"cached\":40,\"source\":\"converted\"}").expect("old row parses");
        assert_eq!(record.cache_creation, 0);
        assert_eq!(record.semantics, SEMANTICS_LEGACY);
        assert_eq!(record.latency_ms, None);
        assert_eq!(record.status_code, None);
        assert!(!record.is_streaming);
        assert_eq!(record.error, None);
        assert!(record_succeeded(&record), "no status counts as success");
        assert_eq!(fresh_input_of(&record), 60);
    }

    #[test]
    fn usage_from_json_reads_anthropic_and_openai_shapes() {
        assert_eq!(
            usage_from_json(&json!({
                "input_tokens": 12,
                "output_tokens": 4,
                "cache_read_input_tokens": 2,
                "cache_creation_input_tokens": 1
            })),
            Some((12, 4, 2, 1))
        );
        assert_eq!(
            usage_from_json(&json!({
                "prompt_tokens": 18,
                "completion_tokens": 11,
                "total_tokens": 29,
                "prompt_tokens_details": { "cached_tokens": 3 }
            })),
            Some((18, 11, 3, 0))
        );
        assert_eq!(
            usage_from_json(&json!({
                "prompt_tokens": 18,
                "completion_tokens": 11,
                "total_tokens": 29
            })),
            Some((18, 11, 0, 0))
        );
        // OpenAI Responses cache-write chain: input_tokens_details.
        assert_eq!(
            usage_from_json(&json!({
                "input_tokens": 30,
                "output_tokens": 7,
                "input_tokens_details": { "cached_tokens": 4, "cache_write_tokens": 2 }
            })),
            Some((30, 7, 4, 2))
        );
        // Top-level cache_write_tokens (zen/Kimi style).
        assert_eq!(
            usage_from_json(&json!({
                "prompt_tokens": 25,
                "completion_tokens": 3,
                "cache_write_tokens": 6
            })),
            Some((25, 3, 0, 6))
        );
        assert_eq!(usage_from_json(&json!({ "total_tokens": 5 })), None);
        assert_eq!(usage_from_json(&json!({})), None);
        // DeepSeek shape: prompt_cache_hit_tokens reports the cached prefix
        // (deepseek-chat/reasoner, via spec/one-api gateways).
        assert_eq!(
            usage_from_json(&json!({
                "prompt_tokens": 20,
                "completion_tokens": 6,
                "prompt_cache_hit_tokens": 8,
                "prompt_cache_miss_tokens": 12
            })),
            Some((20, 6, 8, 0))
        );
    }

    #[test]
    fn usage_from_frame_finds_usage_at_top_level_or_inside_message() {
        assert_eq!(
            usage_from_frame(&json!({
                "type": "response.completed",
                "usage": { "input_tokens": 5, "output_tokens": 2 }
            })),
            Some((5, 2, 0, 0))
        );
        assert_eq!(
            usage_from_frame(&json!({
                "type": "message_start",
                "message": { "usage": { "input_tokens": 7, "output_tokens": 1 } }
            })),
            Some((7, 1, 0, 0))
        );
        assert_eq!(
            usage_from_frame(&json!({
                "type": "content_block_delta",
                "delta": { "type": "text_delta", "text": "the word usage inside text" }
            })),
            None
        );
    }

    #[test]
    fn semantics_for_usage_classifies_by_shape() {
        // Anthropic family: input_tokens is the billed fresh figure.
        assert_eq!(
            semantics_for_usage(&json!({
                "input_tokens": 12,
                "cache_read_input_tokens": 2
            })),
            SEMANTICS_FRESH
        );
        // OpenAI family without a write figure: input includes cache read.
        assert_eq!(
            semantics_for_usage(&json!({
                "prompt_tokens": 18,
                "prompt_tokens_details": { "cached_tokens": 3 }
            })),
            SEMANTICS_LEGACY
        );
        // OpenAI family with a write figure: input includes read + write.
        assert_eq!(
            semantics_for_usage(&json!({
                "prompt_tokens": 25,
                "prompt_tokens_details": { "cached_tokens": 3, "cache_write_tokens": 6 }
            })),
            SEMANTICS_TOTAL
        );
        assert_eq!(
            semantics_for_usage(&json!({ "prompt_tokens": 5, "cache_write_tokens": 1 })),
            SEMANTICS_TOTAL
        );
        // OpenAI Responses API: input_tokens_details.cached_tokens marks the
        // input as cache-inclusive even though the top-level key is
        // input_tokens.
        assert_eq!(
            semantics_for_usage(&json!({
                "input_tokens": 40,
                "input_tokens_details": { "cached_tokens": 4 }
            })),
            SEMANTICS_LEGACY
        );
        // DeepSeek shape: prompt_cache_hit_tokens (no write figure) →
        // LEGACY, same as OpenAI's prompt_tokens + cached_tokens.
        assert_eq!(
            semantics_for_usage(&json!({
                "prompt_tokens": 20,
                "completion_tokens": 6,
                "prompt_cache_hit_tokens": 8,
                "prompt_cache_miss_tokens": 12
            })),
            SEMANTICS_LEGACY
        );
        assert_eq!(
            semantics_for_usage(&json!({
                "input_tokens": 40,
                "input_tokens_details": { "cached_tokens": 4, "cache_write_tokens": 2 }
            })),
            SEMANTICS_TOTAL
        );
        // Unknown shape: keep input as-is.
        assert_eq!(
            semantics_for_usage(&json!({ "total_tokens": 5 })),
            SEMANTICS_FRESH
        );
    }

    #[test]
    fn period_seconds_known_windows_only() {
        assert_eq!(period_seconds(PERIOD_24H), Some(86_400));
        assert_eq!(period_seconds(PERIOD_48H), Some(172_800));
        assert_eq!(period_seconds(PERIOD_7D), Some(604_800));
        assert_eq!(period_seconds(PERIOD_30D), Some(2_592_000));
        assert_eq!(period_seconds("forever"), None);
    }
}

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use chrono::{DateTime, Local, NaiveDate};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{Bucket, Parts, Provider, RATE_REV, Snapshot};
use crate::price::{price_usd, usd_to_micros};

pub struct Shared {
    pub progress_done: AtomicU32,
    pub progress_total: AtomicU32,
    pub scanning: AtomicBool,
    pub generation: AtomicU64,
    pub snapshot: Mutex<Arc<Snapshot>>,
}

impl Shared {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            progress_done: AtomicU32::new(0),
            progress_total: AtomicU32::new(0),
            scanning: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            snapshot: Mutex::new(Arc::new(Snapshot::default())),
        })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct FileEntry {
    len: u64,
    mtime: i64,
    parsed_to: u64,
    model: Option<String>,
    buckets: Vec<Bucket>,
    seen: Vec<u64>,
    calls: u64,
}

#[derive(Serialize, Deserialize)]
struct DiskCache {
    rev: u32,
    files: BTreeMap<String, FileEntry>,
}

struct Source {
    path: PathBuf,
    provider: Provider,
}

enum Plan {
    Fresh,
    Append { start: u64 },
    Replace,
    Accumulate { start: u64 },
}

fn scan_log(text: &str) {
    use std::io::Write;
    let path = data_dir().join("watt.log");
    let line = format!("{} {text}\n", Local::now().format("%Y-%m-%d %H:%M:%S"));
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(line.as_bytes());
        let _ = file.flush();
    }
}

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("Watt");
    let _ = fs::create_dir_all(&dir);
    dir
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn run_scan(shared: &Shared) {
    let cache_path = data_dir().join("cache.json");
    let mut cache = load_cache(&cache_path);
    let sources = discover();
    shared
        .progress_total
        .store(sources.len() as u32, Ordering::Relaxed);
    shared.progress_done.store(0, Ordering::Relaxed);

    let mut ready: Vec<(String, FileEntry)> = Vec::new();
    let mut pending: Vec<(Source, Plan, FileEntry)> = Vec::new();

    for source in sources {
        let key = source.path.to_string_lossy().to_string();
        let (len, mtime) = file_sig(&source.path);
        let prev = cache.files.get(&key).cloned().unwrap_or_default();
        match plan_for(source.provider, &prev, len, mtime) {
            Plan::Fresh => {
                ready.push((key, prev));
                shared.progress_done.fetch_add(1, Ordering::Relaxed);
            }
            other => pending.push((source, other, prev)),
        }
    }

    publish(shared, &ready);

    let done = &shared.progress_done;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build().ok();
    let parse_pending = || {
        pending
            .par_iter()
            .map(|(source, plan, prev)| {
                let entry = parse_source(source, plan, prev);
                let n = done.fetch_add(1, Ordering::Relaxed);
                if n.is_multiple_of(250) {
                    scan_log(&format!("progression {n}"));
                }
                (source.path.to_string_lossy().to_string(), entry)
            })
            .collect()
    };
    let parsed: Vec<(String, FileEntry)> = match &pool {
        Some(pool) => pool.install(parse_pending),
        None => parse_pending(),
    };

    let mut files = BTreeMap::new();
    for (key, entry) in ready.into_iter().chain(parsed) {
        files.insert(key, entry);
    }
    let snap = snapshot_of(&files);
    cache.rev = RATE_REV;
    cache.files = files;
    save_cache(&cache_path, &cache);
    publish_snap(shared, snap);
}

fn plan_for(provider: Provider, prev: &FileEntry, len: u64, mtime: i64) -> Plan {
    let fresh = prev.mtime == mtime && prev.len == len && prev.parsed_to >= len && len > 0;
    if provider == Provider::Grok {
        if fresh {
            Plan::Fresh
        } else {
            let start = if len < prev.parsed_to {
                0
            } else {
                prev.parsed_to.min(len)
            };
            Plan::Accumulate { start }
        }
    } else if provider == Provider::Cursor {
        if fresh { Plan::Fresh } else { Plan::Replace }
    } else if fresh {
        Plan::Fresh
    } else if prev.parsed_to > 0 && len >= prev.parsed_to && len >= prev.len {
        Plan::Append {
            start: prev.parsed_to,
        }
    } else {
        Plan::Replace
    }
}

fn publish(shared: &Shared, ready: &[(String, FileEntry)]) {
    let mut map = BTreeMap::new();
    for (key, entry) in ready {
        map.insert(key.clone(), entry.clone());
    }
    publish_snap(shared, snapshot_of(&map));
}

fn publish_snap(shared: &Shared, snap: Snapshot) {
    if let Ok(mut guard) = shared.snapshot.lock() {
        *guard = Arc::new(snap);
    }
    shared.generation.fetch_add(1, Ordering::Relaxed);
}

fn snapshot_of(files: &BTreeMap<String, FileEntry>) -> Snapshot {
    let mut buckets = Vec::new();
    let mut calls = 0;
    for entry in files.values() {
        buckets.extend(entry.buckets.iter().cloned());
        calls += entry.calls;
    }
    Snapshot {
        buckets,
        files: files.len(),
        calls,
    }
}

fn discover() -> Vec<Source> {
    let home = home_dir();
    let mut out = Vec::new();
    let mut jsonl = Vec::new();
    walk_jsonl(&home.join(".claude").join("projects"), &mut jsonl);
    for path in jsonl.drain(..) {
        out.push(Source {
            path,
            provider: Provider::Claude,
        });
    }
    walk_jsonl(&home.join(".codex").join("sessions"), &mut jsonl);
    for path in jsonl.drain(..) {
        out.push(Source {
            path,
            provider: Provider::Codex,
        });
    }
    walk_jsonl(&home.join(".grok").join("logs"), &mut jsonl);
    for path in jsonl.drain(..) {
        out.push(Source {
            path,
            provider: Provider::Grok,
        });
    }
    let cursor = home
        .join("AppData")
        .join("Roaming")
        .join("Cursor")
        .join("User")
        .join("globalStorage")
        .join("state.vscdb");
    if cursor.is_file() {
        out.push(Source {
            path: cursor,
            provider: Provider::Cursor,
        });
    }
    out
}

fn walk_jsonl(root: &Path, out: &mut Vec<PathBuf>) {
    if !root.exists() {
        return;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for ent in rd.flatten() {
            let path = ent.path();
            let name = ent.file_name();
            let name = name.to_string_lossy();
            if name == "node_modules" || name == "target" || name.starts_with('.') && path.is_dir()
            {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if name.ends_with(".jsonl") {
                out.push(path);
            }
        }
    }
}

fn parse_source(source: &Source, plan: &Plan, prev: &FileEntry) -> FileEntry {
    let (len, mtime) = file_sig(&source.path);
    match source.provider {
        Provider::Cursor => parse_cursor(&source.path, len, mtime),
        Provider::Grok => {
            let start = match plan {
                Plan::Accumulate { start } => *start,
                _ => 0,
            };
            parse_lines(source.provider, &source.path, start, prev, true, len, mtime)
        }
        _ => {
            let (start, base) = match plan {
                Plan::Append { start } => (*start, prev.clone()),
                _ => (0, FileEntry::default()),
            };
            parse_lines(
                source.provider,
                &source.path,
                start,
                &base,
                false,
                len,
                mtime,
            )
        }
    }
}

fn parse_lines(
    provider: Provider,
    path: &Path,
    start: u64,
    prev: &FileEntry,
    accumulate: bool,
    len: u64,
    mtime: i64,
) -> FileEntry {
    let mut model = prev.model.clone();
    let mut seen: HashSet<u64> = prev.seen.iter().copied().collect();
    let mut buckets = if accumulate || start > 0 {
        prev.buckets.clone()
    } else {
        Vec::new()
    };
    let mut calls = if accumulate || start > 0 {
        prev.calls
    } else {
        0
    };
    let mut index: HashMap<(String, String), usize> = HashMap::new();
    for (i, b) in buckets.iter().enumerate() {
        index.insert((b.day.clone(), b.model.clone()), i);
    }

    let consumed = match for_each_line(path, start, |line| {
        if let Some(found) = sniff_model(line)
            && (provider != Provider::Claude || found.contains("claude") || model.is_none())
            && (provider != Provider::Grok || line.contains("model changed"))
        {
            model = Some(found);
        }
        let Some(hit) = (match provider {
            Provider::Claude => parse_claude(line),
            Provider::Codex => parse_codex(line),
            Provider::Grok => parse_grok(line, &mut seen),
            _ => None,
        }) else {
            return;
        };
        let used_model = hit
            .model
            .clone()
            .or_else(|| model.clone())
            .unwrap_or_else(|| "inconnu".to_string());
        let day = hit
            .day
            .or_else(|| file_day(path))
            .unwrap_or_else(|| Local::now().date_naive());
        add_bucket(
            &mut buckets,
            &mut index,
            provider,
            &day,
            &used_model,
            hit.parts,
        );
        calls += 1;
    }) {
        Ok(n) => n,
        Err(_) => {
            return FileEntry {
                len,
                mtime,
                parsed_to: prev.parsed_to,
                ..prev.clone()
            };
        }
    };

    FileEntry {
        len,
        mtime,
        parsed_to: start + consumed,
        model,
        buckets,
        seen: if accumulate {
            seen.into_iter().collect()
        } else {
            Vec::new()
        },
        calls,
    }
}

struct Hit {
    day: Option<NaiveDate>,
    model: Option<String>,
    parts: Parts,
}

fn parse_claude(line: &str) -> Option<Hit> {
    if !line.contains("input_tokens") {
        return None;
    }
    let obj = object_after(line, "\"usage\":")?;
    let usage: Value = serde_json::from_str(obj).ok()?;
    let fresh = num(&usage["input_tokens"]);
    let cache_read = num(&usage["cache_read_input_tokens"]);
    let created = num(&usage["cache_creation_input_tokens"]);
    let mut write_5m = num(&usage["cache_creation"]["ephemeral_5m_input_tokens"]);
    let write_1h = num(&usage["cache_creation"]["ephemeral_1h_input_tokens"]);
    if write_5m + write_1h == 0 {
        write_5m = created;
    }
    let output = num(&usage["output_tokens"]);
    let parts = Parts {
        fresh,
        cache_read,
        write_5m,
        write_1h,
        output,
        prompt: fresh + cache_read + write_5m + write_1h,
    };
    if parts.is_empty() {
        return None;
    }
    Some(Hit {
        day: json_string(line, "timestamp").and_then(|s| parse_day(&s)),
        model: sniff_model(line).filter(|m| m.contains("claude")),
        parts,
    })
}

fn parse_codex(line: &str) -> Option<Hit> {
    if !line.contains("token_usage_record") {
        return None;
    }
    let v: Value = serde_json::from_str(line).ok()?;
    if v["type"].as_str() != Some("token_usage_record") {
        return None;
    }
    // `usage` est l’appel. `thread_token_usage` est le cumul : on ne le compte pas.
    let u = &v["payload"]["usage"];
    let input = num(&u["input_tokens"]);
    let cached = num(&u["cached_input_tokens"]).min(input);
    let write = num(&u["cache_write_input_tokens"]);
    let output = num(&u["output_tokens"]);
    let fresh = input.saturating_sub(cached);
    let parts = Parts {
        fresh,
        cache_read: cached,
        write_5m: write,
        write_1h: 0,
        output,
        prompt: input,
    };
    if parts.is_empty() {
        return None;
    }
    Some(Hit {
        day: v["timestamp"].as_str().and_then(parse_day),
        model: None,
        parts,
    })
}

fn parse_grok(line: &str, seen: &mut HashSet<u64>) -> Option<Hit> {
    if !line.contains("prompt_tokens") || !line.contains("inference_done") {
        return None;
    }
    let v: Value = serde_json::from_str(line).ok()?;
    let msg = v["msg"].as_str().unwrap_or("");
    if !msg.ends_with("inference_done") {
        return None;
    }
    let ctx = &v["ctx"];
    let prompt = num(&ctx["prompt_tokens"]);
    let cached = num(&ctx["cached_prompt_tokens"]).min(prompt);
    let output = num(&ctx["completion_tokens"]);
    let parts = Parts {
        fresh: prompt.saturating_sub(cached),
        cache_read: cached,
        write_5m: 0,
        write_1h: 0,
        output,
        prompt,
    };
    if parts.is_empty() {
        return None;
    }
    let ts = v["ts"].as_str().unwrap_or("");
    let loop_index = num(&ctx["loop_index"]);
    let hash = fnv(&format!("{ts}|{loop_index}|{prompt}|{cached}|{output}"));
    if !seen.insert(hash) {
        return None;
    }
    Some(Hit {
        day: parse_day(ts),
        model: ctx["model"].as_str().map(|s| s.to_string()),
        parts,
    })
}

fn parse_cursor(path: &Path, len: u64, mtime: i64) -> FileEntry {
    let mut entry = FileEntry {
        len,
        mtime,
        parsed_to: len,
        model: Some("cursor".to_string()),
        ..FileEntry::default()
    };
    let Ok(conn) = open_sqlite(path) else {
        return entry;
    };
    let mut composers: HashMap<String, NaiveDate> = HashMap::new();
    let mut bubbles: Vec<(String, String)> = Vec::new();
    let query = "SELECT key, value FROM cursorDiskKV WHERE key LIKE 'composerData:%' OR key LIKE 'bubbleId:%'";
    let mut stmt = match conn.prepare(query) {
        Ok(s) => s,
        Err(_) => return entry,
    };
    let rows = stmt.query_map([], |row| {
        let key: String = row.get(0)?;
        let value: Vec<u8> = row.get(1).unwrap_or_default();
        Ok((key, value))
    });
    let Ok(rows) = rows else { return entry };
    for row in rows.flatten() {
        let (key, value) = row;
        let text = String::from_utf8_lossy(&value);
        if let Some(id) = key.strip_prefix("composerData:") {
            if let Ok(v) = serde_json::from_str::<Value>(&text)
                && let Some(day) = v["createdAt"].as_i64().and_then(epoch_day)
            {
                composers.insert(id.to_string(), day);
            }
        } else if key.starts_with("bubbleId:") {
            bubbles.push((key, text.into_owned()));
        }
    }
    let mut index = HashMap::new();
    for (key, text) in bubbles {
        let rest = key.strip_prefix("bubbleId:").unwrap_or("");
        let Some((cid, _)) = rest.split_once(':') else {
            continue;
        };
        let Some(day) = composers.get(cid).copied() else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if v["type"].as_i64() != Some(2) {
            continue;
        }
        let tc = &v["tokenCount"];
        let input = num(&tc["inputTokens"]);
        let output = num(&tc["outputTokens"]);
        if input + output == 0 {
            continue;
        }
        let parts = Parts {
            fresh: input,
            cache_read: 0,
            write_5m: 0,
            write_1h: 0,
            output,
            prompt: input,
        };
        add_bucket(
            &mut entry.buckets,
            &mut index,
            Provider::Cursor,
            &day,
            "cursor",
            parts,
        );
        entry.calls += 1;
    }
    entry
}

fn add_bucket(
    buckets: &mut Vec<Bucket>,
    index: &mut HashMap<(String, String), usize>,
    provider: Provider,
    day: &NaiveDate,
    model: &str,
    parts: Parts,
) {
    let day = day.format("%Y-%m-%d").to_string();
    let cost = usd_to_micros(price_usd(model, parts));
    let key = (day.clone(), model.to_string());
    if let Some(&i) = index.get(&key) {
        let b = &mut buckets[i];
        b.input += parts.fresh;
        b.output += parts.output;
        b.cache_read += parts.cache_read;
        b.cache_write += parts.write_5m + parts.write_1h;
        b.cost_micros += cost;
    } else {
        index.insert(key, buckets.len());
        buckets.push(Bucket {
            day,
            provider,
            model: model.to_string(),
            input: parts.fresh,
            output: parts.output,
            cache_read: parts.cache_read,
            cache_write: parts.write_5m + parts.write_1h,
            cost_micros: cost,
        });
    }
}

fn for_each_line(path: &Path, start: u64, mut on_line: impl FnMut(&str)) -> std::io::Result<u64> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = start.min(len);
    file.seek(SeekFrom::Start(start))?;
    let mut reader = BufReader::new(file);
    let mut buf = Vec::new();
    let mut consumed = 0u64;
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break;
        }
        let newline = buf.last() == Some(&b'\n');
        if !newline && !trailing_json(&buf) {
            break;
        }
        let text = String::from_utf8_lossy(&buf);
        let line = text.trim_end_matches(['\r', '\n']);
        if !line.is_empty() {
            on_line(line);
        }
        consumed += n as u64;
        if !newline {
            break;
        }
    }
    Ok(consumed)
}

fn trailing_json(buf: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(buf) else {
        return false;
    };
    let text = text.trim();
    if !(text.ends_with('}') || text.ends_with(']')) {
        return false;
    }
    serde_json::from_str::<Value>(text).is_ok()
}

fn object_after<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let idx = line.find(key)?;
    let after = &line[idx + key.len()..];
    let rel = after.find('{')?;
    let between = &after[..rel];
    if between.chars().any(|c| !c.is_whitespace() && c != ':') {
        return None;
    }
    let start = idx + key.len() + rel;
    let bytes = line.as_bytes();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for i in start..bytes.len() {
        let c = bytes[i];
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&line[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

fn json_string(line: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let i = line.find(&pat)?;
    let rest = &line[i + pat.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn sniff_model(line: &str) -> Option<String> {
    let mut rest = line;
    let mut found = None;
    let pat = "\"model\":\"";
    while let Some(i) = rest.find(pat) {
        let s = &rest[i + pat.len()..];
        if let Some(end) = s.find('"') {
            let id = &s[..end];
            if is_model_id(id) {
                found = Some(id.to_string());
                if id.contains("claude")
                    || id.contains("gpt")
                    || id.contains("grok")
                    || id.contains("gemini")
                {
                    return Some(id.to_string());
                }
            }
        }
        rest = &rest[i + pat.len()..];
    }
    found
}

fn is_model_id(id: &str) -> bool {
    (3..64).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
}

fn num(v: &Value) -> u64 {
    match v {
        Value::Number(n) => n
            .as_u64()
            .or_else(|| n.as_f64().map(|f| f.max(0.0) as u64))
            .unwrap_or(0),
        _ => 0,
    }
}

fn parse_day(ts: &str) -> Option<NaiveDate> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(ts) {
        return Some(dt.with_timezone(&Local).date_naive());
    }
    let head = ts.get(..10)?;
    NaiveDate::parse_from_str(head, "%Y-%m-%d").ok()
}

fn epoch_day(n: i64) -> Option<NaiveDate> {
    let ms = if n < 10_000_000_000 {
        n.saturating_mul(1000)
    } else {
        n
    };
    DateTime::from_timestamp_millis(ms).map(|d| d.with_timezone(&Local).date_naive())
}

fn file_day(path: &Path) -> Option<NaiveDate> {
    let modified = path.metadata().ok()?.modified().ok()?;
    let secs = modified.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    DateTime::from_timestamp(secs, 0).map(|d| d.with_timezone(&Local).date_naive())
}

fn file_sig(path: &Path) -> (u64, i64) {
    let Ok(meta) = path.metadata() else {
        return (0, 0);
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    (meta.len(), mtime)
}

fn fnv(s: &str) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn open_sqlite(path: &Path) -> Result<rusqlite::Connection, rusqlite::Error> {
    let fwd = path.to_string_lossy().replace('\\', "/");
    let enc = fwd
        .replace(' ', "%20")
        .replace('#', "%23")
        .replace('?', "%3F");
    let uri = format!("file:{enc}?mode=ro&nolock=1");
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI;
    rusqlite::Connection::open_with_flags(&uri, flags)
}

fn load_cache(path: &Path) -> DiskCache {
    let empty = DiskCache {
        rev: RATE_REV,
        files: BTreeMap::new(),
    };
    let Ok(text) = fs::read_to_string(path) else {
        return empty;
    };
    match serde_json::from_str::<DiskCache>(&text) {
        Ok(cache) if cache.rev == RATE_REV => cache,
        _ => empty,
    }
}

fn save_cache(path: &Path, cache: &DiskCache) {
    let Ok(text) = serde_json::to_string(cache) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, text).is_ok() {
        let _ = fs::remove_file(path);
        if fs::rename(&tmp, path).is_err() {
            let _ = fs::remove_file(&tmp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_usage_is_not_doubled_by_iterations() {
        let line = r#"{"type":"assistant","timestamp":"2026-10-12T04:38:57.976Z","message":{"model":"claude-opus-5-5","usage":{"input_tokens":2,"cache_creation_input_tokens":100,"cache_read_input_tokens":50,"output_tokens":10,"cache_creation":{"ephemeral_1h_input_tokens":100,"ephemeral_5m_input_tokens":0},"iterations":[{"input_tokens":2,"output_tokens":10}]}}}"#;
        let hit = parse_claude(line).unwrap();
        assert_eq!(hit.parts.fresh, 2);
        assert_eq!(hit.parts.write_1h, 100);
        assert_eq!(hit.parts.cache_read, 50);
        assert_eq!(hit.parts.output, 10);
        assert_eq!(hit.model.as_deref(), Some("claude-opus-5-5"));
    }

    #[test]
    fn codex_uses_the_call_not_the_thread_total() {
        let line = r#"{"timestamp":"2026-10-28T05:45:35.858Z","type":"token_usage_record","payload":{"usage":{"input_tokens":1000,"cached_input_tokens":400,"cache_write_input_tokens":0,"output_tokens":25,"reasoning_output_tokens":10,"total_tokens":1025},"thread_token_usage":{"input_tokens":99999,"cached_input_tokens":1,"output_tokens":99999,"total_tokens":199998}}}"#;
        let hit = parse_codex(line).unwrap();
        assert_eq!(hit.parts.fresh, 600);
        assert_eq!(hit.parts.cache_read, 400);
        assert_eq!(hit.parts.output, 25);
    }

    #[test]
    fn grok_duplicate_event_is_ignored() {
        let line = r#"{"ts":"2026-10-20T14:12:10.102Z","msg":"shell.turn.inference_done","ctx":{"loop_index":1,"prompt_tokens":1000,"cached_prompt_tokens":100,"completion_tokens":20,"reasoning_tokens":5}}"#;
        let mut seen = HashSet::new();
        let first = parse_grok(line, &mut seen).unwrap();
        assert_eq!(first.parts.fresh, 900);
        assert_eq!(first.parts.output, 20);
        assert!(parse_grok(line, &mut seen).is_none());
    }
}

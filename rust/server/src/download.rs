//! The model, downloaded by the backend itself: from Hugging Face (the original checkpoint,
//! at a fixed revision), written as it arrives in the layout of Ollama's store under
//! ~/.cwbcode/models (or $CWBCODE_MODELS), where the loader looks for it. This is
//! scripts/model.py's Hugging Face path, in Rust, so that the Mac app (which has no Python, and
//! no `make download`) gets its model without anything else installed:
//!
//! - The safetensors headers are read with range requests; each tensor `X` goes into a small
//!   safetensors file of its own, with `X_scale` beside it for the quantized expert banks
//!   (labelled as Ollama labels them: the packed weight as U32 words, the scales as bytes).
//!   The draft network's tensors come from mtp.safetensors, prefixed `draft.`.
//! - Each file is named by its SHA-256, and every one is checked against Ollama's registry: the
//!   bytes are Ollama's, byte for byte.
//! - The token in $HF_TOKEN (or $HUGGING_FACE_HUB_TOKEN, or the one `hf auth login` saved) is
//!   used when there is one, and only ever sent to Hugging Face itself, never to the CDN it
//!   redirects to. The repository is public: without a token it works too.
//! - What is finished is kept in a state file: a download that stopped goes on from there.
//!
//! It runs on a thread (and a runtime) of its own; the server answers meanwhile, and Status
//! says how far it is. When it is done, the model thread loads the model.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use futures::{StreamExt, TryStreamExt};
use reqwest::Url;
use serde_json::{Map, Value, json};
use sha2::Digest;

use crate::worker::{Job, Shared};

/// The Hugging Face checkpoint of each model that can come from there, at a fixed revision:
/// the conversion is checked against that revision's files (as in scripts/model.py).
pub const HF_SOURCES: &[(&str, &str, &str)] = &[("inkling-2-mini:26b-a5b-nvfp4", "thinkingmachines/Inkling-2-Mini", "c9d028302cd0f035fdf8e7e91f089a4ce94d2195")];
const JSON_FILES: &[&str] = &["config.json", "generation_config.json", "hf_quant_config.json", "processor_config.json", "special_tokens_map.json", "tokenizer.json", "tokenizer_config.json"];
const DRAFT_FILE: &str = "mtp.safetensors";
/// Tensor files written at once.
const WORKERS: usize = 8;
/// Connections fetching pieces at once (over all the files being written).
const PIECES: usize = 16;
/// Bytes in a piece of a large tensor.
const PIECE: u64 = 32 << 20;
const USER_AGENT: &str = concat!("cwbcode-model-download/", env!("CARGO_PKG_VERSION"));
const TENSOR: &str = "application/vnd.ollama.image.tensor";
const JSON: &str = "application/vnd.ollama.image.json";

/// Where Hugging Face is (CWBCODE_HF_ENDPOINT, for tests) and Ollama's registry
/// (CWBCODE_OLLAMA_REGISTRY).
fn hub_endpoint() -> String {
    std::env::var("CWBCODE_HF_ENDPOINT").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "https://huggingface.co".into())
}

fn registry_endpoint() -> String {
    std::env::var("CWBCODE_OLLAMA_REGISTRY").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "https://registry.ollama.ai".into())
}

/// The repository and revision a model comes from, if it can be downloaded.
pub fn source_for(model: &str) -> Option<(&'static str, &'static str)> {
    let model = model.strip_prefix("ollama:").unwrap_or(model);
    HF_SOURCES.iter().find(|(name, ..)| *name == model).map(|(_, repo, revision)| (*repo, *revision))
}

// ---------- how far it is ----------

/// The download's progress, for Status.
#[derive(Default)]
pub struct Download {
    running: AtomicBool,
    /// Bytes of tensor data written (with what was already there when it started).
    done: AtomicU64,
    info: Mutex<Info>,
}

#[derive(Clone, Default, Debug)]
pub struct Info {
    pub total: u64,
    /// Bytes per second, over the last few seconds.
    pub speed: f64,
    /// Where it comes from: "Hugging Face, without a token", ...
    pub source: String,
    /// Why the last download stopped ("" while it runs, and after one that went well).
    pub error: String,
    /// What it is doing: "12 of 1250 files", "checking against Ollama's registry", or, when
    /// it is done, what the check found.
    pub note: String,
}

impl Download {
    /// Bytes done and the rest, each read once.
    pub fn snapshot(&self) -> (u64, Info) {
        let info = self.info.lock().unwrap().clone();
        (self.done.load(Ordering::Relaxed), info)
    }

    pub fn running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    fn update(&self, f: impl FnOnce(&mut Info)) {
        f(&mut self.info.lock().unwrap());
    }
}

/// Start downloading the model, unless it is already here or a download is running. The model
/// state goes to "downloading"; when it is done the model thread is asked to load it.
pub fn start(shared: &Arc<Shared>) -> Result<()> {
    let model = shared.config.model.clone();
    source_for(&model).with_context(|| format!("{model} cannot be downloaded by the backend (it has no Hugging Face source here)"))?;
    if crate::inkling::ollama::manifest_path(&model).is_some() {
        bail!("{model} is already downloaded");
    }
    if shared.download.running.swap(true, Ordering::SeqCst) {
        bail!("the model is already being downloaded");
    }
    *shared.model_state.lock().unwrap() = "downloading".into();
    shared.download.update(|info| *info = Info { note: "starting".into(), source: source(&token_here()), ..Info::default() });
    let starter = shared;
    let shared = shared.clone();
    let spawned = std::thread::Builder::new().name("download".into()).spawn(move || {
        // reqwest's rustls takes the process's crypto provider (the backend installs ring at
        // start; done again here, for whoever calls this first).
        rustls::crypto::ring::default_provider().install_default().ok();
        let result = tokio::runtime::Builder::new_multi_thread().worker_threads(4).thread_name("download-io").enable_all().build().map_err(anyhow::Error::from).and_then(|runtime| runtime.block_on(run(&shared)));
        match result {
            Ok(said) => {
                eprintln!("cwbcode: download: done; {said}");
                shared.download.update(|info| {
                    info.note = said;
                    info.speed = 0.0;
                });
                // Loaded by the model thread (MLX is per thread), without a restart.
                *shared.model_state.lock().unwrap() = "loading".into();
                shared.download.running.store(false, Ordering::SeqCst);
                if let Err(e) = shared.submit(Job::Load) {
                    *shared.model_state.lock().unwrap() = format!("failed: {e:#}");
                }
            }
            Err(e) => {
                eprintln!("cwbcode: download: stopped: {e:#}");
                shared.download.update(|info| {
                    info.error = format!("{e:#}");
                    info.speed = 0.0;
                });
                *shared.model_state.lock().unwrap() = "missing".into();
                shared.download.running.store(false, Ordering::SeqCst);
            }
        }
    });
    if let Err(e) = spawned {
        *starter.model_state.lock().unwrap() = "missing".into();
        starter.download.running.store(false, Ordering::SeqCst);
        bail!("cannot start the download: {e}");
    }
    Ok(())
}

// ---------- the token ----------

/// The token this backend would use (see `hf_token`).
fn token_here() -> Option<(String, String)> {
    hf_token(|var| std::env::var(var).ok(), &PathBuf::from(std::env::var("HOME").unwrap_or_default()))
}

/// Where the download comes from, as Status says it.
fn source(token: &Option<(String, String)>) -> String {
    match token {
        Some((_, from)) => format!("Hugging Face, with the token from {from}"),
        None => "Hugging Face, without a token".to_string(),
    }
}

/// The Hugging Face token and where it came from, if there is one: $HF_TOKEN, then
/// $HUGGING_FACE_HUB_TOKEN, then the file $HF_TOKEN_PATH names, then $HF_HOME/token
/// (~/.cache/huggingface/token by default), as huggingface_hub looks for it.
pub fn hf_token(env: impl Fn(&str) -> Option<String>, home: &Path) -> Option<(String, String)> {
    for var in ["HF_TOKEN", "HUGGING_FACE_HUB_TOKEN"] {
        if let Some(value) = env(var).map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) {
            return Some((value, format!("${var}")));
        }
    }
    let hf_home = env("HF_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home.join(".cache/huggingface"));
    let paths = [env("HF_TOKEN_PATH").filter(|v| !v.is_empty()).map(PathBuf::from), Some(hf_home.join("token"))];
    for path in paths.into_iter().flatten() {
        if let Ok(text) = std::fs::read_to_string(&path) {
            let text = text.trim();
            if !text.is_empty() {
                return Some((text.to_string(), path.display().to_string()));
            }
        }
    }
    None
}

/// Is the token to go with a request to `target`? Only to the hub itself (the same scheme,
/// host and port), never to where it redirects for the bytes.
pub fn token_goes_to(hub: &Url, target: &Url) -> bool {
    hub.scheme() == target.scheme() && hub.host_str() == target.host_str() && hub.port_or_known_default() == target.port_or_known_default()
}

// ---------- Hugging Face ----------

/// An HTTP status that was not what was wanted (kept as a type: a refused token is tried
/// again without it).
#[derive(Debug)]
struct HttpStatus(u16, String);

impl std::fmt::Display for HttpStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP {} for {}", self.0, self.1)
    }
}

impl std::error::Error for HttpStatus {}

struct Hub {
    client: reqwest::Client,
    base: Url,
    repo: String,
    revision: String,
    token: Option<String>,
    /// file -> (the address of its bytes on the CDN, when it was given)
    signed: Mutex<HashMap<String, (Url, Instant)>>,
}

fn client() -> Result<reqwest::Client> {
    // reqwest's rustls takes the process's crypto provider: ring, as the server uses.
    rustls::crypto::ring::default_provider().install_default().ok();
    Ok(reqwest::Client::builder()
        .user_agent(USER_AGENT)
        // Redirects are followed by hand: the token goes to the hub only.
        .redirect(reqwest::redirect::Policy::none())
        // One connection per piece, as the proven recipe does: HTTP/2 would put them all on one.
        .http1_only()
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60))
        .build()?)
}

impl Hub {
    fn new(base: &str, repo: &str, revision: &str, token: Option<String>) -> Result<Self> {
        Ok(Self { client: client()?, base: Url::parse(base).with_context(|| format!("not an address: {base}"))?, repo: repo.into(), revision: revision.into(), token, signed: Mutex::new(HashMap::new()) })
    }

    fn url(&self, file: &str) -> Result<Url> {
        let mut url = self.base.clone();
        url.path_segments_mut().map_err(|_| anyhow::anyhow!("{} cannot be a base address", self.base))?.pop_if_empty().extend(self.repo.split('/')).push("resolve").push(&self.revision).extend(file.split('/'));
        Ok(url)
    }

    /// GET `url` (with a range), following redirects by hand: the token goes only with
    /// requests to the hub. The response and the address that answered.
    async fn send(&self, mut url: Url, range: Option<(u64, u64)>) -> Result<(reqwest::Response, Url)> {
        for _ in 0..10 {
            let mut request = self.client.get(url.clone());
            if let Some((start, end)) = range {
                request = request.header(reqwest::header::RANGE, format!("bytes={start}-{}", end - 1));
            }
            if let Some(token) = &self.token {
                if token_goes_to(&self.base, &url) {
                    request = request.bearer_auth(token);
                }
            }
            let response = request.send().await.map_err(|e| anyhow::Error::new(e.without_url())).with_context(|| without_query(&url))?;
            let status = response.status();
            if status.is_redirection() {
                let location = response.headers().get(reqwest::header::LOCATION).and_then(|v| v.to_str().ok()).with_context(|| format!("a redirect without a location from {}", without_query(&url)))?;
                url = url.join(location)?;
                continue;
            }
            if !status.is_success() {
                return Err(HttpStatus(status.as_u16(), without_query(&url)).into());
            }
            return Ok((response, url));
        }
        bail!("too many redirects from {}", without_query(&url))
    }

    /// Where a file's bytes are: the address on the CDN the hub redirects to, kept for twenty
    /// minutes (they are good for about an hour).
    async fn cdn(&self, file: &str, fresh: bool) -> Result<Url> {
        if !fresh {
            if let Some((url, when)) = self.signed.lock().unwrap().get(file) {
                if when.elapsed() < Duration::from_secs(1200) {
                    return Ok(url.clone());
                }
            }
        }
        let (response, url) = self.send(self.url(file)?, Some((0, 1))).await?;
        drop(response);
        self.signed.lock().unwrap().insert(file.to_string(), (url.clone(), Instant::now()));
        Ok(url)
    }

    async fn get(&self, file: &str) -> Result<Vec<u8>> {
        let (response, _) = self.send(self.url(file)?, None).await?;
        Ok(response.bytes().await?.to_vec())
    }

    /// Bytes [start, end) of a file, to `sink` as they arrive; a broken connection is picked
    /// up where it left off.
    async fn range(&self, file: &str, start: u64, end: u64, mut sink: impl FnMut(u64, &[u8]) -> std::io::Result<()>) -> Result<()> {
        let (mut pos, mut attempt) = (start, 0u32);
        while pos < end {
            let result: Result<()> = async {
                let url = self.cdn(file, attempt > 0).await?;
                let (mut response, _) = self.send(url, Some((pos, end))).await?;
                if response.status().as_u16() != 206 && pos != 0 {
                    bail!("the server ignored the range (HTTP {})", response.status().as_u16());
                }
                while pos < end {
                    let Some(chunk) = response.chunk().await.map_err(|e| e.without_url())? else { break };
                    let take = chunk.len().min((end - pos) as usize);
                    sink(pos, &chunk[..take])?;
                    pos += take as u64;
                }
                if pos < end {
                    bail!("the connection closed early");
                }
                Ok(())
            }
            .await;
            if let Err(e) = result {
                attempt += 1;
                if attempt >= 6 || e.downcast_ref::<HttpStatus>().is_some_and(|s| s.0 == 401 || s.0 == 403 || s.0 == 404) {
                    return Err(e.context(format!("{file} bytes {pos}-{end}")));
                }
                tokio::time::sleep(Duration::from_secs((1u64 << attempt).min(20))).await;
            }
        }
        Ok(())
    }

    /// A safetensors file's header (in file order) and where its data starts.
    async fn header(&self, file: &str) -> Result<(Map<String, Value>, u64)> {
        let mut len = Vec::new();
        self.range(file, 0, 8, |_, b| {
            len.extend_from_slice(b);
            Ok(())
        })
        .await?;
        let n = u64::from_le_bytes(len[..8].try_into()?);
        if n > 100 << 20 {
            bail!("{file}: a header of {n} bytes is not a safetensors header");
        }
        let mut body = Vec::new();
        self.range(file, 8, 8 + n, |_, b| {
            body.extend_from_slice(b);
            Ok(())
        })
        .await?;
        let head: Value = serde_json::from_slice(&body).with_context(|| format!("{file}: the header is not JSON"))?;
        match head {
            Value::Object(map) => Ok((map, 8 + n)),
            _ => bail!("{file}: the header is not a JSON object"),
        }
    }
}

fn without_query(url: &Url) -> String {
    let mut shown = url.clone();
    shown.set_query(None);
    shown.to_string()
}

// ---------- the conversion (pure) ----------

/// A tensor's bytes in a source file, and how it is labelled in the file written.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub source: String,
    pub start: u64,
    pub end: u64,
    pub name: String,
    pub dtype: String,
    pub shape: Vec<u64>,
}

/// One tensor file of the model: its name in the manifest, its tensors, its metadata.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub name: String,
    pub parts: Vec<Part>,
    pub meta: Option<(&'static str, &'static str)>,
}

impl Group {
    pub fn data_bytes(&self) -> u64 {
        self.parts.iter().map(|p| p.end - p.start).sum()
    }
}

fn entry(head: &Map<String, Value>, name: &str) -> Result<(String, Vec<u64>, u64, u64)> {
    let t = &head[name];
    let dtype = t["dtype"].as_str().with_context(|| format!("{name}: no dtype"))?.to_string();
    let shape = t["shape"].as_array().with_context(|| format!("{name}: no shape"))?.iter().map(|d| d.as_u64().context("a dimension that is not a number")).collect::<Result<Vec<_>>>()?;
    let offsets = t["data_offsets"].as_array().with_context(|| format!("{name}: no data_offsets"))?;
    let (a, b) = (offsets.first().and_then(Value::as_u64).context("data_offsets")?, offsets.get(1).and_then(Value::as_u64).context("data_offsets")?);
    if b < a {
        bail!("{name}: data_offsets go backwards");
    }
    Ok((dtype, shape, a, b))
}

/// The tensor files of one shard of the checkpoint (whose data starts at `base`): every
/// tensor, by name, with its `_scale` beside it when it has one. A quantized expert bank is
/// NVFP4 packed in bytes (U8) or MXFP8 (F8_E4M3); Ollama labels the packed weight as 32-bit
/// words and the scales as bytes, with the format in the file's metadata. The bytes are the
/// same.
pub fn plan_shard(shard: &str, head: &Map<String, Value>, base: u64) -> Result<Vec<Group>> {
    let mut names: Vec<&String> = head.keys().filter(|k| *k != "__metadata__").collect();
    names.sort();
    let mut groups = Vec::new();
    for name in &names {
        if let Some(weight) = name.strip_suffix("_scale") {
            if head.contains_key(weight) {
                continue; // goes with its weight
            }
        }
        let (dtype, shape, a, b) = entry(head, name)?;
        let scale_name = format!("{name}_scale");
        let group = if head.contains_key(&scale_name) {
            let meta = match dtype.as_str() {
                "U8" => ("16", "nvfp4"),
                "F8_E4M3" => ("32", "mxfp8"),
                other => bail!("{name}: unexpected quantized dtype {other}"),
            };
            let (_, scale_shape, sa, sb) = entry(head, &scale_name)?;
            let mut packed = shape.clone();
            let last = packed.last_mut().with_context(|| format!("{name}: a quantized tensor without dimensions"))?;
            *last /= 4;
            Group {
                name: name.to_string(),
                parts: vec![
                    Part { source: shard.into(), start: base + a, end: base + b, name: name.to_string(), dtype: "U32".into(), shape: packed },
                    Part { source: shard.into(), start: base + sa, end: base + sb, name: scale_name, dtype: "U8".into(), shape: scale_shape },
                ],
                meta: Some(meta),
            }
        } else {
            Group { name: name.to_string(), parts: vec![Part { source: shard.into(), start: base + a, end: base + b, name: name.to_string(), dtype, shape }], meta: None }
        };
        groups.push(group);
    }
    Ok(groups)
}

/// The draft network's tensor files: each tensor of mtp.safetensors on its own, named
/// `draft.<name>`.
pub fn plan_draft(file: &str, head: &Map<String, Value>, base: u64) -> Result<Vec<Group>> {
    let mut names: Vec<&String> = head.keys().filter(|k| *k != "__metadata__").collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let (dtype, shape, a, b) = entry(head, name)?;
            let prefixed = format!("draft.{name}");
            Ok(Group { name: prefixed.clone(), parts: vec![Part { source: file.into(), start: base + a, end: base + b, name: prefixed, dtype, shape }], meta: None })
        })
        .collect()
}

/// A tensor file's header exactly as Ollama writes one: its length (8 bytes, little-endian),
/// then compact JSON with the metadata first and the tensors in order, padded with spaces to
/// a multiple of eight.
pub fn tensor_header(group: &Group) -> Vec<u8> {
    let mut entries = Map::new();
    if let Some((group_size, quant_type)) = group.meta {
        entries.insert("__metadata__".into(), json!({"group_size": group_size, "quant_type": quant_type}));
    }
    let mut offset = 0;
    for p in &group.parts {
        let n = p.end - p.start;
        entries.insert(p.name.clone(), json!({"dtype": p.dtype, "shape": p.shape, "data_offsets": [offset, offset + n]}));
        offset += n;
    }
    let mut raw = serde_json::to_vec(&Value::Object(entries)).expect("JSON of plain values");
    raw.resize(raw.len().div_ceil(8) * 8, b' ');
    let mut out = (raw.len() as u64).to_le_bytes().to_vec();
    out.extend(raw);
    out
}

/// JSON as Python's `json.dumps` writes it by default (", " and ": ", non-ASCII escaped,
/// floats as Python prints them): the config files written here are then byte for byte the
/// ones Ollama (and scripts/model.py) write.
pub fn python_json(value: &Value) -> String {
    fn float(f: f64) -> String {
        if !f.is_finite() {
            return if f.is_nan() { "NaN".into() } else if f > 0.0 { "Infinity".into() } else { "-Infinity".into() };
        }
        let sci = format!("{f:e}");
        let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
        let exp: i32 = exp.parse().unwrap_or(0);
        if !(-4..16).contains(&exp) {
            format!("{mantissa}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
        } else {
            let plain = format!("{f}");
            if plain.contains('.') { plain } else { format!("{plain}.0") }
        }
    }
    fn string(s: &str, out: &mut String) {
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                    let mut units = [0u16; 2];
                    for unit in c.encode_utf16(&mut units) {
                        out.push_str(&format!("\\u{unit:04x}"));
                    }
                }
                c => out.push(c),
            }
        }
        out.push('"');
    }
    fn write(value: &Value, out: &mut String) {
        match value {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
                (Some(i), ..) if !n.is_f64() => out.push_str(&i.to_string()),
                (_, Some(u), _) if !n.is_f64() => out.push_str(&u.to_string()),
                (.., Some(f)) => out.push_str(&float(f)),
                _ => out.push_str(&n.to_string()),
            },
            Value::String(s) => string(s, out),
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    write(item, out);
                }
                out.push(']');
            }
            Value::Object(map) => {
                out.push('{');
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    string(k, out);
                    out.push_str(": ");
                    write(v, out);
                }
                out.push('}');
            }
        }
    }
    let mut out = String::new();
    write(value, &mut out);
    out
}

/// The draft network's config: config.json's `mtp_config`, with its architecture.
pub fn draft_config(config: &Value) -> Result<Vec<u8>> {
    let mut draft = config["mtp_config"].as_object().context("config.json has no mtp_config")?.clone();
    draft.insert("architectures".into(), json!(["Inkling2ForConditionalGenerationMTP"]));
    Ok(python_json(&Value::Object(draft)).into_bytes())
}

// ---------- writing ----------

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

static PARTIAL: AtomicU64 = AtomicU64::new(0);

fn partial(blobs: &Path) -> PathBuf {
    blobs.join(format!(".partial-{}-{}", std::process::id(), PARTIAL.fetch_add(1, Ordering::Relaxed)))
}

/// Hash a finished file and give it its name: (digest, size).
fn seal(blobs: &Path, tmp: &Path) -> Result<(String, u64)> {
    let mut sha = sha2::Sha256::new();
    let mut file = std::fs::File::open(tmp)?;
    let mut buffer = vec![0u8; 8 << 20];
    let mut size = 0u64;
    loop {
        let n = std::io::Read::read(&mut file, &mut buffer)?;
        if n == 0 {
            break;
        }
        sha.update(&buffer[..n]);
        size += n as u64;
    }
    let digest = format!("sha256:{}", hex(&sha.finalize()));
    std::fs::rename(tmp, blobs.join(digest.replace(':', "-")))?;
    Ok((digest, size))
}

fn write_bytes(blobs: &Path, data: &[u8]) -> Result<(String, u64)> {
    let tmp = partial(blobs);
    std::fs::write(&tmp, data)?;
    seal(blobs, &tmp)
}

/// Write one tensor file: the header, then each tensor's bytes fetched in pieces over several
/// connections at once, each written at its place; hashed when it is whole.
async fn write_group(hub: &Hub, blobs: &Path, group: &Group, pieces: &tokio::sync::Semaphore, progress: &Download) -> Result<(String, u64)> {
    let head = tensor_header(group);
    let tmp = partial(blobs);
    let file = {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&head)?;
        f.set_len(head.len() as u64 + group.data_bytes())?;
        f
    };
    let mut jobs = Vec::new();
    let mut at = head.len() as u64;
    for p in &group.parts {
        let mut lo = p.start;
        while lo < p.end {
            let hi = (lo + PIECE).min(p.end);
            jobs.push((p.source.as_str(), lo, hi, at + lo - p.start));
            lo = hi;
        }
        at += p.end - p.start;
    }
    let file = &file;
    let fetched = futures::future::try_join_all(jobs.into_iter().map(|(source, lo, hi, place)| async move {
        let _permit = pieces.acquire().await?;
        hub.range(source, lo, hi, |pos, chunk| {
            file.write_all_at(chunk, place + pos - lo)?;
            progress.done.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            Ok(())
        })
        .await
    }))
    .await;
    if let Err(e) = fetched {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let blobs = blobs.to_path_buf();
    tokio::task::spawn_blocking(move || seal(&blobs, &tmp)).await?
}

/// Free bytes where `path` is.
fn free_space(path: &Path) -> Option<u64> {
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: statvfs fills the struct passed to it, from a NUL-terminated path.
    (unsafe { libc::statvfs(c.as_ptr(), &mut stat) } == 0).then(|| stat.f_bavail as u64 * stat.f_frsize)
}

fn gb(n: u64) -> String {
    format!("{:.1} GB", n as f64 / 1e9)
}

pub fn manifest_in(store: &Path, model: &str) -> PathBuf {
    let model = model.strip_prefix("ollama:").unwrap_or(model);
    let (name, tag) = model.split_once(':').unwrap_or((model, "latest"));
    store.join("manifests/registry.ollama.ai/library").join(name).join(tag)
}

/// One download, start to end: what it found to say at the end.
async fn run(shared: &Shared) -> Result<String> {
    let model = shared.config.model.strip_prefix("ollama:").unwrap_or(&shared.config.model).to_string();
    let (repo, revision) = source_for(&model).context("no Hugging Face source")?;
    let store = crate::inkling::ollama::own_store();
    let blobs = store.join("blobs");
    std::fs::create_dir_all(&blobs).with_context(|| format!("cannot create {}", blobs.display()))?;
    // One download into a store at a time, also across programs (another backend, the app).
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(store.join(".download.lock"))?;
    // SAFETY: flock on a file this function keeps open until it returns.
    if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        bail!("another program is downloading into {} right now", store.display());
    }
    // What a download that stopped left half written.
    for entry in std::fs::read_dir(&blobs)?.flatten() {
        if entry.file_name().to_string_lossy().starts_with(".partial-") {
            let _ = std::fs::remove_file(entry.path());
        }
    }

    let found = token_here();
    let base = hub_endpoint();
    let progress = &shared.download;
    progress.update(|info| {
        info.source = source(&found);
        info.note = "reading the checkpoint's index".into();
    });
    eprintln!("cwbcode: download: {model} from {base}/{repo} (revision {}), {}, into {}", &revision[..12], source(&found).trim_start_matches("Hugging Face, "), store.display());

    let mut hub = Hub::new(&base, repo, revision, found.as_ref().map(|(t, _)| t.clone()))?;
    let groups = match plan(&hub).await {
        Ok(groups) => groups,
        Err(e) if found.is_some() && e.downcast_ref::<HttpStatus>().is_some_and(|s| s.0 == 401 || s.0 == 403) => {
            eprintln!("cwbcode: download: the token was refused ({e}); trying without it");
            progress.update(|info| info.source = "Hugging Face, without a token (the token was refused)".into());
            hub = Hub::new(&base, repo, revision, None)?;
            plan(&hub).await?
        }
        Err(e) => return Err(e.context(format!("Hugging Face: {repo}"))),
    };
    let total: u64 = groups.iter().map(Group::data_bytes).sum();

    // Picked up where an earlier download stopped: what is done is kept in this file (the
    // same file scripts/model.py keeps).
    let state_path = store.join(format!(".download-{}.json", model.replace([':', '/'], "-")));
    let mut state: Value = std::fs::read(&state_path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null);
    if state["revision"].as_str() != Some(revision) || !state["done"].is_object() {
        state = json!({"revision": revision, "done": {}});
    }
    let mut done: HashMap<String, (String, u64)> = HashMap::new();
    for (name, v) in state["done"].as_object().into_iter().flatten() {
        if let (Some(digest), Some(size)) = (v["digest"].as_str(), v["size"].as_u64()) {
            if blobs.join(digest.replace(':', "-")).is_file() {
                done.insert(name.clone(), (digest.to_string(), size));
            }
        }
    }
    let already: u64 = groups.iter().filter(|g| done.contains_key(&g.name)).map(Group::data_bytes).sum();
    let mut todo: Vec<&Group> = groups.iter().filter(|g| !done.contains_key(&g.name)).collect();
    // The largest first, so that the many small ones fill in at the end.
    todo.sort_by_key(|g| std::cmp::Reverse(g.data_bytes()));
    let need = total - already + (2 << 30);
    if let Some(free) = free_space(&blobs) {
        if free < need {
            bail!("not enough disk space in {}: {} needed, {} free", blobs.display(), gb(need), gb(free));
        }
    }
    if already > 0 {
        eprintln!("cwbcode: download: resuming: {} of {} tensor files ({}) are already here", groups.len() - todo.len(), groups.len(), gb(already));
    }
    progress.done.store(already, Ordering::Relaxed);
    let count = groups.len();
    progress.update(|info| {
        info.total = total;
        info.note = format!("{} of {count} files", done.len());
    });

    let save = |done: &HashMap<String, (String, u64)>| -> Result<()> {
        let mut map = Map::new();
        for (name, (digest, size)) in done {
            map.insert(name.clone(), json!({"digest": digest, "size": size}));
        }
        let tmp = state_path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(&json!({"revision": revision, "done": map}))?)?;
        std::fs::rename(&tmp, &state_path)?;
        Ok(())
    };

    // How fast, over the last five seconds, measured twice a second (not per Status call:
    // several pages asking would make it jump); and a line in the log now and then.
    let started = Instant::now();
    let ticker = {
        let total = total;
        let started_with = already;
        async move {
            let mut samples: VecDeque<(Instant, u64)> = VecDeque::new();
            let mut said = Instant::now();
            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;
                let now = Instant::now();
                let bytes = progress.done.load(Ordering::Relaxed);
                samples.push_back((now, bytes));
                while samples.len() > 2 && now.duration_since(samples[0].0) > Duration::from_secs(5) {
                    samples.pop_front();
                }
                let (t0, d0) = samples[0];
                let speed = if now > t0 { (bytes - d0) as f64 / now.duration_since(t0).as_secs_f64() } else { 0.0 };
                progress.update(|info| info.speed = speed);
                if said.elapsed() >= Duration::from_secs(30) {
                    said = now;
                    let average = (bytes - started_with) as f64 / started.elapsed().as_secs_f64();
                    eprintln!("cwbcode: download: {} of {} ({:.1}%), {:.1} MB/s now, {:.1} MB/s on average", gb(bytes), gb(total), bytes as f64 / total.max(1) as f64 * 100.0, speed / 1e6, average / 1e6);
                }
            }
        }
    };

    let pieces = tokio::sync::Semaphore::new(PIECES);
    let work = async {
        let mut results = futures::stream::iter(todo.iter().map(|g| {
            let (hub, blobs, pieces) = (&hub, &blobs, &pieces);
            async move { write_group(hub, blobs, g, pieces, progress).await.map(|r| (g.name.clone(), r)).with_context(|| g.name.clone()) }
        }))
        .buffer_unordered(WORKERS);
        while let Some((name, result)) = results.try_next().await? {
            done.insert(name, result);
            save(&done)?;
            let n = done.len();
            progress.update(|info| info.note = format!("{n} of {count} files"));
        }
        anyhow::Ok(())
    };
    tokio::select! {
        result = work => result?,
        _ = ticker => unreachable!("the ticker does not end"),
    }
    let fetched = progress.done.load(Ordering::Relaxed) - already;
    let seconds = started.elapsed().as_secs_f64();
    eprintln!("cwbcode: download: tensors done: {} fetched in {:.0} s ({:.1} MB/s on average)", gb(fetched), seconds, fetched as f64 / seconds.max(1e-6) / 1e6);

    // The config and tokenizer files as they are, and the draft network's config (its part
    // of config.json).
    progress.update(|info| {
        info.note = "config and tokenizer files".into();
        info.speed = 0.0;
    });
    let mut layers: Vec<Value> = groups.iter().map(|g| {
        let (digest, size) = &done[&g.name];
        json!({"mediaType": TENSOR, "digest": digest, "size": size, "name": g.name})
    }).collect();
    let mut config = Value::Null;
    for file in JSON_FILES {
        let data = hub.get(file).await.with_context(|| file.to_string())?;
        if *file == "config.json" {
            config = serde_json::from_slice(&data).context("config.json is not JSON")?;
        }
        let (digest, size) = write_bytes(&blobs, &data)?;
        layers.push(json!({"mediaType": JSON, "digest": digest, "size": size, "name": file}));
    }
    let (digest, size) = write_bytes(&blobs, &draft_config(&config)?)?;
    layers.push(json!({"mediaType": JSON, "digest": digest, "size": size, "name": "draft/config.json"}));
    let image_config = json!({"model_format": "safetensors", "capabilities": ["completion", "vision", "tools", "thinking"],
        "draft": {"model_format": "safetensors", "architecture": "Inkling2ForConditionalGenerationMTP", "tensor_prefix": "draft.", "config": "draft/config.json"},
        "source": format!("huggingface.co/{repo}@{revision}")});
    let (digest, size) = write_bytes(&blobs, python_json(&image_config).as_bytes())?;
    let manifest = json!({"schemaVersion": 2, "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
        "config": {"mediaType": "application/vnd.docker.container.image.v1+json", "digest": digest, "size": size},
        "layers": layers});

    // Every tensor file should be the one in Ollama's registry, byte for byte.
    progress.update(|info| info.note = "checking against Ollama's registry".into());
    let said = match registry_digests(&model).await {
        Ok(theirs) => {
            let ours = groups.len();
            let bad: Vec<String> = groups.iter().filter(|g| theirs.get(&g.name) != Some(&done[&g.name].0)).map(|g| g.name.clone()).collect();
            if bad.is_empty() && ours == theirs.len() {
                let said = format!("verified: all {ours} tensor files match Ollama's registry (SHA-256)");
                eprintln!("cwbcode: download: {said}");
                said
            } else {
                let first = bad.first().cloned().unwrap_or_else(|| "one the registry has and the checkpoint does not".into());
                for name in &bad {
                    done.remove(name);
                }
                save(&done)?;
                bail!("{} of {ours} tensor files do not match Ollama's registry (first: {first}); download again to fetch them", bad.len());
            }
        }
        Err(e) => {
            let said = format!("not checked against Ollama's registry ({e:#}); sizes are as Hugging Face gave them");
            eprintln!("cwbcode: download: {said}");
            said
        }
    };

    let path = manifest_in(&store, &model);
    std::fs::create_dir_all(path.parent().expect("a manifest path has a folder"))?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, python_json(&manifest))?;
    std::fs::rename(&tmp, &path)?;
    let _ = std::fs::remove_file(&state_path);
    progress.done.store(total, Ordering::Relaxed);
    eprintln!("cwbcode: download: manifest {}", path.display());
    Ok(said)
}

/// Every tensor file of the model, from the checkpoint's headers (a few MB read).
async fn plan(hub: &Hub) -> Result<Vec<Group>> {
    let index: Value = serde_json::from_slice(&hub.get("model.safetensors.index.json").await?).context("the checkpoint's index is not JSON")?;
    let mut shards: Vec<&str> = index["weight_map"].as_object().context("the index has no weight_map")?.values().filter_map(Value::as_str).collect();
    shards.sort();
    shards.dedup();
    let mut groups = Vec::new();
    for shard in shards {
        let (head, base) = hub.header(shard).await?;
        groups.extend(plan_shard(shard, &head, base)?);
    }
    let (head, base) = hub.header(DRAFT_FILE).await?;
    groups.extend(plan_draft(DRAFT_FILE, &head, base)?);
    Ok(groups)
}

/// The tensor files' digests in Ollama's registry, by name.
async fn registry_digests(model: &str) -> Result<HashMap<String, String>> {
    let (name, tag) = model.split_once(':').unwrap_or((model, "latest"));
    let url = format!("{}/v2/library/{name}/manifests/{tag}", registry_endpoint().trim_end_matches('/'));
    let response = client()?.get(&url).header(reqwest::header::ACCEPT, "application/vnd.docker.distribution.manifest.v2+json").send().await?;
    if !response.status().is_success() {
        bail!("HTTP {} for {url}", response.status().as_u16());
    }
    let manifest: Value = response.json().await?;
    Ok(manifest["layers"]
        .as_array()
        .context("the registry's manifest has no layers")?
        .iter()
        .filter(|l| l["mediaType"] == TENSOR)
        .filter_map(|l| Some((l["name"].as_str()?.to_string(), l["digest"].as_str()?.to_string())))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn expert_banks_go_with_their_scales_relabelled_as_ollama_labels_them() {
        let h = head(json!({
            "__metadata__": {"format": "pt"},
            "b.w13_weight": {"dtype": "U8", "shape": [4, 8, 16], "data_offsets": [0, 512]},
            "a.norm": {"dtype": "BF16", "shape": [8], "data_offsets": [512, 528]},
            "b.w13_weight_scale": {"dtype": "F8_E4M3", "shape": [4, 8, 2], "data_offsets": [528, 592]},
            "c.w2_weight": {"dtype": "F8_E4M3", "shape": [2, 64], "data_offsets": [592, 720]},
            "c.w2_weight_scale": {"dtype": "U8", "shape": [2, 2], "data_offsets": [720, 724]},
            "lonely_scale": {"dtype": "F32", "shape": [1], "data_offsets": [724, 728]},
        }));
        let groups = plan_shard("s.safetensors", &h, 100).unwrap();
        let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
        // By name, the scales inside their weights' files; a "_scale" with no weight is a tensor.
        assert_eq!(names, ["a.norm", "b.w13_weight", "c.w2_weight", "lonely_scale"]);
        assert_eq!(groups[0].meta, None);
        assert_eq!(groups[0].parts[0], Part { source: "s.safetensors".into(), start: 612, end: 628, name: "a.norm".into(), dtype: "BF16".into(), shape: vec![8] });
        let nv = &groups[1];
        assert_eq!(nv.meta, Some(("16", "nvfp4")));
        assert_eq!((nv.parts[0].dtype.as_str(), nv.parts[0].shape.clone(), nv.parts[0].start, nv.parts[0].end), ("U32", vec![4, 8, 4], 100, 612));
        assert_eq!((nv.parts[1].name.as_str(), nv.parts[1].dtype.as_str(), nv.parts[1].shape.clone()), ("b.w13_weight_scale", "U8", vec![4, 8, 2]));
        let mx = &groups[2];
        assert_eq!(mx.meta, Some(("32", "mxfp8")));
        assert_eq!((mx.parts[0].dtype.as_str(), mx.parts[0].shape.clone()), ("U32", vec![2, 16]));
        assert_eq!(mx.data_bytes(), 128 + 4);
        let bad = head(json!({"x": {"dtype": "BF16", "shape": [2], "data_offsets": [0, 4]}, "x_scale": {"dtype": "U8", "shape": [1], "data_offsets": [4, 5]}}));
        assert!(plan_shard("s", &bad, 0).unwrap_err().to_string().contains("unexpected quantized dtype BF16"));

        let draft = plan_draft("mtp.safetensors", &head(json!({"__metadata__": {}, "z": {"dtype": "BF16", "shape": [2], "data_offsets": [0, 4]}, "a": {"dtype": "F32", "shape": [1], "data_offsets": [4, 8]}})), 16).unwrap();
        assert_eq!(draft.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(), ["draft.a", "draft.z"]);
        assert_eq!((draft[0].parts[0].name.as_str(), draft[0].parts[0].start, draft[0].parts[0].end), ("draft.a", 20, 24));
    }

    #[test]
    fn a_header_is_compact_metadata_first_and_padded_to_eight() {
        let group = Group {
            name: "w".into(),
            parts: vec![
                Part { source: "s".into(), start: 10, end: 26, name: "w".into(), dtype: "U32".into(), shape: vec![2, 2] },
                Part { source: "s".into(), start: 40, end: 44, name: "w_scale".into(), dtype: "U8".into(), shape: vec![2, 2] },
            ],
            meta: Some(("16", "nvfp4")),
        };
        let h = tensor_header(&group);
        let n = u64::from_le_bytes(h[..8].try_into().unwrap()) as usize;
        assert_eq!((n % 8, h.len()), (0, 8 + n));
        let text = std::str::from_utf8(&h[8..]).unwrap();
        assert_eq!(text.trim_end(), r#"{"__metadata__":{"group_size":"16","quant_type":"nvfp4"},"w":{"dtype":"U32","shape":[2,2],"data_offsets":[0,16]},"w_scale":{"dtype":"U8","shape":[2,2],"data_offsets":[16,20]}}"#);
        assert!(text.len() - text.trim_end().len() < 8);
    }

    /// The header written for a real expert bank is the one in Ollama's blob, byte for byte.
    /// Reads (never writes) the owner's Ollama store; skipped where the model is not there.
    #[test]
    fn the_header_is_byte_for_byte_ollamas() {
        let store = std::env::var("OLLAMA_MODELS").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".ollama/models"));
        let Ok(text) = std::fs::read_to_string(manifest_in(&store, crate::llm::DEFAULT_MODEL)) else { return };
        let manifest: Value = serde_json::from_str(&text).unwrap();
        let name = "model.llm.layers.0.mlp.experts.w13_weight";
        let Some(layer) = manifest["layers"].as_array().unwrap().iter().find(|l| l["name"] == name) else { return };
        let blob = store.join("blobs").join(layer["digest"].as_str().unwrap().replace(':', "-"));
        let mut file = std::fs::File::open(&blob).unwrap();
        let mut len = [0u8; 8];
        std::io::Read::read_exact(&mut file, &mut len).unwrap();
        let mut theirs = len.to_vec();
        let mut body = vec![0u8; u64::from_le_bytes(len) as usize];
        std::io::Read::read_exact(&mut file, &mut body).unwrap();
        theirs.extend(&body);
        // The checkpoint's own header for this tensor and its scale (as Hugging Face has it):
        // packed NVFP4 bytes [128, 2304, 1152] and E4M3 scales [128, 2304, 144].
        let (w, s) = (128 * 2304 * 1152u64, 128 * 2304 * 144u64);
        let h = head(json!({
            name: {"dtype": "U8", "shape": [128, 2304, 1152], "data_offsets": [1000, 1000 + w]},
            format!("{name}_scale"): {"dtype": "F8_E4M3", "shape": [128, 2304, 144], "data_offsets": [1000 + w, 1000 + w + s]},
        }));
        let groups = plan_shard("model-00001-of-00001.safetensors", &h, 8).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(String::from_utf8_lossy(&tensor_header(&groups[0])), String::from_utf8_lossy(&theirs));
        assert_eq!(tensor_header(&groups[0]).len() as u64 + groups[0].data_bytes(), layer["size"].as_u64().unwrap());
    }

    #[test]
    fn config_files_are_written_as_python_writes_them() {
        let config: Value = serde_json::from_str(r#"{"mtp_config": {"hidden_size": 2304, "rms_norm_eps": 1e-06, "rope_theta": 1000000.0, "q_bias": false, "ids": [0, 2], "kind": "rnn_hidden", "x": 0.0001, "big": 1e16, "neg": -2.5, "name": "café"}, "other": 1}"#).unwrap();
        assert_eq!(
            String::from_utf8(draft_config(&config).unwrap()).unwrap(),
            r#"{"hidden_size": 2304, "rms_norm_eps": 1e-06, "rope_theta": 1000000.0, "q_bias": false, "ids": [0, 2], "kind": "rnn_hidden", "x": 0.0001, "big": 1e+16, "neg": -2.5, "name": "caf\u00e9", "architectures": ["Inkling2ForConditionalGenerationMTP"]}"#
        );
        assert!(draft_config(&json!({})).is_err());
        // The real one is Ollama's blob, byte for byte (its digest, as Ollama's registry has it).
        let real: Value = serde_json::from_str(r#"{"mtp_config": {"hidden_size": 2304, "num_hidden_layers": 3, "num_attention_heads": 24, "num_key_value_heads": 8, "head_dim": 128, "intermediate_size": 9216, "max_position_embeddings": 1048576, "torch_dtype": "bfloat16", "q_bias": false, "o_bias": false, "rms_norm_eps": 1e-06, "rope_theta": 1000000.0, "local_layer_ids": [0, 2], "sliding_window_size": 1024, "block_size": 8, "markov_rank": 256, "markov_head_type": "rnn_hidden", "target_layer_ids": [5, 23, 41, 48]}}"#).unwrap();
        assert_eq!(hex(&sha2::Sha256::digest(draft_config(&real).unwrap())), "16d87b9bffa9334e5bd3164e415a5184e95cb07a46d3206e95978f5f48959a27");
    }

    #[test]
    fn the_token_is_found_where_huggingface_hub_keeps_it() {
        let base = std::env::temp_dir().join(format!("cwbcode-token-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join(".cache/huggingface")).unwrap();
        std::fs::create_dir_all(base.join("hf")).unwrap();
        let env = |pairs: &'static [(&'static str, &'static str)]| move |var: &str| pairs.iter().find(|(k, _)| *k == var).map(|(_, v)| v.to_string());
        // Nothing anywhere: no token (the repository is public).
        assert_eq!(hf_token(env(&[]), &base), None);
        std::fs::write(base.join(".cache/huggingface/token"), "hf_saved\n").unwrap();
        assert_eq!(hf_token(env(&[]), &base).unwrap().0, "hf_saved");
        // $HF_HOME moves the saved one; there is none there.
        let hf_home: &'static str = Box::leak(base.join("hf").display().to_string().into_boxed_str());
        let pairs: &'static [(&str, &str)] = Box::leak(vec![("HF_HOME", hf_home)].into_boxed_slice());
        assert_eq!(hf_token(env(pairs), &base), None);
        std::fs::write(base.join("hf/token"), "hf_moved").unwrap();
        assert_eq!(hf_token(env(pairs), &base).unwrap(), ("hf_moved".to_string(), format!("{hf_home}/token")));
        // The environment comes first; an empty variable is no token.
        assert_eq!(hf_token(env(&[("HF_TOKEN", " hf_env "), ("HUGGING_FACE_HUB_TOKEN", "hf_other")]), &base).unwrap(), ("hf_env".to_string(), "$HF_TOKEN".to_string()));
        assert_eq!(hf_token(env(&[("HF_TOKEN", ""), ("HUGGING_FACE_HUB_TOKEN", "hf_other")]), &base).unwrap().0, "hf_other");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_token_goes_to_the_hub_only() {
        let hub = Url::parse("https://huggingface.co").unwrap();
        for yes in ["https://huggingface.co/x/resolve/r/f", "https://huggingface.co:443/api/resolve-cache/x?y=1"] {
            assert!(token_goes_to(&hub, &Url::parse(yes).unwrap()), "{yes}");
        }
        for no in ["https://us.aws.cdn.hf.co/xet-bridge-us/abc", "https://cas-bridge.xethub.hf.co/x", "http://huggingface.co/x", "https://huggingface.co:8443/x", "https://huggingface.co.evil.example/x", "https://evil.example/huggingface.co"] {
            assert!(!token_goes_to(&hub, &Url::parse(no).unwrap()), "{no}");
        }
        // A relative redirect stays on the hub; the address of a file is under the revision.
        let h = Hub::new("https://huggingface.co", "thinkingmachines/Inkling-2-Mini", "c9d0", None).unwrap();
        let file = h.url("model.safetensors.index.json").unwrap();
        assert_eq!(file.as_str(), "https://huggingface.co/thinkingmachines/Inkling-2-Mini/resolve/c9d0/model.safetensors.index.json");
        assert!(token_goes_to(&h.base, &file.join("/api/resolve-cache/models/x").unwrap()));
        assert_eq!(source_for("ollama:inkling-2-mini:26b-a5b-nvfp4"), Some(("thinkingmachines/Inkling-2-Mini", "c9d028302cd0f035fdf8e7e91f089a4ce94d2195")));
        assert_eq!(source_for("other:latest"), None);
    }

    /// Against the real Hugging Face and Ollama's registry (a few MB; `make test-model` runs
    /// it): the plan is the registry's, file for file and size for size, and one small tensor
    /// file, written here, is the registry's byte for byte.
    #[test]
    #[ignore]
    fn the_plan_is_ollamas_registry_file_for_file() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (repo, revision) = source_for(crate::llm::DEFAULT_MODEL).unwrap();
            let hub = Hub::new("https://huggingface.co", repo, revision, None).unwrap();
            let groups = plan(&hub).await.unwrap();
            let url = "https://registry.ollama.ai/v2/library/inkling-2-mini/manifests/26b-a5b-nvfp4";
            let manifest: Value = client().unwrap().get(url).header(reqwest::header::ACCEPT, "application/vnd.docker.distribution.manifest.v2+json").send().await.unwrap().json().await.unwrap();
            let theirs: HashMap<String, (String, u64)> = manifest["layers"].as_array().unwrap().iter().filter(|l| l["mediaType"] == TENSOR).map(|l| (l["name"].as_str().unwrap().to_string(), (l["digest"].as_str().unwrap().to_string(), l["size"].as_u64().unwrap()))).collect();
            assert_eq!((groups.len(), theirs.len()), (1250, 1250));
            for g in &groups {
                let size = tensor_header(g).len() as u64 + g.data_bytes();
                assert_eq!(theirs.get(&g.name).map(|t| t.1), Some(size), "{}", g.name);
            }
            let small = groups.iter().filter(|g| g.data_bytes() > 0).min_by_key(|g| g.data_bytes()).unwrap();
            let blobs = std::env::temp_dir().join(format!("cwbcode-plan-{}", std::process::id()));
            std::fs::create_dir_all(&blobs).unwrap();
            let (digest, _) = write_group(&hub, &blobs, small, &tokio::sync::Semaphore::new(4), &Download::default()).await.unwrap();
            assert_eq!(digest, theirs[&small.name].0, "{}", small.name);
            let _ = std::fs::remove_dir_all(&blobs);
        });
    }
}

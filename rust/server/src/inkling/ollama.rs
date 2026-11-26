//! Read a model straight out of Ollama's store (or one in its layout, made from a Hugging
//! Face download by the backend itself or `scripts/model.py`): the manifest maps each tensor name and config
//! file to a blob, and every tensor blob is a small safetensors file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;

#[derive(Clone)]
pub struct Manifest {
    pub name: String,
    pub blobs: PathBuf,
    /// Tensor and config-file name -> blob path.
    pub files: HashMap<String, PathBuf>,
    /// Total size of all layers, bytes.
    pub size: u64,
}

/// The store a Hugging Face download is written into (by the backend itself, see
/// download.rs, or by `scripts/model.py`), in Ollama's layout: $CWBCODE_MODELS, or
/// ~/.cwbcode/models.
pub fn own_store() -> PathBuf {
    std::env::var("CWBCODE_MODELS").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cwbcode/models"))
}

/// The stores a model is looked for in, in order: Ollama's own (`ollama pull`), then cwbcode's.
fn stores() -> [PathBuf; 2] {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let ollama = std::env::var("OLLAMA_MODELS").map(PathBuf::from).unwrap_or_else(|_| home.join(".ollama/models"));
    [ollama, own_store()]
}

/// Is `spec` an Ollama model name ("inkling-2-mini:26b-a5b-nvfp4", optionally "ollama:...")
/// in one of the stores? Its manifest, and the store it is in.
pub fn manifest_path(spec: &str) -> Option<(PathBuf, PathBuf)> {
    let spec = spec.strip_prefix("ollama:").unwrap_or(spec);
    if spec.contains('/') && !spec.starts_with("registry.") || spec.is_empty() {
        return None;
    }
    let (name, tag) = spec.split_once(':').unwrap_or((spec, "latest"));
    stores().into_iter().find_map(|store| {
        let p = store.join("manifests/registry.ollama.ai/library").join(name).join(tag);
        p.is_file().then_some((p, store))
    })
}

/// MLX keeps every tensor file open until its array is evaluated, and this model is hundreds of
/// files, while a terminal's default limit is 256 open files: raise the limit to fit them.
fn allow_open_files(count: usize) {
    let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: getrlimit and setrlimit only read and write the struct passed to them.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) != 0 {
            return;
        }
        let wanted = (count as libc::rlim_t + 256).min(limit.rlim_max);
        if limit.rlim_cur < wanted {
            limit.rlim_cur = wanted;
            libc::setrlimit(libc::RLIMIT_NOFILE, &limit);
        }
    }
}

impl Manifest {
    pub fn load(spec: &str) -> Result<Self> {
        let (path, store) = manifest_path(spec).with_context(|| format!("no model {spec} in Ollama's store or ~/.cwbcode/models (run `make download`)"))?;
        let root: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
        let blobs = store.join("blobs");
        let mut files = HashMap::new();
        let mut size = 0;
        for layer in root["layers"].as_array().context("manifest has no layers")? {
            size += layer["size"].as_u64().unwrap_or(0);
            if let (Some(name), Some(digest)) = (layer["name"].as_str(), layer["digest"].as_str()) {
                files.insert(name.to_string(), blobs.join(digest.replace(':', "-")));
            }
        }
        if files.is_empty() {
            bail!("{spec} is not a safetensors (MLX) model in Ollama's store");
        }
        allow_open_files(files.len());
        Ok(Self { name: spec.strip_prefix("ollama:").unwrap_or(spec).to_string(), blobs, files, size })
    }

    pub fn file(&self, name: &str) -> Result<&Path> {
        self.files.get(name).map(|p| p.as_path()).with_context(|| format!("{} has no {name}", self.name))
    }

    /// A path to a tensor blob that MLX will open: it picks the loader by file extension, so
    /// link the blob under a `.safetensors` name in a temporary folder.
    pub fn safetensors(&self, blob: &Path) -> Result<PathBuf> {
        let dir = std::env::temp_dir().join("ollama-safetensors");
        std::fs::create_dir_all(&dir)?;
        let name = blob.file_name().context("blob path has no file name")?;
        let link = dir.join(name).with_extension("safetensors");
        if std::fs::read_link(&link).ok().as_deref() != Some(blob) {
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(blob, &link)?;
        }
        Ok(link)
    }

    pub fn json(&self, name: &str) -> Result<Value> {
        Ok(serde_json::from_str(&std::fs::read_to_string(self.file(name)?)?)?)
    }

    /// The store the model was found in (the folder holding `blobs/` and `manifests/`).
    pub fn store(&self) -> PathBuf {
        self.blobs.parent().map(Path::to_path_buf).unwrap_or_else(|| self.blobs.clone())
    }

    /// What the model is, in a line: architecture, layers, experts, how the experts are
    /// quantized, and whether it reads images and has a draft network. From its config and the
    /// expert tensors' own headers (a few hundred bytes each; nothing is loaded).
    pub fn about(&self) -> String {
        let mut parts = Vec::new();
        if let Ok(cfg) = self.json("config.json") {
            let text = &cfg["text_config"];
            if let Some(arch) = cfg["architectures"][0].as_str() {
                parts.push(arch.to_string());
            }
            if let Some(layers) = text["num_hidden_layers"].as_u64() {
                parts.push(format!("{layers} layers"));
            }
            if let (Some(experts), Some(active)) = (text["n_routed_experts"].as_u64(), text["num_experts_per_tok"].as_u64()) {
                parts.push(format!("{experts} experts, {active} per token"));
            }
        }
        let mut quant: Vec<String> = self
            .files
            .iter()
            .filter(|(name, _)| name.ends_with("experts.w13_weight"))
            .filter_map(|(_, path)| {
                use std::io::Read;
                let mut file = std::fs::File::open(path).ok()?;
                let mut len = [0u8; 8];
                file.read_exact(&mut len).ok()?;
                let mut head = vec![0u8; (u64::from_le_bytes(len) as usize).min(1 << 16)];
                file.read_exact(&mut head).ok()?;
                let head: Value = serde_json::from_slice(&head).ok()?;
                head["__metadata__"]["quant_type"].as_str().map(str::to_uppercase)
            })
            .collect();
        quant.sort();
        quant.dedup();
        if !quant.is_empty() {
            parts.push(format!("experts in {}", quant.join(" + ")));
        }
        if self.files.keys().any(|name| name.starts_with("model.visual.")) {
            parts.push("vision".to_string());
        }
        if self.files.contains_key("draft/config.json") {
            parts.push("draft network".to_string());
        }
        parts.join(" · ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads only the model's config and a few tensor headers; skipped where it is not installed.
    #[test]
    fn the_model_is_described_without_loading_it() {
        let Ok(manifest) = Manifest::load(crate::llm::DEFAULT_MODEL) else { return };
        let about = manifest.about();
        assert!(about.starts_with("Inkling2LoopForConditionalGeneration · "), "{about}");
        assert!(about.contains("experts, ") && about.contains("experts in ") && about.contains("draft network"), "{about}");
        assert!(manifest.store().join("blobs").is_dir());
        assert!(manifest.size > 1 << 30);
    }
}

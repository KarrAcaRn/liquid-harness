//! Where llama.cpp keeps downloaded GGUF files: shared by the llama.cpp
//! backend (memory estimates) and the core's self-description (`lh doctor`).
//! Pure `std::fs` lookups; nothing is downloaded or opened beyond metadata.
use std::path::{Path, PathBuf};

/// A file on disk with its size in bytes.
pub type SizedFile = (PathBuf, u64);

/// `~/` expansion only (no variables), as for `file:` sources and `models_dir`.
pub fn expand(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{home}/{rest}");
    }
    path.into()
}

/// The download cache llama.cpp uses: configured `models_dir`, else
/// `$LLAMA_CACHE`, else `~/.cache/llama.cpp`.
pub fn cache_dir(models_dir: Option<&str>) -> Option<PathBuf> {
    cache_dir_from(models_dir, std::env::var_os("LLAMA_CACHE"))
}
/// [`cache_dir`] with an explicit `LLAMA_CACHE` value (tests).
pub fn cache_dir_from(
    models_dir: Option<&str>,
    llama_cache: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    if let Some(dir) = models_dir {
        return Some(PathBuf::from(expand(dir)));
    }
    if let Some(dir) = llama_cache.filter(|d| !d.is_empty()) {
        return Some(dir.into());
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache/llama.cpp"))
}

fn ggufs(dir: &Path, depth: u32, out: &mut Vec<SizedFile>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // `metadata` follows the snapshot symlinks of the Hugging Face cache layout.
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() && depth > 0 {
            ggufs(&path, depth - 1, out);
        } else if meta.is_file() && path.extension().is_some_and(|e| e == "gguf") {
            out.push((path, meta.len()));
        }
    }
}

/// Downloaded files for `owner/repo[:quant]` (the part after `hf:`):
/// (model, largest matching projector). Both the Hugging Face hub layout
/// (`models--owner--repo/snapshots/*/file.gguf`) and llama.cpp's flat
/// `owner_repo_file.gguf` names are recognised.
pub fn resolve_hf(source: &str, cache: &Path) -> Option<(SizedFile, Option<SizedFile>)> {
    let (repo, quant) = source.split_once(':').unwrap_or((source, ""));
    let (owner, name) = repo.split_once('/')?;
    let (owner, name, quant) = (
        owner.to_lowercase(),
        name.to_lowercase(),
        quant.to_lowercase(),
    );
    let stem = name.strip_suffix("-gguf").unwrap_or(&name).to_string();
    let hub = format!("models--{owner}--{name}");
    let flat = format!("{owner}_{name}_");
    let mut files = Vec::new();
    ggufs(cache, 4, &mut files);
    files.sort();
    let mut best: Option<(u8, SizedFile)> = None;
    let mut projector: Option<SizedFile> = None;
    for (path, size) in files {
        let full = path.to_string_lossy().to_lowercase();
        let file = path
            .file_name()
            .map(|f| f.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let rank = if full.contains(&format!("/{hub}/")) {
            3
        } else if file.starts_with(&flat) {
            2
        } else if file.contains(&stem) {
            1
        } else {
            continue;
        };
        if file.contains("mmproj") {
            if projector.as_ref().is_none_or(|(_, s)| size > *s) {
                projector = Some((path, size));
            }
        } else if (quant.is_empty() || file.contains(&quant))
            && best.as_ref().is_none_or(|(r, _)| rank > *r)
        {
            best = Some((rank, (path, size)));
        }
    }
    best.map(|(_, model)| (model, projector))
}

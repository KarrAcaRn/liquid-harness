//! `backend/estimate`: a rough resident-memory estimate for a model before it
//! is loaded, from the GGUF files on disk (weights + projector + KV cache +
//! a flat compute allowance). Sources not yet downloaded have no estimate.
use lh_protocol::{
    ModelSpec,
    json::{Value, json},
};
use std::{
    fs::File,
    io::{self, BufReader, Read},
    path::{Path, PathBuf},
};

/// Compute buffers, scratch and runtime overhead are not modelled exactly.
const COMPUTE_ALLOWANCE: u64 = 256 << 20;

#[derive(Debug, Default, PartialEq)]
pub struct Shape {
    pub layers: u64,
    /// Sum over attention layers of kv_heads * (key_length + value_length).
    pub kv_width: u64,
    pub sliding_window: Option<u64>,
    /// Attention layers using the sliding window, counted in `kv_width` at full context otherwise.
    pub swa_width: u64,
}

enum Meta {
    Uint(u64),
    Bools(Vec<bool>),
    Uints(Vec<u64>),
    Text(String),
    Other,
}
fn read_u32(r: &mut impl Read) -> io::Result<u32> {
    let mut b = [0; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}
fn read_u64(r: &mut impl Read) -> io::Result<u64> {
    let mut b = [0; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}
fn skip(r: &mut impl Read, n: u64) -> io::Result<()> {
    let copied = io::copy(&mut r.take(n), &mut io::sink())?;
    if copied == n {
        Ok(())
    } else {
        Err(io::ErrorKind::UnexpectedEof.into())
    }
}
fn read_string(r: &mut impl Read, keep: bool) -> io::Result<Option<String>> {
    let n = read_u64(r)?;
    if !keep || n > 1 << 16 {
        skip(r, n)?;
        return Ok(None);
    }
    let mut b = vec![0; n as usize];
    r.read_exact(&mut b)?;
    Ok(Some(String::from_utf8_lossy(&b).into_owned()))
}
fn scalar(r: &mut impl Read, kind: u32) -> io::Result<Option<u64>> {
    let size = match kind {
        0 | 1 | 7 => 1,
        2 | 3 => 2,
        4..=6 => 4,
        10..=12 => 8,
        _ => return Err(io::Error::other("unknown GGUF type")),
    };
    let mut b = [0; 8];
    r.read_exact(&mut b[..size])?;
    let v = u64::from_le_bytes(b);
    Ok(match kind {
        0 | 2 | 4 | 10 | 7 => Some(v),
        1 if (v as i8) >= 0 => Some(v),
        3 if (v as i16) >= 0 => Some(v),
        5 if (v as i32) >= 0 => Some(v),
        11 if (v as i64) >= 0 => Some(v),
        _ => None,
    })
}
fn value(r: &mut impl Read, kind: u32, keep: bool) -> io::Result<Meta> {
    match kind {
        8 => Ok(read_string(r, keep)?.map_or(Meta::Other, Meta::Text)),
        9 => {
            let inner = read_u32(r)?;
            let n = read_u64(r)?;
            let numeric = matches!(inner, 0..=5 | 7 | 10 | 11) && keep && n <= 4096;
            let mut uints = Vec::new();
            for _ in 0..n {
                if inner == 8 {
                    read_string(r, false)?;
                } else if inner == 9 {
                    return Err(io::Error::other("nested GGUF arrays are not supported"));
                } else {
                    let v = scalar(r, inner)?;
                    if numeric {
                        uints.push(v.unwrap_or(0));
                    }
                }
            }
            Ok(match (numeric, inner) {
                (true, 7) => Meta::Bools(uints.into_iter().map(|v| v != 0).collect()),
                (true, _) => Meta::Uints(uints),
                _ => Meta::Other,
            })
        }
        6 | 12 => {
            skip(r, if kind == 6 { 4 } else { 8 })?;
            Ok(Meta::Other)
        }
        _ => Ok(scalar(r, kind)?.map_or(Meta::Other, Meta::Uint)),
    }
}

/// Attention shape from GGUF metadata; `None` for files without one (projectors).
pub fn shape(path: &Path) -> io::Result<Option<Shape>> {
    let mut r = BufReader::new(File::open(path)?);
    let mut magic = [0; 4];
    r.read_exact(&mut magic)?;
    if &magic != b"GGUF" {
        return Err(io::Error::other("not a GGUF file"));
    }
    if read_u32(&mut r)? < 2 {
        return Err(io::Error::other("unsupported GGUF version"));
    }
    let _tensors = read_u64(&mut r)?;
    let count = read_u64(&mut r)?;
    let mut arch = None;
    let mut entries = Vec::new();
    for _ in 0..count {
        let key = read_string(&mut r, true)?.unwrap_or_default();
        let kind = read_u32(&mut r)?;
        let keep = key == "general.architecture"
            || key.ends_with(".block_count")
            || key.ends_with(".embedding_length")
            || key.contains(".attention.");
        let v = value(&mut r, kind, keep)?;
        if key == "general.architecture"
            && let Meta::Text(a) = &v
        {
            arch = Some(a.clone());
        }
        if keep {
            entries.push((key, v));
        }
    }
    let Some(arch) = arch else { return Ok(None) };
    let get = |name: &str| {
        let full = format!("{arch}.{name}");
        entries.iter().find(|(k, _)| *k == full).map(|(_, v)| v)
    };
    let uint = |name: &str| match get(name) {
        Some(Meta::Uint(v)) => Some(*v),
        _ => None,
    };
    let (Some(layers), Some(heads)) = (uint("block_count"), uint("attention.head_count")) else {
        return Ok(None);
    };
    let head_dim = uint("embedding_length").unwrap_or(0) / heads.max(1);
    let key = uint("attention.key_length").unwrap_or(head_dim);
    let val = uint("attention.value_length").unwrap_or(head_dim);
    let kv_heads: Vec<u64> = match get("attention.head_count_kv") {
        Some(Meta::Uints(v)) => v.clone(),
        Some(Meta::Uint(v)) => vec![*v; layers as usize],
        _ => vec![heads; layers as usize],
    };
    let swa = match get("attention.sliding_window_pattern") {
        Some(Meta::Bools(v)) => v.clone(),
        _ => vec![],
    };
    let window = uint("attention.sliding_window");
    let mut result = Shape {
        layers,
        sliding_window: window,
        ..Default::default()
    };
    for (i, heads) in kv_heads.iter().enumerate() {
        let width = heads * (key + val);
        if window.is_some() && swa.get(i) == Some(&true) {
            result.swa_width += width;
        } else {
            result.kv_width += width;
        }
    }
    Ok(Some(result))
}

fn cache_dir(models_dir: Option<&str>) -> Option<PathBuf> {
    if let Some(dir) = models_dir {
        return Some(PathBuf::from(super::expand(dir)));
    }
    if let Some(dir) = std::env::var_os("LLAMA_CACHE") {
        return Some(dir.into());
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache/llama.cpp"))
}
fn ggufs(dir: &Path, depth: u32, out: &mut Vec<(PathBuf, u64)>) {
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
/// A file on disk with its size in bytes.
type Sized = (PathBuf, u64);
/// Downloaded files for `hf:owner/repo[:quant]`: (model, largest matching projector).
pub fn resolve_hf(source: &str, cache: &Path) -> Option<(Sized, Option<Sized>)> {
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
    let mut best: Option<(u8, (PathBuf, u64))> = None;
    let mut projector: Option<(PathBuf, u64)> = None;
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

pub fn estimate(model: &ModelSpec, models_dir: Option<&str>) -> Value {
    let ctx = u64::from(model.ctx.unwrap_or(4096));
    let batch = u64::from(model.batch.unwrap_or(512));
    let unknown = |why: &str| json!({"ram_bytes":null,"basis":"unknown","reason":why});
    let (weights, projector) = if let Some(path) = model.source.strip_prefix("file:") {
        let path = PathBuf::from(super::expand(path));
        let Ok(meta) = std::fs::metadata(&path) else {
            return unknown("model file not found");
        };
        let projector = model
            .mmproj
            .as_deref()
            .filter(|m| *m != "auto")
            .map(|m| PathBuf::from(super::expand(m.strip_prefix("file:").unwrap_or(m))))
            .and_then(|p| std::fs::metadata(&p).ok().map(|m| (p, m.len())));
        ((path, meta.len()), projector)
    } else if let Some(source) = model.source.strip_prefix("hf:") {
        let Some(cache) = cache_dir(models_dir) else {
            return unknown("no llama.cpp cache directory");
        };
        match resolve_hf(source, &cache) {
            Some((weights, projector)) => (weights, projector.filter(|_| model.mmproj.is_some())),
            None => return unknown("not downloaded yet"),
        }
    } else {
        return unknown("unsupported source");
    };
    let shape = shape(&weights.0).ok().flatten().unwrap_or_default();
    let swa_tokens = shape.sliding_window.map_or(ctx, |w| ctx.min(w + batch));
    // f16 K and V caches.
    let kv = 2 * (shape.kv_width * ctx + shape.swa_width * swa_tokens);
    let projector_bytes = projector.as_ref().map_or(0, |(_, s)| *s);
    json!({
        "ram_bytes": weights.1 + projector_bytes + kv + COMPUTE_ALLOWANCE,
        "basis": "estimate",
        "weights_bytes": weights.1,
        "projector_bytes": projector_bytes,
        "kv_cache_bytes": kv,
        "compute_allowance_bytes": COMPUTE_ALLOWANCE,
        "ctx": ctx,
        "file": weights.0.file_name().map(|f| f.to_string_lossy().into_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn gguf(path: &Path, extra: &[u8]) {
        let mut b = b"GGUF".to_vec();
        b.extend(3u32.to_le_bytes());
        b.extend(0u64.to_le_bytes());
        let kv: Vec<(&str, Vec<u8>)> = vec![
            ("general.architecture", {
                let mut v = 8u32.to_le_bytes().to_vec();
                v.extend(4u64.to_le_bytes());
                v.extend(b"lfm3");
                v
            }),
            (
                "lfm3.block_count",
                [4u32.to_le_bytes(), 4u32.to_le_bytes()].concat(),
            ),
            (
                "lfm3.embedding_length",
                [4u32.to_le_bytes(), 64u32.to_le_bytes()].concat(),
            ),
            (
                "lfm3.attention.head_count",
                [4u32.to_le_bytes(), 4u32.to_le_bytes()].concat(),
            ),
            ("lfm3.attention.head_count_kv", {
                let mut v = 9u32.to_le_bytes().to_vec();
                v.extend(4u32.to_le_bytes());
                v.extend(4u64.to_le_bytes());
                for h in [0u32, 2, 0, 2] {
                    v.extend(h.to_le_bytes());
                }
                v
            }),
            ("lfm3.attention.sliding_window_pattern", {
                let mut v = 9u32.to_le_bytes().to_vec();
                v.extend(7u32.to_le_bytes());
                v.extend(4u64.to_le_bytes());
                v.extend([0u8, 0, 0, 1]);
                v
            }),
            (
                "lfm3.attention.sliding_window",
                [4u32.to_le_bytes(), 8u32.to_le_bytes()].concat(),
            ),
            ("tokenizer.ggml.tokens", {
                let mut v = 9u32.to_le_bytes().to_vec();
                v.extend(8u32.to_le_bytes());
                v.extend(1u64.to_le_bytes());
                v.extend(1u64.to_le_bytes());
                v.extend(b"a");
                v
            }),
            (
                "general.score",
                [6u32.to_le_bytes(), 1.5f32.to_le_bytes()].concat(),
            ),
        ];
        b.extend((kv.len() as u64).to_le_bytes());
        for (k, v) in kv {
            b.extend((k.len() as u64).to_le_bytes());
            b.extend(k.as_bytes());
            b.extend(v);
        }
        b.extend(extra);
        std::fs::write(path, b).unwrap();
    }
    #[test]
    fn gguf_shape_cache_layouts_and_estimate() {
        let dir = std::env::temp_dir().join(format!("lh-estimate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let snap = dir.join("models--LiquidAI--Tiny-GGUF/snapshots/abc");
        std::fs::create_dir_all(&snap).unwrap();
        gguf(&snap.join("Tiny-Q4_0.gguf"), &[0; 1000]);
        gguf(&snap.join("Tiny-Q8_0.gguf"), &[0; 2000]);
        std::fs::write(snap.join("mmproj-Tiny-F16.gguf"), [0; 300]).unwrap();
        gguf(&dir.join("LiquidAI_Other-GGUF_Other-Q4_0.gguf"), &[]);
        // Head dim 16, two full-attention layers' worth: one full, one sliding.
        let s = shape(&snap.join("Tiny-Q4_0.gguf")).unwrap().unwrap();
        assert_eq!(
            s,
            Shape {
                layers: 4,
                kv_width: 2 * 32,
                sliding_window: Some(8),
                swa_width: 2 * 32
            }
        );
        let ((path, _), projector) = resolve_hf("LiquidAI/Tiny-GGUF:Q8_0", &dir).unwrap();
        assert!(path.ends_with("Tiny-Q8_0.gguf"));
        assert!(projector.unwrap().0.ends_with("mmproj-Tiny-F16.gguf"));
        let ((path, _), _) = resolve_hf("LiquidAI/Other-GGUF:Q4_0", &dir).unwrap();
        assert!(path.ends_with("LiquidAI_Other-GGUF_Other-Q4_0.gguf"));
        assert!(resolve_hf("LiquidAI/Missing-GGUF:Q4_0", &dir).is_none());
        let model = ModelSpec {
            source: "hf:LiquidAI/Tiny-GGUF:Q4_0".into(),
            ctx: Some(100),
            mmproj: Some("auto".into()),
            batch: Some(4),
            threads: None,
        };
        let e = estimate(&model, Some(dir.to_str().unwrap()));
        let weights = e["weights_bytes"].as_u64().unwrap();
        // K+V f16: full layer 64*100*2, sliding layer 64*min(100, 8+4)*2.
        assert_eq!(e["kv_cache_bytes"], 2 * (64 * 100 + 64 * 12));
        assert_eq!(e["projector_bytes"], 300);
        assert_eq!(
            e["ram_bytes"].as_u64().unwrap(),
            weights + 300 + 2 * (64 * 100 + 64 * 12) + COMPUTE_ALLOWANCE
        );
        let missing = estimate(
            &ModelSpec {
                source: "hf:LiquidAI/Missing-GGUF:Q4_0".into(),
                ..model.clone()
            },
            Some(dir.to_str().unwrap()),
        );
        assert!(missing["ram_bytes"].is_null());
        let file = estimate(
            &ModelSpec {
                source: format!("file:{}", snap.join("Tiny-Q4_0.gguf").display()),
                mmproj: None,
                ..model
            },
            None,
        );
        assert_eq!(file["weights_bytes"].as_u64().unwrap(), weights);
        assert_eq!(file["projector_bytes"], 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

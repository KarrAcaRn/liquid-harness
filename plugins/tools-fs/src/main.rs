use lh_plugin_sdk::mcp::{ToolServer, serve, tool};
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};

const FILE_CAP: usize = 8 * 1024 * 1024;
const OUTPUT_CAP: usize = 64 * 1024;
const VISIT_CAP: usize = 10_000;

#[derive(Default)]
struct Server {
    root: Option<PathBuf>,
    allow_secrets: bool,
}

fn secret(name: &str) -> bool {
    name == ".env" || name.starts_with(".env.") || name.ends_with(".pem") || name.starts_with("id_")
}

fn text<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{key} must be a string"))
}

fn number(args: &Value, key: &str, default: usize, max: usize) -> Result<usize, String> {
    match args.get(key) {
        None => Ok(default),
        Some(v) => v
            .as_u64()
            .filter(|n| *n > 0 && *n <= max as u64)
            .map(|n| n as usize)
            .ok_or_else(|| format!("{key} must be an integer in 1..={max}")),
    }
}

fn read(path: &Path) -> Result<String, String> {
    if !fs::metadata(path).map_err(|e| e.to_string())?.is_file() {
        return Err("not a regular file".into());
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take((FILE_CAP + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > FILE_CAP {
        return Err("file exceeds 8 MiB limit".into());
    }
    String::from_utf8(bytes).map_err(|_| "file is not UTF-8 text".into())
}

// Bound output by UTF-8 bytes, always preserving space for a visible cap marker.
fn append(out: &mut String, line: &str) -> bool {
    if out.len() + line.len() > OUTPUT_CAP - 64 {
        out.push_str("[truncated: output byte limit]\n");
        false
    } else {
        out.push_str(line);
        true
    }
}

impl Server {
    fn root(&self) -> Result<PathBuf, String> {
        self.root.clone().map(Ok).unwrap_or_else(|| {
            std::env::current_dir()
                .and_then(fs::canonicalize)
                .map_err(|e| e.to_string())
        })
    }

    fn resolve(&self, input: &str) -> Result<PathBuf, String> {
        if input.is_empty() {
            return Err("path must not be empty".into());
        }
        let mut path = self.root()?;
        // Reject rather than normalize parent traversal; check every existing component.
        for component in Path::new(input).components() {
            match component {
                Component::CurDir => (),
                Component::Normal(name) => {
                    if !self.allow_secrets && secret(&name.to_string_lossy()) {
                        return Err("secret path denied (allow_secrets is false)".into());
                    }
                    path.push(name);
                    match fs::symlink_metadata(&path) {
                        Ok(meta) if meta.file_type().is_symlink() => {
                            return Err("symlinks are not allowed".into());
                        }
                        Ok(_) => (),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                        Err(e) => return Err(e.to_string()),
                    }
                }
                _ => return Err("path must be relative without parent components".into()),
            }
        }
        // Recheck the configured root too (it may have been replaced).
        if fs::symlink_metadata(self.root()?)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("workspace root became a symlink".into());
        }
        Ok(path)
    }

    fn read_file(&self, args: &Value) -> Result<String, String> {
        let path = self.resolve(text(args, "path")?)?;
        let offset = number(args, "offset", 1, usize::MAX)?;
        let limit = number(args, "limit", 200, 2000)?;
        let content = read(&path)?;
        let mut out = String::new();
        let mut lines = content.lines().enumerate().skip(offset - 1);
        for (index, line) in lines.by_ref().take(limit) {
            if !append(&mut out, &format!("{}|{line}\n", index + 1)) {
                return Ok(out);
            }
        }
        if lines.next().is_some() {
            append(
                &mut out,
                "[truncated: line limit; increase offset to continue]\n",
            );
        }
        Ok(out)
    }

    fn write_file(&self, args: &Value) -> Result<String, String> {
        let path = self.resolve(text(args, "path")?)?;
        let content = text(args, "content")?;
        if content.len() > FILE_CAP {
            return Err("content exceeds 8 MiB limit".into());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        self.resolve(text(args, "path")?)?;
        if path.exists() && !fs::metadata(&path).map_err(|e| e.to_string())?.is_file() {
            return Err("not a regular file".into());
        }
        fs::write(path, content).map_err(|e| e.to_string())?;
        Ok(format!("Wrote {} bytes", content.len()))
    }

    fn edit_file(&self, args: &Value) -> Result<String, String> {
        let path = self.resolve(text(args, "path")?)?;
        let old = text(args, "old_string")?;
        let new = text(args, "new_string")?;
        if old.is_empty() || old == new {
            return Err("old_string must be nonempty and differ from new_string".into());
        }
        let content = read(&path)?;
        // Include overlapping matches when determining uniqueness.
        let mut matches = content
            .char_indices()
            .filter_map(|(i, _)| content[i..].starts_with(old).then_some(i));
        let index = matches.next().ok_or("old_string was not found")?;
        if matches.next().is_some() {
            return Err("old_string is not unique".into());
        }
        let result_len = content.len() - old.len() + new.len();
        if result_len > FILE_CAP {
            return Err("edited file exceeds 8 MiB limit".into());
        }
        let result = format!(
            "{}{}{}",
            &content[..index],
            new,
            &content[index + old.len()..]
        );
        self.resolve(text(args, "path")?)?;
        fs::write(path, result).map_err(|e| e.to_string())?;
        let line = content[..index].bytes().filter(|b| *b == b'\n').count() + 1;
        let mut out = format!(
            "Replaced one occurrence at line {line}\n--- before (excerpt)\n+++ after (excerpt)\n"
        );
        for (prefix, value) in [("-", old), ("+", new)] {
            for fragment in value.lines().take(10) {
                let excerpt: String = fragment.chars().take(200).collect();
                out.push_str(&format!("{prefix}{excerpt}\n"));
            }
        }
        Ok(out)
    }

    fn list_dir(&self, args: &Value) -> Result<String, String> {
        let path = self.resolve(
            args.get("path")
                .map(|_| text(args, "path"))
                .transpose()?
                .unwrap_or("."),
        )?;
        let limit = number(args, "limit", 200, 1000)?;
        let root = self.root()?;
        let mut entries = Vec::new();
        let mut capped = false;
        for (i, entry) in fs::read_dir(path).map_err(|e| e.to_string())?.enumerate() {
            if i >= VISIT_CAP {
                capped = true;
                break;
            }
            let entry = entry.map_err(|e| e.to_string())?;
            let relative = entry
                .path()
                .strip_prefix(&root)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .into_owned();
            if self.resolve(&relative).is_err() {
                continue;
            }
            let suffix = if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                "/"
            } else {
                ""
            };
            entries.push(format!("{relative}{suffix}\n"));
        }
        entries.sort();
        let mut out = String::new();
        for entry in entries.iter().take(limit) {
            if !append(&mut out, entry) {
                return Ok(out);
            }
        }
        if capped || entries.len() > limit {
            append(&mut out, "[truncated: entry limit]\n");
        }
        Ok(out)
    }

    fn search_files(&self, args: &Value) -> Result<String, String> {
        let root = self.root()?;
        let path = self.resolve(
            args.get("path")
                .map(|_| text(args, "path"))
                .transpose()?
                .unwrap_or("."),
        )?;
        let pattern = text(args, "pattern")?;
        let target = args
            .get("target")
            .map(|_| text(args, "target"))
            .transpose()?
            .unwrap_or("content");
        if !["content", "name", "files"].contains(&target) {
            return Err("target must be content, name or files".into());
        }
        let limit = number(args, "limit", 200, 1000)?;
        let regex = if target == "content" {
            Some(regex::Regex::new(pattern).map_err(|e| e.to_string())?)
        } else {
            None
        };
        let glob = if regex.is_none() {
            Some(
                globset::Glob::new(pattern)
                    .map_err(|e| e.to_string())?
                    .compile_matcher(),
            )
        } else {
            None
        };
        let allow = self.allow_secrets;
        let mut builder = ignore::WalkBuilder::new(path);
        builder
            .follow_links(false)
            .hidden(false)
            .require_git(false)
            .parents(false)
            .git_global(false)
            .sort_by_file_path(|a, b| a.cmp(b))
            .filter_entry(move |entry| {
                let name = entry.file_name().to_string_lossy();
                !entry.file_type().is_some_and(|t| t.is_symlink())
                    && ![".git", "target", "node_modules"].contains(&name.as_ref())
                    && (allow || !secret(&name))
            });
        let mut out = String::new();
        let mut count = 0;
        for (visited, entry) in builder.build().enumerate() {
            if visited >= VISIT_CAP {
                append(&mut out, "[truncated: traversal limit]\n");
                break;
            }
            let entry = entry.map_err(|e| e.to_string())?;
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(&root)
                .map_err(|e| e.to_string())?
                .to_string_lossy();
            let checked = self.resolve(&relative)?;
            if let Some(glob) = &glob {
                if glob.is_match(relative.as_ref()) || glob.is_match(entry.file_name()) {
                    if count == limit {
                        append(&mut out, "[truncated: match limit]\n");
                        break;
                    }
                    if !append(&mut out, &format!("{relative}\n")) {
                        break;
                    }
                    count += 1;
                }
            } else if let Some(regex) = &regex {
                // Binary, oversized and unreadable files are omitted from content search.
                let Ok(content) = read(&checked) else {
                    continue;
                };
                if content.contains('\0') {
                    continue;
                }
                for (i, line) in content.lines().enumerate() {
                    if regex.is_match(line) {
                        if count == limit {
                            append(&mut out, "[truncated: match limit]\n");
                            return Ok(out);
                        }
                        if !append(&mut out, &format!("{relative}:{}:{line}\n", i + 1)) {
                            return Ok(out);
                        }
                        count += 1;
                    }
                }
            }
        }
        Ok(out)
    }
}

impl ToolServer for Server {
    fn name(&self) -> &str {
        "tools-fs"
    }
    fn configure(&mut self, config: Value) -> Result<(), String> {
        if !config.is_object() {
            return Err("config must be an object".into());
        }
        let root = config
            .get("workspace_root")
            .map(|_| text(&config, "workspace_root"))
            .transpose()?
            .unwrap_or(".");
        let root = fs::canonicalize(root).map_err(|e| e.to_string())?;
        if !root.is_dir() {
            return Err("workspace_root must be a directory".into());
        }
        let allow = config
            .get("allow_secrets")
            .map(|v| v.as_bool().ok_or("allow_secrets must be boolean"))
            .transpose()?
            .unwrap_or(false);
        self.root = Some(root);
        self.allow_secrets = allow;
        Ok(())
    }
    fn tools(&self) -> Value {
        json!([
            tool(
                "read_file",
                "Read UTF-8 text with 1-based line numbers (8 MiB file, 64 KiB output caps).",
                json!({"path":{"type":"string"},"offset":{"type":"integer","minimum":1},"limit":{"type":"integer","minimum":1,"maximum":2000}}),
                &["path"],
                false
            ),
            tool(
                "write_file",
                "Overwrite a UTF-8 file, creating parents. Workspace-relative paths only.",
                json!({"path":{"type":"string"},"content":{"type":"string"}}),
                &["path", "content"],
                true
            ),
            tool(
                "edit_file",
                "Replace a unique exact nonempty string; return a small diff excerpt.",
                json!({"path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"}}),
                &["path", "old_string", "new_string"],
                true
            ),
            tool(
                "list_dir",
                "List immediate children, directories suffixed with /. Secrets and symlinks omitted.",
                json!({"path":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":1000}}),
                &[],
                false
            ),
            tool(
                "search_files",
                "Search names by glob or UTF-8 content by regex. Honors .gitignore; skips .git, target, node_modules, symlinks and secrets. Content search omits unreadable/binary/oversized files. Bounded to 10,000 visits and 64 KiB output.",
                json!({"path":{"type":"string"},"pattern":{"type":"string"},"target":{"type":"string","enum":["content","name","files"]},"limit":{"type":"integer","minimum":1,"maximum":1000}}),
                &["pattern"],
                false
            )
        ])
    }
    fn call(&mut self, name: &str, args: Value) -> Result<String, String> {
        match name {
            "read_file" => self.read_file(&args),
            "write_file" => self.write_file(&args),
            "edit_file" => self.edit_file(&args),
            "list_dir" => self.list_dir(&args),
            "search_files" => self.search_files(&args),
            _ => Err(format!("unknown tool: {name}")),
        }
    }
}

fn main() -> std::io::Result<()> {
    serve(Server::default())
}

#[cfg(test)]
#[path = "../tests/filesystem.rs"]
mod tests;

//! Deterministic discovery of local Markdown skills (see `docs/skills.md`).
use crate::{Error, Result};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// Markdown body, excluding the YAML frontmatter.
    pub content: String,
    /// Suggested tools, not permissions or an allowlist.
    pub tools: Vec<String>,
}

pub fn search_paths(config_dir: &Path, plugin_dirs: &[PathBuf]) -> Vec<PathBuf> {
    paths_with_env(config_dir, plugin_dirs, std::env::var_os("LH_SKILL_PATH"))
}

fn paths_with_env(
    config_dir: &Path,
    plugin_dirs: &[PathBuf],
    env: Option<std::ffi::OsString>,
) -> Vec<PathBuf> {
    let mut paths: Vec<_> = env
        .map(|value| {
            std::env::split_paths(&value)
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        })
        .unwrap_or_default();
    paths.push(config_dir.join("skills"));
    paths.push(PathBuf::from("./skills"));
    paths.extend(plugin_dirs.iter().map(|p| p.join("skills")));
    paths
}

/// Entries name skill directories or their immediate parent directories.
/// Missing entries are ignored; aliases of the same canonical file are read once.
/// Distinct files with the same name are errors, never precedence overrides.
pub fn discover(paths: &[PathBuf]) -> Result<BTreeMap<String, Skill>> {
    let mut files = BTreeSet::new();
    for path in paths {
        match std::fs::metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            result => {
                result?;
            }
        }
        let direct = path.join("SKILL.md");
        if direct.try_exists()? {
            files.insert(direct.canonicalize()?);
        } else {
            let mut children = std::fs::read_dir(path)?
                .map(|entry| entry.map(|e| e.path()))
                .collect::<std::io::Result<Vec<_>>>()?;
            children.sort();
            for child in children {
                if child.is_dir() {
                    let file = child.join("SKILL.md");
                    if file.try_exists()? {
                        files.insert(file.canonicalize()?);
                    }
                }
            }
        }
    }
    let mut skills = BTreeMap::new();
    let mut origins = BTreeMap::new();
    for file in files {
        let text = std::fs::read_to_string(&file)?;
        let skill =
            parse(&text).map_err(|e| Error::new(-32602, format!("{}: {e}", file.display())))?;
        if let Some(previous) = origins.insert(skill.name.clone(), file.clone()) {
            return Err(Error::new(
                -32602,
                format!(
                    "duplicate skill name '{}' in {} and {}",
                    skill.name,
                    previous.display(),
                    file.display()
                ),
            ));
        }
        skills.insert(skill.name.clone(), skill);
    }
    Ok(skills)
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

// Deliberately a restricted YAML reader, not an implementation of arbitrary YAML.
fn scalar(raw: &str) -> std::result::Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("empty scalar".into());
    }
    if raw.starts_with('\'') || raw.starts_with('"') {
        let quote = raw.as_bytes()[0] as char;
        let mut chars = raw[1..].char_indices().peekable();
        let mut out = String::new();
        while let Some((index, ch)) = chars.next() {
            if ch == quote {
                if quote == '\'' && chars.peek().is_some_and(|(_, c)| *c == '\'') {
                    chars.next();
                    out.push('\'');
                    continue;
                }
                let suffix = &raw[index + 2..];
                let rest = suffix.trim();
                if !rest.is_empty() && !(suffix.starts_with(' ') && rest.starts_with('#')) {
                    return Err("unexpected text after quoted scalar".into());
                }
                return Ok(out);
            }
            if quote == '"' && ch == '\\' {
                out.push(match chars.next().map(|(_, c)| c) {
                    Some('n') => '\n',
                    Some('r') => '\r',
                    Some('t') => '\t',
                    Some('"') => '"',
                    Some('\\') => '\\',
                    _ => return Err("unsupported double-quoted escape".into()),
                });
            } else {
                out.push(ch);
            }
        }
        return Err("unterminated quoted scalar".into());
    }
    let value = raw.split(" #").next().unwrap_or(raw).trim_end();
    if value.is_empty()
        || value.starts_with(['-', '?', ':', '!', '&', '*', '#', '|', '>', '@', '`', '%'])
        || value.contains(['[', ']', '{', '}'])
        || value.contains(": ")
        || value.ends_with(':')
    {
        return Err("unsupported or malformed plain scalar; quote the value".into());
    }
    Ok(value.into())
}

fn inline_tools(raw: &str) -> std::result::Result<Vec<String>, String> {
    // Tool identifiers cannot contain quotes, commas or whitespace, even when quoted.
    let end = raw.rfind(']').ok_or("unterminated tools list")?;
    let tail = raw[end + 1..].trim();
    if !tail.is_empty() && !tail.starts_with('#') {
        return Err("invalid tools list suffix".into());
    }
    let inner = &raw[1..end];
    if inner.contains('#') {
        return Err("comments inside inline tools lists are unsupported".into());
    }
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }
    inner
        .split(',')
        .map(|item| {
            let value = scalar(item)?;
            if !safe_name(&value) {
                return Err("unsafe tool name".into());
            }
            Ok(value)
        })
        .collect()
}

fn parse(text: &str) -> std::result::Result<Skill, String> {
    let mut offset = 0;
    let mut lines = text.split_inclusive('\n');
    if lines.next().map(|line| {
        offset += line.len();
        line.trim_end_matches(['\r', '\n'])
    }) != Some("---")
    {
        return Err("expected opening YAML frontmatter delimiter '---'".into());
    }
    let mut header = Vec::new();
    let mut closed = false;
    for line in lines {
        offset += line.len();
        let line = line.trim_end_matches(['\r', '\n']);
        if line == "---" {
            closed = true;
            break;
        }
        if line.contains('\t') {
            return Err("tabs are unsupported in frontmatter".into());
        }
        header.push(line);
    }
    if !closed {
        return Err("missing closing frontmatter delimiter".into());
    }
    let mut fields = BTreeSet::new();
    let mut name = None;
    let mut description = None;
    let mut tools = Vec::new();
    let mut i = 0;
    while i < header.len() {
        let line = header[i];
        i += 1;
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line.starts_with(' ') {
            return Err("unexpected frontmatter indentation".into());
        }
        let (key, raw) = line.split_once(':').ok_or("expected key: value")?;
        if !raw.is_empty() && !raw.starts_with(' ') {
            return Err("expected a space after frontmatter ':'".into());
        }
        if !fields.insert(key) {
            return Err(format!("duplicate frontmatter key '{key}'"));
        }
        let raw = raw.trim();
        match key {
            "name" => name = Some(scalar(raw)?),
            "description" if ["|", "|-", ">", ">-"].contains(&raw) => {
                let mut block = Vec::new();
                while i < header.len() && (header[i].starts_with(' ') || header[i].is_empty()) {
                    block.push(header[i]);
                    i += 1;
                }
                let indent = block
                    .iter()
                    .find(|l| !l.trim().is_empty())
                    .map(|l| l.len() - l.trim_start_matches(' ').len())
                    .unwrap_or(0);
                let mut value = String::new();
                for (n, line) in block.iter().enumerate() {
                    if !line.trim().is_empty()
                        && line.len() - line.trim_start_matches(' ').len() != indent
                    {
                        return Err("description block requires uniform indentation".into());
                    }
                    let line = if line.trim().is_empty() {
                        ""
                    } else {
                        &line[indent..]
                    };
                    value.push_str(line);
                    let next_nonempty = block.get(n + 1).is_some_and(|s| !s.trim().is_empty());
                    if raw.starts_with('>') && !line.is_empty() && next_nonempty {
                        value.push(' ');
                    } else if !(raw.starts_with('>')
                        && line.is_empty()
                        && next_nonempty
                        && block[..n].iter().any(|s| !s.trim().is_empty()))
                    {
                        value.push('\n');
                    }
                }
                let trimmed = value.trim_end_matches('\n').to_owned();
                description = Some(if raw.ends_with('-') || trimmed.is_empty() {
                    trimmed
                } else {
                    trimmed + "\n"
                });
            }
            "description" => description = Some(scalar(raw)?),
            "tools" if raw.starts_with('[') => tools = inline_tools(raw)?,
            "tools" if raw.is_empty() || raw.starts_with('#') => {
                let mut indent = None;
                while i < header.len() {
                    let line = header[i];
                    if line.trim().is_empty() || line.trim_start().starts_with('#') {
                        i += 1;
                        continue;
                    }
                    if !line.starts_with(' ') {
                        break;
                    }
                    let width = line.len() - line.trim_start_matches(' ').len();
                    if *indent.get_or_insert(width) != width {
                        return Err("inconsistent tools indentation".into());
                    }
                    let value = scalar(
                        line.trim_start()
                            .strip_prefix("- ")
                            .ok_or("expected tools list item")?,
                    )?;
                    if !safe_name(&value) {
                        return Err("unsafe tool name".into());
                    }
                    tools.push(value);
                    i += 1;
                }
                if tools.is_empty() {
                    return Err("use tools: [] for an empty list".into());
                }
            }
            "tools" => return Err("tools must be a list".into()),
            _ => return Err(format!("unknown frontmatter key '{key}'")),
        }
    }
    let name = name.ok_or("missing skill name")?;
    if !safe_name(&name) {
        return Err("unsafe skill name: use 1..64 ASCII letters/digits, '-' or '_', starting with a letter/digit".into());
    }
    let description = description.ok_or("missing skill description")?;
    if description.trim().is_empty() {
        return Err("empty skill description".into());
    }
    Ok(Skill {
        name,
        description,
        tools,
        content: text[offset..].into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "lh-skills-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn skill(&self, dir: &str, name: &str) -> PathBuf {
            let path = self.0.join(dir);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(
                path.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: A skill\n---\nBody\n"),
            )
            .unwrap();
            path
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn discovers_sorted_and_deduplicates() {
        let tmp = Temp::new();
        let z = tmp.skill("z", "z");
        tmp.skill("a", "a");
        let skills = discover(&[z, tmp.0.clone(), tmp.0.join("missing")]).unwrap();
        assert_eq!(
            skills.keys().map(String::as_str).collect::<Vec<_>>(),
            ["a", "z"]
        );
        assert_eq!(skills["a"].content, "Body\n");
    }
    #[test]
    fn duplicate_names_and_malformed_files_fail() {
        let tmp = Temp::new();
        tmp.skill("one", "same");
        let two = tmp.skill("two", "same");
        let error = discover(std::slice::from_ref(&tmp.0))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("duplicate skill name")
                && error.contains("one")
                && error.contains("two")
        );
        std::fs::write(two.join("SKILL.md"), "bad").unwrap();
        assert!(
            discover(&[two])
                .unwrap_err()
                .to_string()
                .contains("SKILL.md")
        );
    }
    #[cfg(unix)]
    #[test]
    fn canonical_aliases_are_deduplicated() {
        let tmp = Temp::new();
        let real = tmp.skill("real", "one");
        let alias = tmp.0.join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        assert_eq!(discover(&[real, alias, tmp.0.clone()]).unwrap().len(), 1);
    }
    #[test]
    fn scalars_blocks_and_tool_lists() {
        let skill = parse("---\r\nname: 'coding'\r\ndescription: >-\r\n  First line\r\n  second line\r\ntools:\r\n  - read_file\r\n  - 'run_command'\r\n---\r\n# Body\r\n").unwrap();
        assert_eq!(skill.description, "First line second line");
        assert_eq!(skill.tools, ["read_file", "run_command"]);
        assert_eq!(skill.content, "# Body\r\n");
        let skill = parse("---\nname: coding\ndescription: |\n  First\n  second\ntools: [read_file, \"run_command\"] # note\n---\n").unwrap();
        assert_eq!(skill.description, "First\nsecond\n");
        assert_eq!(skill.tools.len(), 2);
        assert_eq!(scalar("'it''s fine' # note").unwrap(), "it's fine");
        assert_eq!(scalar("\"a\\nb\"").unwrap(), "a\nb");
    }
    #[test]
    fn rejects_invalid_or_unsupported_metadata() {
        for header in [
            "name: ../escape\ndescription: test",
            "name: a\nname: b\ndescription: test",
            "name: a",
            "description: test",
            "name: a\ndescription: ''",
            "name: a\ndescription: [bad]",
            "name: a\ndescription: \"unclosed",
            "name: a\ndescription: test\ntools: shell",
            "name: a\ndescription: test\ntools: [shell,]",
            "name: a\ndescription: test\nunknown: value",
            "name: a\ndescription: &alias text",
            "name: a\ndescription: test\ntools:\n  - ../bad",
        ] {
            assert!(parse(&format!("---\n{header}\n---\n")).is_err(), "{header}");
        }
        assert!(parse("---\nname: a\ndescription: test").is_err());
    }
    #[test]
    fn search_order_without_mutating_environment() {
        let env = std::env::join_paths([Path::new("first"), Path::new("second")]).unwrap();
        assert_eq!(
            paths_with_env(Path::new("config"), &[PathBuf::from("plugin")], Some(env)),
            [
                "first",
                "second",
                "config/skills",
                "./skills",
                "plugin/skills"
            ]
            .map(PathBuf::from)
        );
    }
    #[test]
    fn bundled_skills_parse() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills");
        let skills = discover(&[root]).unwrap();
        for name in ["coding", "shell", "web-research", "git"] {
            assert!(skills.contains_key(name));
        }
    }
}

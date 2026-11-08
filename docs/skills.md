# Local skills

`lh_core::skills` discovers local Markdown instructions without starting plugins,
loading models, or making network requests. A `Skill` has serializable `name`,
`description`, `content` (the Markdown body without frontmatter), and `tools`.
Tools are advisory metadata, never permissions or an approval bypass. Only load
skills from trusted locations; their Markdown is instruction-bearing content.

## Discovery

`search_paths(config_dir, plugin_dirs)` returns, in order:

1. Nonempty `LH_SKILL_PATH` entries, using the platform path separator.
2. `<config_dir>/skills`.
3. `./skills`, relative to the process working directory.
4. `<plugin_dir>/skills` for each supplied plugin directory, in supplied order.

No tilde or variable expansion is performed. `discover(&paths)` accepts a skill
directory containing `SKILL.md` or a parent whose immediate child directories
contain `SKILL.md`. A direct skill takes priority over scanning its children;
there is no recursive traversal. Missing search entries are ignored; I/O and
invalid UTF-8 errors are returned. Canonical file paths are sorted and deduplicated
(including symlink aliases). Results are a `BTreeMap` ordered by skill name.
Distinct files declaring the same name fail with both paths, regardless of search
order: there is no first-wins or last-wins override. Invalid metadata errors name
the source file. Discovery does not enforce a filesystem sandbox.

## Frontmatter format

The core uses a dependency-free, deliberately restricted YAML reader, **not a
full YAML parser**. Unsupported structures fail instead of being silently ignored.
Files must begin with an exact `---` line and close frontmatter with another exact
`---` line. LF and CRLF are accepted. The remaining body is preserved verbatim.

```yaml
---
name: coding
description: >-
  Inspect and implement code changes.
  Run the relevant checks and report their results.
tools:
  - read_file
  - edit_file
  - run_command
---
```

Supported fields (unknown and repeated keys are errors):

- `name`: required, 1–64 ASCII letters/digits, hyphens or underscores, starting
  with a letter/digit. Names are case-sensitive; slashes, dots and whitespace fail.
- `description`: required nonblank string. Plain single-line, single-quoted
  (doubled apostrophes), and double-quoted strings are supported. Double-quoted
  escapes are limited to `\\`, `\"`, `\n`, `\r`, and `\t`.
- `tools`: optional, default empty. Accepts a single-line bracket list such as
  `[read_file, "run_command"]`, `[]`, or a uniformly space-indented block list.
  Identifiers use the same safe-name rules as skill names; order is preserved.
  Empty values and trailing commas are not supported.

Descriptions also accept uniformly space-indented literal (`|`, `|-`) or folded
(`>`, `>-`) blocks. Literal blocks preserve line breaks; folded blocks join
adjacent nonblank lines with spaces. A `-` strips the final newline; otherwise
one final newline is retained for nonempty blocks. More-indented block text,
indent indicators, keep-chomping (`+`), and multiline quoted/plain scalars are
not supported. Blank lines and whole-line comments are accepted between fields.
Inline comments follow a scalar separated by a space or follow a bracket list.
Tabs in frontmatter are rejected.

Plain strings do not support square/curly brackets, YAML indicators at
the start, or mapping-like `: ` / trailing `:`; quote such descriptions instead.
There is no implicit boolean/number/date conversion: all metadata scalars are
strings. Nested mappings, anchors, aliases, tags, directives and additional YAML
documents are unsupported. Use only the subset above, not arbitrary YAML.

## Bundled skills and verification

The repository ships `coding`, `shell`, `web-research`, and `git` under `skills/`.
Their tool names match the developer MCP tools documented in [tools.md](tools.md).

`harness-self` is built into `lh` (`crates/lh-core/skills/harness-self/SKILL.md`,
compiled in with `include_str!`, listed by `skill_registry` without a search path).
It explains `harness_info`, rebuilding in `build.source_dir`, `harness_restart`
and when not to restart. A local skill named `harness-self` replaces it (no
duplicate error, because built-ins are only added for names discovery did not
find). `lh doctor` lists every `SKILL.md` on the search path with its error, if
any, via `skills::inventory`, which never fails as a whole; `labelled_search_paths`
adds the origin (`LH_SKILL_PATH`, `config-dir`, `cwd`, `plugin-dir`) per entry.
The discovery module's unit tests cover ordering, duplicate names, canonical
aliases, malformed metadata, body preservation, multiline descriptions, list
forms, path ordering, and parsing the bundled skills. They run offline:

```sh
cargo test --offline -p lh-core skills::tests
```

The agent includes only name/description in its injected system index. Built-in
`skills_list` returns that index; `skill_view({name})` loads the Markdown body on
demand, within the same output/context budgets as other tool results. Skills do
not grant permissions or implicitly execute commands. Install/copy this directory
or set `LH_SKILL_PATH` when running outside the repository.

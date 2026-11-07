# AGENTS.md – notes for contributors and agents

- Language: everything in English (docs, comments, commit messages, identifiers).
- Rust stable (1.99+), edition 2024. Cargo workspace; layout and protocol are defined in
  `docs/ARCHITECTURE.md` – read it first and keep it in sync with the code in the same commit.
- Before every commit: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace` must be green. Tests must run offline without models.
- Keep the core light: no inference, no HTTP client and no new heavy dependency in `lh-core`.
  Justify each new dependency in the commit message.
- Toolchain on the dev host: `~/.cargo/bin` (add to `PATH`). llama.cpp CPU build:
  `/opt/data/tools/llama.cpp/llama-b12189/llama-server` (run with `LD_LIBRARY_PATH` set to that dir).
  Model cache: `LLAMA_CACHE=/opt/data/models/llama-cache` (d1-2B Q8_0 already downloaded there).
- Real-model runs (smoke, evals, benchmarks) are CPU-heavy: run only one at a time host-wide, check
  `pgrep -af llama-server` and `/proc/loadavg` first, keep them short (minutes), use `-t 8` threads at most,
  and stop every llama-server you started. Discard latency numbers taken under concurrent load.
- Test only on this Linux host; no multi-distro or long soak runs.

## Kanban: done means integrated

- Finished work is committed and integrated into `main` (fast-forward or merge from the task branch),
  with fmt/clippy/test green on the merged state. Resolve conflicts by content; never discard others' work.
- Serialize integration with `flock "$(git rev-parse --path-format=absolute --git-common-dir)/lh-integration.lock"`.
- Push `main` to `origin` after integration when a remote exists (allowed by this project rule).
- In the task result, report: commit hash, test summary, what was measured (with load average), known gaps.

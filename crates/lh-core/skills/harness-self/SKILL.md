---
name: harness-self
description: >-
  Inspect where this liquid-harness runs (host, config, plugins, skills,
  models), rebuild it and restart it safely. Read before restarting.
tools: [harness_info, harness_restart]
---
# Inspecting and restarting the harness

You run inside liquid-harness (`lh`). The "Harness environment" block in your
system message is a short summary; `harness_info` returns the full JSON.

## Inspect

- `harness_info` with no arguments returns everything. Use `section` to keep
  the result small: `build`, `host`, `config`, `discovery`, `models`,
  `backends`, `service`, `env` or `problems`.
- `problems` lists what is broken (missing llama-server, model not downloaded,
  plugin executable not found, unreadable skill) with a hint for each.
- The same report is available to the user as `lh doctor` (alias `lh where`,
  `--json` for JSON) and `GET /lh/v1/self`.
- Secrets are never shown: token and key variables only appear as set/unset.

## Rebuild

Only when the user asks for it and `build.source_dir` is known: in that
directory run `cargo build --workspace --release` (with the shell tool, which
needs approval). A rebuilt binary does not take effect until a restart;
`build.binary_changed` becomes true once the binary on disk differs from the
running one.

## Restart

- `harness_restart` with a short `reason` restarts the running `lh serve` in
  place: it stops accepting requests, lets in-flight requests finish (or
  cancels them with an error after the drain timeout), stops plugins and the
  llama-servers it owns, then re-executes the same binary with the same
  arguments and environment (same PID). The router and preloaded models load
  again in the background.
- It always needs the user's approval, ends this agent run, and the user's
  page reconnects after a few seconds.
- Outside `lh serve` (for example `lh chat`) the tool is not available; tell
  the user to run `lh restart --url http://127.0.0.1:8484` or the restart
  button on the web UI Status page instead.

## When not to restart

- Never restart without a clear reason the user agreed to (new binary,
  changed config or plugins, a stuck backend that recovery did not fix).
- Not while the user waits for another long answer or download: everything
  in flight is cancelled after the drain timeout.
- Not to "fix" a single failed request; models are reloaded automatically on
  the next request. Check `harness_info` `problems` and `models` first.
- Not repeatedly: if the harness fails again after a restart, report the
  problem and the log destination (`service.log`) instead.

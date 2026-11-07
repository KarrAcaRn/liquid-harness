---
name: shell
description: Execute bounded shell commands and inspect their actual results safely.
tools:
  - run_command
---
# Shell

Check the working directory and repository instructions before running commands.
Use explicit relative working directories and bounded timeouts. Quote arguments
and avoid interpolating untrusted text into shell syntax. Inspect exit_code,
timed_out and truncated as well as output; a returned result is not proof of success.

The shell has the user's permissions and is not a sandbox. Request approval for
mutating or destructive commands according to host policy. Never bypass approval,
expose secrets, or silently retry commands that may already have had side effects.
Clean up processes you start and report failures without fabricating output.

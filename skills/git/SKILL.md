---
name: git
description: Inspect repository state and make scoped, verified Git changes.
tools: [run_command, read_file]
---
# Git

Read repository instructions and inspect status, branch and diff before changes.
Preserve other contributors' edits. Stage only intended paths, inspect the staged
diff, and run required checks before committing when commits are authorized.

Do not reset, clean, force-push, rewrite history, or discard uncommitted work
without explicit authorization. Treat remote publication as a separate action
subject to host approval. Resolve conflicts by understanding both changes, not
by blindly choosing a side. Verify the final repository state and report actual
commit identifiers only when a commit was created.

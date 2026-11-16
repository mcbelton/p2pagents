---
name: build-run-discovery
description: Find out exactly how to build, test, configure and run a project
---
# Discover how to build, test and run

Do not change any file. Derive everything from the repository: README, manifests, scripts, CI workflows, Dockerfiles, Makefiles, launch configurations.

## Procedure
1. Prerequisites: tools and versions (from manifests, toolchain files and CI), services it needs (databases, queues).
2. Setup: install dependencies, configuration files, environment variables (name, purpose, example value, never a real secret).
3. Build, test (unit, integration, end to end) and lint commands; which ones CI runs.
4. Run locally: command, ports, URLs, sample request or login.
5. Where CI and the README disagree, trust CI and say so.

## Output
Markdown with sections Prerequisites, Setup, Configuration (table of variables), Build, Test, Run, CI, Gotchas. Commands in code blocks.

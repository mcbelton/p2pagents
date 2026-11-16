---
name: reverse-engineer
description: Build an accurate model of an existing codebase from the code itself
---
# Reverse-engineer a codebase

Read before you conclude. Every statement must point to a file (and ideally a symbol). Change nothing.

## Procedure
1. **Map the repository**: top-level folders, languages, manifests and their dependencies, build and CI files, configuration, docs.
2. **Find the entry points**: `main` functions, servers and routes, CLI commands, jobs and schedulers, event handlers, exported APIs.
3. **Identify the components**: for each one, its responsibility, key types and functions, and what it depends on.
4. **Trace the main data flows** end to end (request to response, event to side effect): where data comes in, how it is validated and transformed, where it is stored, what leaves the system.
5. **List the stores and external services**: databases, caches, queues, files, third-party APIs, with how they are configured.
6. **Note conventions**: error handling, logging, testing style, naming, module layout.

## Output (concise Markdown)
- `## Purpose` (2-3 sentences)
- `## Components` (table: component, responsibility, key files)
- `## Entry points`
- `## Data flows` (numbered steps with file references)
- `## Stores and external services`
- `## Conventions`
- `## Open questions` (what the code doesn't make clear)

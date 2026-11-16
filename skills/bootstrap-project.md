---
name: bootstrap-project
description: Set up a fresh project so a team can develop, test and ship it from day one
---
# Bootstrap a project

Work only inside the current folder. Prefer the ecosystem's own tools over hand-written files.

## Procedure
1. **Detect** what is already there: manifests (package.json, Cargo.toml, pyproject.toml, *.csproj, go.mod, pom.xml), lock files, source folders, existing CI. Never overwrite existing work; extend it.
2. **Choose** (empty folder only) the language and toolchain the task asks for; otherwise the most common choice for the stated purpose. Say why in one line.
3. **Scaffold with the official tool** (`cargo init`, `npm init -y`, `dotnet new`, `uv init`, `go mod init`), then add:
   - a minimal program or library entry point that builds;
   - one meaningful unit test (not a placeholder `assert true`);
   - a lock file, committed;
   - a `.gitignore` for the toolchain (build output, dependencies, secrets such as `.env`).
4. **README.md**: purpose, prerequisites with exact versions, setup, build, test, run, and how to contribute.
5. **CI** (`.github/workflows/ci.yml`): build and test on push and pull request, using the same commands as the README. Pin action versions.
6. **Claims** (`evidence/claims.json` when asked): each claim is a testable statement with the tests or checks that prove it.

## Rules
- Never add credentials, tokens or real personal data. Use placeholders and document required environment variables.
- Every command you document must be one you ran successfully.

## Report
End with: the toolchain and versions, the files you added or changed, and the exact build and test commands.

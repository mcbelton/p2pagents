# Implementation Plan: Photo Albums

**Branch**: `001-photo-albums` | **Spec**: [spec.md](./spec.md)

## Summary

Albums are a table of album rows and an ordered join table to photos, served by two
REST endpoints and shown on an album page.

## Technical Context

**Language/Version**: TypeScript 5
**Primary Dependencies**: Express, React
**Storage**: PostgreSQL
**Testing**: Vitest

## Project Structure

```text
src/albums/      # model, service, routes
web/albums/      # album page
tests/albums/    # one test per acceptance scenario
```

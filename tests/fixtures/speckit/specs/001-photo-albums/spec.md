# Feature Specification: Photo Albums

**Feature Branch**: `001-photo-albums`

**Created**: 2026-08-01

**Status**: Draft

**Input**: User description: "Let people group their photos into albums"

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Create an album (Priority: P1)

A member groups photos from their library into a named album.

**Why this priority**: Albums are the feature; nothing else works without them.

**Independent Test**: Create an album with two photos and see it in the album list.

**Acceptance Scenarios**:

1. **Given** a library with photos, **When** the member creates album "Trip" with two of them, **Then** "Trip" is listed with 2 photos
2. **Given** an album named "Trip", **When** the member creates another album "Trip", **Then** they are told the name is taken

---

### User Story 2 - Reorder photos (Priority: P2)

A member drags photos within an album to change their order.

**Why this priority**: Albums tell a story, so order matters.

**Independent Test**: Move the last photo first and reload.

**Acceptance Scenarios**:

1. **Given** an album with photos A, B, C, **When** the member drags C before A, **Then** the album shows C, A, B after a reload

---

### Edge Cases

- An empty album shows a hint to add photos
- Deleting a photo from the library removes it from every album

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: System MUST let members create an album with a unique name
- **FR-002**: System MUST keep the order of photos in an album
- **FR-003**: System MUST keep albums private to their owner unless shared via [NEEDS CLARIFICATION: share by link or by account?]

### Key Entities

- **Album**: a name, an owner and an ordered list of photos

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Members create their first album in under 1 minute
- **SC-002**: 90% of members who upload 20+ photos create an album within a week

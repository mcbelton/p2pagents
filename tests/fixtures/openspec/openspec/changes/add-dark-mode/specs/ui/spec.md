## ADDED Requirements

### Requirement: Theme selection
The app SHALL let users switch between light and dark themes,
defaulting to the system preference.

#### Scenario: User toggles dark mode
- **WHEN** the user clicks the theme toggle
- **THEN** the app switches to dark mode and persists the choice

## MODIFIED Requirements

### Requirement: Contrast
Text SHALL keep a contrast ratio of at least 4.5:1 in both themes.

#### Scenario: Dark theme contrast
- **WHEN** the dark theme is on
- **THEN** body text has a contrast ratio of at least 4.5:1

## REMOVED Requirements

### Requirement: Fixed light theme
**Reason**: Users pick a theme now.
**Migration**: None.

## RENAMED Requirements

- FROM: `### Requirement: Colors`
- TO: `### Requirement: Theme colors`

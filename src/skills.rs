//! Skills: reusable instructions for agents (how to bootstrap a project, review
//! security, debug an incident, ...). A plan step names them with `skill=a,b` and
//! the boss puts their text in the step's prompt, so every peer gets the same
//! procedure whichever provider runs it. Built-in skills ship with p2pagents
//! (`skills/*.md`); a profile's own skills (`<profile>/skills/<name>.md`) are added
//! to them and replace built-ins of the same name.
//!
//! A skill is Markdown with front matter:
//!
//! ```text
//! ---
//! name: security-review
//! description: Security review of a change against common vulnerability classes
//! ---
//! # Security review
//! ...
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::profile::validate_name;

const BUILT_IN: &[(&str, &str)] = &[
    (
        "bootstrap-project",
        include_str!("../skills/bootstrap-project.md"),
    ),
    (
        "reverse-engineer",
        include_str!("../skills/reverse-engineer.md"),
    ),
    (
        "build-run-discovery",
        include_str!("../skills/build-run-discovery.md"),
    ),
    ("risk-review", include_str!("../skills/risk-review.md")),
    ("design-doc", include_str!("../skills/design-doc.md")),
    (
        "implement-change",
        include_str!("../skills/implement-change.md"),
    ),
    ("code-review", include_str!("../skills/code-review.md")),
    (
        "security-review",
        include_str!("../skills/security-review.md"),
    ),
    (
        "privacy-review",
        include_str!("../skills/privacy-review.md"),
    ),
    (
        "compliance-check",
        include_str!("../skills/compliance-check.md"),
    ),
    ("ops-insights", include_str!("../skills/ops-insights.md")),
    ("tsg-writing", include_str!("../skills/tsg-writing.md")),
    (
        "incident-debugging",
        include_str!("../skills/incident-debugging.md"),
    ),
    ("write-spec", include_str!("../skills/write-spec.md")),
    ("api-contract", include_str!("../skills/api-contract.md")),
    ("prototype-ux", include_str!("../skills/prototype-ux.md")),
    (
        "spec-to-claims",
        include_str!("../skills/spec-to-claims.md"),
    ),
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    /// `built-in` or `profile`.
    pub source: &'static str,
}

/// Splits front matter from the body; returns (description, body).
fn parse(text: &str) -> (String, &str) {
    let Some(rest) = text.strip_prefix("---") else {
        return (String::new(), text);
    };
    let Some((head, body)) = rest.split_once("\n---") else {
        return (String::new(), text);
    };
    let description = head
        .lines()
        .find_map(|l| l.strip_prefix("description:"))
        .map(|d| d.trim().to_owned())
        .unwrap_or_default();
    (description, body.trim_start_matches(['\r', '\n']))
}

#[derive(Debug, Clone)]
pub struct Skills {
    dir: PathBuf,
}

impl Skills {
    /// The skills of a profile whose own skills live in `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn own(&self, name: &str) -> Option<String> {
        validate_name(name).ok()?;
        std::fs::read_to_string(self.dir.join(format!("{name}.md"))).ok()
    }

    /// The skill's full text (front matter included).
    pub fn text(&self, name: &str) -> Result<String> {
        if let Some(text) = self.own(name) {
            return Ok(text);
        }
        BUILT_IN
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, t)| (*t).to_owned())
            .with_context(|| format!("there is no skill '{name}' (see p2pagents skill list)"))
    }

    pub fn list(&self) -> Vec<SkillInfo> {
        let mut skills: Vec<SkillInfo> = BUILT_IN
            .iter()
            .map(|(name, text)| SkillInfo {
                name: (*name).to_owned(),
                description: parse(text).0,
                source: "built-in",
            })
            .collect();
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(name) = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .filter(|_| path.extension().is_some_and(|e| e == "md"))
                    .filter(|n| validate_name(n).is_ok())
                else {
                    continue;
                };
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                skills.retain(|s| s.name != name);
                skills.push(SkillInfo {
                    name: name.to_owned(),
                    description: parse(&text).0,
                    source: "profile",
                });
            }
        }
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        skills
    }

    /// Adds or replaces a profile skill.
    pub fn add(&self, name: &str, text: &str) -> Result<()> {
        validate_name(name).context("invalid skill name")?;
        if text.trim().is_empty() {
            bail!("the skill is empty");
        }
        std::fs::create_dir_all(&self.dir)?;
        std::fs::write(self.dir.join(format!("{name}.md")), text)?;
        Ok(())
    }

    /// Removes a profile skill (built-ins can't be removed, only replaced).
    pub fn remove(&self, name: &str) -> Result<bool> {
        validate_name(name).context("invalid skill name")?;
        match std::fs::remove_file(self.dir.join(format!("{name}.md"))) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// The instructions for a prompt: each skill's body under a heading.
    pub fn instructions(&self, names: &[String]) -> Result<String> {
        let mut text = String::new();
        for name in names {
            let full = self.text(name)?;
            text += &format!("\n### Skill: {name}\n{}\n", parse(&full).1.trim());
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_skills_are_listed_and_profile_skills_replace_them() {
        let dir = tempfile::tempdir().unwrap();
        let skills = Skills::new(dir.path().join("skills"));
        let all = skills.list();
        assert_eq!(all.len(), BUILT_IN.len());
        assert!(
            all.iter()
                .all(|s| !s.description.is_empty() && s.source == "built-in")
        );
        let review = skills.instructions(&["security-review".into()]).unwrap();
        assert!(review.starts_with("\n### Skill: security-review\n# Security review"));
        assert!(!review.contains("description:"), "front matter is left out");

        skills
            .add(
                "security-review",
                "---\nname: security-review\ndescription: Ours\n---\nUse our checklist.",
            )
            .unwrap();
        skills.add("house-style", "Write British English.").unwrap();
        let mine = skills.list();
        let ours = mine.iter().find(|s| s.name == "security-review").unwrap();
        assert_eq!(
            (ours.description.as_str(), ours.source),
            ("Ours", "profile")
        );
        assert!(mine.iter().any(|s| s.name == "house-style"));
        let text = skills
            .instructions(&["security-review".into(), "house-style".into()])
            .unwrap();
        assert!(text.contains("Use our checklist.") && text.contains("Write British English."));
        assert!(skills.instructions(&["nope".into()]).is_err());
        assert!(skills.add("../x", "y").is_err());
        assert!(skills.remove("security-review").unwrap());
        assert!(
            skills
                .text("security-review")
                .unwrap()
                .contains("Security review"),
            "built-in again"
        );
    }
}

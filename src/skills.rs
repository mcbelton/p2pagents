//! Skills: reusable instructions for agents (how to bootstrap a project, review
//! security, debug an incident, ...). A plan step names them with `skill=a,b` and
//! the boss puts their text in the step's prompt, so every peer gets the same
//! procedure whichever provider runs it. Built-in skills ship with p2pagents
//! (`skills/*.md`); a profile's own skills (`<profile>/skills/<name>.md`) are added
//! to them and replace built-ins of the same name. Enabled plugins add theirs
//! (`SKILL.md`): by name when no other skill has it, always as `<plugin>.<skill>`.
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

/// The longest skill read from a file (it goes into prompts).
const MAX_SKILL_BYTES: u64 = 64 * 1024;

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
    (
        "architecture-design",
        include_str!("../skills/architecture-design.md"),
    ),
    ("adr", include_str!("../skills/adr.md")),
    ("data-design", include_str!("../skills/data-design.md")),
    (
        "threat-modeling",
        include_str!("../skills/threat-modeling.md"),
    ),
    ("test-strategy", include_str!("../skills/test-strategy.md")),
    (
        "operability-design",
        include_str!("../skills/operability-design.md"),
    ),
    (
        "review-technical",
        include_str!("../skills/review-technical.md"),
    ),
    (
        "review-product",
        include_str!("../skills/review-product.md"),
    ),
    (
        "review-leadership",
        include_str!("../skills/review-leadership.md"),
    ),
    (
        "review-customer",
        include_str!("../skills/review-customer.md"),
    ),
    (
        "stakeholder-simulation",
        include_str!("../skills/stakeholder-simulation.md"),
    ),
    (
        "feedback-synthesis",
        include_str!("../skills/feedback-synthesis.md"),
    ),
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    /// `built-in`, `profile` or `plugin`.
    pub source: &'static str,
    /// The plugin it comes from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
}

/// A skill of an enabled plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSkillRef {
    pub plugin: String,
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

/// Reads a skill to import: a `SKILL.md`, a folder with one, or a Markdown file.
/// Returns its name (from its front matter, else its folder or file name) and text.
pub fn read_skill_file(path: &Path) -> Result<(String, String)> {
    let file = if path.is_dir() {
        path.join("SKILL.md")
    } else {
        path.to_path_buf()
    };
    let meta = std::fs::metadata(&file).with_context(|| {
        format!(
            "{} has no SKILL.md or isn't a Markdown file",
            path.display()
        )
    })?;
    if meta.len() > MAX_SKILL_BYTES {
        bail!(
            "{} is over 64 KB; skills go into prompts, keep them short",
            file.display()
        );
    }
    let text =
        std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
    let text = text.trim_start_matches('\u{feff}').to_owned();
    let fallback = if file.file_name().is_some_and(|n| n == "SKILL.md") {
        file.parent().and_then(|p| p.file_name())
    } else {
        file.file_stem()
    }
    .and_then(|n| n.to_str())
    .unwrap_or_default()
    .to_owned();
    let name = front_matter_name(&text).unwrap_or(fallback);
    validate_name(&name)
        .with_context(|| format!("the skill's name {name:?} isn't usable; rename it"))?;
    Ok((name, text))
}

/// The `name:` in a skill's front matter, if it has one.
pub fn front_matter_name(text: &str) -> Option<String> {
    text.trim_start_matches('\u{feff}')
        .strip_prefix("---")
        .and_then(|r| r.split_once("\n---"))
        .and_then(|(head, _)| {
            head.lines()
                .find_map(|l| l.strip_prefix("name:"))
                .map(|n| n.trim().trim_matches(['"', '\'']).to_owned())
        })
        .filter(|n| !n.is_empty())
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
    plugins: Vec<PluginSkillRef>,
}

impl Skills {
    /// The skills of a profile whose own skills live in `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            plugins: Vec::new(),
        }
    }

    /// With the skills of enabled plugins: (plugin, skill, its `SKILL.md`).
    pub fn with_plugins(
        mut self,
        skills: Vec<(String, crate::plugins::PluginSkill, PathBuf)>,
    ) -> Self {
        self.plugins = skills
            .into_iter()
            .map(|(plugin, s, path)| PluginSkillRef {
                plugin,
                name: s.name,
                description: s.description,
                path,
            })
            .collect();
        self
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn own(&self, name: &str) -> Option<String> {
        validate_name(name).ok()?;
        std::fs::read_to_string(self.dir.join(format!("{name}.md"))).ok()
    }

    fn built_in(name: &str) -> Option<&'static str> {
        BUILT_IN.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)
    }

    /// The plugin skill `name` refers to: `<plugin>.<skill>`, or a short name no
    /// built-in or profile skill has.
    fn plugin(&self, name: &str) -> Option<&PluginSkillRef> {
        self.plugins
            .iter()
            .find(|s| format!("{}.{}", s.plugin, s.name) == name)
            .or_else(|| self.plugins.iter().find(|s| s.name == name))
    }

    /// The skill's full text (front matter included).
    pub fn text(&self, name: &str) -> Result<String> {
        if let Some(text) = self.own(name) {
            return Ok(text);
        }
        if let Some(text) = Self::built_in(name) {
            return Ok(text.to_owned());
        }
        if let Some(skill) = self.plugin(name) {
            let meta = std::fs::metadata(&skill.path).with_context(|| {
                format!(
                    "skill '{name}' of plugin '{}' is missing; update the plugin",
                    skill.plugin
                )
            })?;
            if meta.len() > MAX_SKILL_BYTES {
                bail!("skill '{name}' of plugin '{}' is over 64 KB", skill.plugin);
            }
            return std::fs::read_to_string(&skill.path)
                .with_context(|| format!("reading skill '{name}' of plugin '{}'", skill.plugin));
        }
        bail!("there is no skill '{name}' (see p2pagents skill list)")
    }

    pub fn list(&self) -> Vec<SkillInfo> {
        let mut skills: Vec<SkillInfo> = BUILT_IN
            .iter()
            .map(|(name, text)| SkillInfo {
                name: (*name).to_owned(),
                description: parse(text).0,
                source: "built-in",
                plugin: None,
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
                    plugin: None,
                });
            }
        }
        for skill in &self.plugins {
            let short_taken = skills.iter().any(|s| s.name == skill.name);
            skills.push(SkillInfo {
                name: if short_taken {
                    format!("{}.{}", skill.plugin, skill.name)
                } else {
                    skill.name.clone()
                },
                description: skill.description.clone(),
                source: "plugin",
                plugin: Some(skill.plugin.clone()),
            });
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
        if text.len() as u64 > MAX_SKILL_BYTES {
            bail!("the skill is over 64 KB; skills go into prompts, keep them short");
        }
        std::fs::create_dir_all(&self.dir)?;
        std::fs::write(self.dir.join(format!("{name}.md")), text)?;
        Ok(())
    }

    /// Removes a profile skill (built-ins can't be removed, only replaced; a
    /// plugin's go with their plugin).
    pub fn remove(&self, name: &str) -> Result<bool> {
        validate_name(name).context("invalid skill name")?;
        match std::fs::remove_file(self.dir.join(format!("{name}.md"))) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if let Some(skill) = self.plugin(name) {
                    bail!(
                        "skill '{name}' comes with plugin '{}': turn the plugin off or remove it",
                        skill.plugin
                    );
                }
                if Self::built_in(name).is_some() {
                    bail!("'{name}' is built in: it can be replaced (skill add) but not removed");
                }
                Ok(false)
            }
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

    #[test]
    fn plugin_skills_are_found_by_name_or_as_plugin_dot_skill_and_skill_files_import() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("team");
        for (folder, text) in [
            (
                "release",
                "---\nname: release-notes\ndescription: Notes\n---\nWrite notes.",
            ),
            (
                "review",
                "---\nname: code-review\ndescription: Theirs\n---\nTheir review.",
            ),
        ] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
            std::fs::write(root.join(folder).join("SKILL.md"), text).unwrap();
        }
        let skill = |name: &str, folder: &str| {
            (
                "team".to_owned(),
                crate::plugins::PluginSkill {
                    name: name.into(),
                    description: String::new(),
                    path: format!("{folder}/SKILL.md"),
                },
                root.join(folder).join("SKILL.md"),
            )
        };
        let skills = Skills::new(dir.path().join("skills")).with_plugins(vec![
            skill("release-notes", "release"),
            skill("code-review", "review"),
        ]);
        let use_ = |name: &str| skills.instructions(&[name.to_owned()]).unwrap();
        assert!(
            use_("release-notes").contains("Write notes."),
            "by name when no other skill has it"
        );
        assert!(
            use_("team.code-review").contains("Their review."),
            "always as plugin.skill"
        );
        assert!(
            !use_("code-review").contains("Their review."),
            "the built-in keeps its name"
        );
        let listed = skills.list();
        assert!(
            listed
                .iter()
                .any(|s| s.name == "release-notes" && s.plugin.as_deref() == Some("team"))
        );
        assert!(
            listed
                .iter()
                .any(|s| s.name == "team.code-review" && s.source == "plugin")
        );
        let err = skills.remove("release-notes").unwrap_err().to_string();
        assert!(err.contains("comes with plugin 'team'"), "{err}");

        // Importing: a SKILL.md, the folder holding one, or a Markdown file.
        let (name, text) = read_skill_file(&root.join("release")).unwrap();
        assert_eq!(
            (name.as_str(), text.contains("Write notes.")),
            ("release-notes", true)
        );
        std::fs::write(dir.path().join("house-style.md"), "Write plainly.").unwrap();
        assert_eq!(
            read_skill_file(&dir.path().join("house-style.md"))
                .unwrap()
                .0,
            "house-style"
        );
        std::fs::write(dir.path().join("big.md"), "x".repeat(70 * 1024)).unwrap();
        assert!(
            read_skill_file(&dir.path().join("big.md")).is_err(),
            "over 64 KB"
        );
        assert!(read_skill_file(&dir.path().join("nothing-here")).is_err());
        assert_eq!(
            front_matter_name("---\nname: \"x-y\"\n---\nz"),
            Some("x-y".into())
        );
    }
}

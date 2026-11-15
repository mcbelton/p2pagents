//! Playbooks: ready-made plans for the whole lifecycle, filled in with your peers,
//! folders and checks. Each is a normal plan (claims, steps, checks, approvals):
//!
//! - `bootstrap`: a fresh project, with all prerequisites for development (toolchain,
//!   README, tests, CI, claims), proven by the owner's checks;
//! - `discover`: an existing project, reverse-engineered into the knowledge base
//!   (architecture, build and run, risks, glossary), confirmed by a person;
//! - `sdlc`: a change through design, implementation, independent verification,
//!   security, privacy and compliance gates and release approval;
//! - `learn`: insights and troubleshooting guide drafts from how the system runs.

use std::collections::BTreeMap;

use anyhow::{Result, bail};

const PLAYBOOKS: &[(&str, &str, &str)] = &[
    (
        "bootstrap",
        "Fresh project: set up everything needed for development, with evidence",
        include_str!("../playbooks/bootstrap.plan"),
    ),
    (
        "discover",
        "Existing project: reverse-engineer it into the knowledge base",
        include_str!("../playbooks/discover.plan"),
    ),
    (
        "sdlc",
        "A change through design, verification, security, privacy, compliance and release gates",
        include_str!("../playbooks/sdlc.plan"),
    ),
    (
        "learn",
        "Insights and troubleshooting guides from how the system runs",
        include_str!("../playbooks/learn.plan"),
    ),
];

/// Names and descriptions of the playbooks.
pub fn list() -> Vec<(&'static str, &'static str)> {
    PLAYBOOKS.iter().map(|(n, d, _)| (*n, *d)).collect()
}

fn template(name: &str) -> Result<&'static str> {
    PLAYBOOKS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, _, t)| *t)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "there is no playbook '{name}' (there are: {})",
                PLAYBOOKS.iter().map(|p| p.0).collect::<Vec<_>>().join(", ")
            )
        })
}

fn default(playbook: &str, var: &str) -> Option<&'static str> {
    Some(match var {
        "folder" if playbook == "learn" => "data",
        "folder" => "app",
        "project" => "the project",
        "approver" | "privacy_approver" => "me",
        "reviewers" => "qa",
        "change_id" => "change",
        "build" => "build",
        "test" => "test",
        "lint" => "lint",
        "secscan" => "secscan",
        "licenses" => "licenses",
        "secrets" => "secrets",
        _ => return None,
    })
}

/// The `{{name}}` placeholders of a playbook, with their defaults (None: required).
pub fn variables(name: &str) -> Result<Vec<(String, Option<&'static str>)>> {
    let text = template(name)?;
    let mut vars: Vec<(String, Option<&'static str>)> = Vec::new();
    for part in text.split("{{").skip(1) {
        if let Some((var, _)) = part.split_once("}}")
            && !vars.iter().any(|(v, _)| v == var)
        {
            vars.push((var.to_owned(), default(name, var)));
        }
    }
    Ok(vars)
}

/// The playbook as plan text with `vars` (and defaults) filled in. Values may not
/// contain line breaks; ids (peers, folders, checks, topics) must be valid names.
pub fn render(name: &str, vars: &BTreeMap<String, String>) -> Result<String> {
    let mut text = template(name)?.to_owned();
    let mut missing = Vec::new();
    for (var, default) in variables(name)? {
        let value = match (vars.get(&var), default) {
            (Some(v), _) => v.trim().to_owned(),
            (None, Some(d)) => d.to_owned(),
            (None, None) => {
                missing.push(var);
                continue;
            }
        };
        if value.is_empty() || value.contains(['\n', '\r']) {
            bail!("{var} must be a single, non-empty line");
        }
        if !matches!(var.as_str(), "change" | "project") && value.contains(char::is_whitespace) {
            bail!("{var} must be a single word, got {value:?}");
        }
        text = text.replace(&format!("{{{{{var}}}}}"), &value);
    }
    if !missing.is_empty() {
        bail!(
            "playbook '{name}' needs: {} (e.g. --set {}=...)",
            missing.join(", "),
            missing[0]
        );
    }
    crate::plan::Plan::parse(&text)?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::Plan;

    fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect()
    }

    #[test]
    fn every_playbook_renders_to_a_valid_plan_with_claims() {
        let all = vars(&[
            ("peer", "alice"),
            ("change", "Add CSV export"),
            ("change_id", "csv"),
        ]);
        for (name, _) in list() {
            let text = render(name, &all).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            assert!(!text.contains("{{"), "{name}: {text}");
            let plan = Plan::parse(&text).unwrap();
            assert!(!plan.claims.is_empty(), "{name} proves claims");
            for claim in &plan.claims {
                assert!(
                    plan.steps.iter().any(|s| s.claims.contains(&claim.id)),
                    "{name}: no step proves {}",
                    claim.id
                );
            }
            // Every claim is backed by runtime checks or a sign-off, never a model alone.
            for step in plan.steps.iter().filter(|s| !s.claims.is_empty()) {
                assert!(
                    step.approve || !step.checks.is_empty(),
                    "{name}: step {}",
                    step.id
                );
            }
        }
        let sdlc = Plan::parse(&render("sdlc", &all).unwrap()).unwrap();
        let verify = sdlc.steps.iter().find(|s| s.id == "verify").unwrap();
        assert_eq!(verify.apply.as_deref(), Some("impl"));
        assert_eq!(verify.target, crate::plan::Target::Squad("qa".into()));
        let discover = Plan::parse(&render("discover", &all).unwrap()).unwrap();
        assert!(
            discover
                .steps
                .iter()
                .any(|s| s.save.as_deref() == Some("architecture"))
        );
    }

    #[test]
    fn rendering_needs_required_values_and_rejects_bad_ones() {
        let err = render("sdlc", &vars(&[("peer", "alice")]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("needs: change"), "{err}");
        let err = render("discover", &BTreeMap::new())
            .unwrap_err()
            .to_string();
        assert!(err.contains("needs: peer"), "{err}");
        assert!(render("discover", &vars(&[("peer", "a b")])).is_err());
        assert!(render("sdlc", &vars(&[("peer", "a"), ("change", "x\ny")])).is_err());
        assert!(render("nope", &BTreeMap::new()).is_err());
        let text = render("discover", &vars(&[("peer", "bob"), ("folder", "repo")])).unwrap();
        assert!(text.contains("@bob:repo") && text.contains("approve @me"));
        let vars = variables("sdlc").unwrap();
        assert!(vars.contains(&("change".into(), None)));
        assert!(vars.contains(&("test".into(), Some("test"))));
    }
}

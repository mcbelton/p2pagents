//! Linked repositories: clone with the profile's GitHub account (token only in the
//! environment), share with a worktree per task, sync, push task branches and open
//! pull requests. A local bare repository stands in for GitHub, and a fake `gh`
//! records how the pull request was requested.

use std::path::Path;
use std::process::Command;

use p2pagents::github::{GithubCredentials, RepoStore, link_url, task_branches, unlink};
use p2pagents::worktree::Worktree;
use p2pagents::{Access, Profile};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn commit(dir: &Path, file: &str, text: &str, message: &str) {
    std::fs::write(dir.join(file), text).unwrap();
    git(dir, &["add", "-A"]);
    git(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            message,
        ],
    );
}

#[tokio::test]
async fn linked_repositories_clone_share_sync_push_and_open_pull_requests() {
    let tmp = tempfile::tempdir().unwrap();
    // "GitHub": a bare repository with one commit on main.
    let origin = tmp.path().join("origin.git");
    std::fs::create_dir(&origin).unwrap();
    git(&origin, &["init", "-q", "--bare", "--initial-branch=main"]);
    let seed = tmp.path().join("seed");
    Command::new("git")
        .args(["clone", "-q"])
        .arg(&origin)
        .arg(&seed)
        .status()
        .unwrap();
    git(&seed, &["config", "core.autocrlf", "false"]);
    git(&seed, &["checkout", "-q", "-b", "main"]);
    commit(&seed, "README.md", "app\n", "init");
    git(&seed, &["push", "-q", "origin", "main"]);

    let profile = Profile::open_in(&tmp.path().join("home/profiles"), "alice").unwrap();
    let creds = GithubCredentials {
        user: "alice".into(),
        host: "github.com".into(),
        token: "t0k3n-never-stored".into(),
    };
    let url = origin.to_string_lossy().into_owned();
    let checks = [("test".to_owned(), "exit 0".to_owned())].into();
    let (repo, shares) = link_url(
        &profile,
        &creds,
        &url,
        "github.com",
        "acme/app",
        None,
        Access::Full,
        checks,
    )
    .await
    .unwrap();
    assert_eq!((repo.name.as_str(), repo.branch.as_str()), ("app", "main"));
    assert!(
        repo.path.starts_with(profile.repos_dir()),
        "cloned outside the profiles folder"
    );
    let share = shares.get("app").unwrap();
    assert!(
        share.worktrees && share.checks.contains_key("test") && share.max_access == Access::Full
    );
    assert_eq!(
        profile.shares().unwrap().get("app").unwrap().path,
        repo.path,
        "saved"
    );
    assert_eq!(
        RepoStore::load(&profile.repos_path())
            .unwrap()
            .get("app")
            .unwrap(),
        &repo
    );
    let config = std::fs::read_to_string(repo.path.join(".git/config")).unwrap();
    assert!(
        !config.contains("t0k3n"),
        "the token is never stored in the clone"
    );
    let again = link_url(
        &profile,
        &creds,
        &url,
        "github.com",
        "acme/app",
        None,
        Access::Full,
        Default::default(),
    )
    .await;
    assert!(again.unwrap_err().to_string().contains("already linked"));

    // An agent's task commits on its own branch (as in a --worktrees folder).
    let root = tmp.path().join("worktrees");
    let wt = Worktree::create(&repo.path, &root, "p2p/boss/1-1")
        .await
        .unwrap();
    std::fs::write(wt.path.join("feature.txt"), "new\n").unwrap();
    wt.commit("p2pagents: task 1 from boss\n\nAdd the feature")
        .await
        .unwrap();
    drop(wt);
    let branches = task_branches(&repo).await.unwrap();
    assert_eq!(branches[0].name, "p2p/boss/1-1");
    assert!(
        branches[0]
            .subject
            .starts_with("p2pagents: task 1 from boss")
    );

    creds.push(&repo, "p2p/boss/1-1").await.unwrap();
    assert!(git(&seed, &["ls-remote", "origin", "refs/heads/p2p/*"]).contains("p2p/boss/1-1"));
    assert!(
        creds.push(&repo, "main").await.is_err(),
        "only task branches are pushed"
    );

    // Someone else pushes to main; sync fast-forwards the clone.
    git(&seed, &["pull", "-q", "origin", "main"]);
    commit(&seed, "CHANGELOG.md", "v2\n", "release v2");
    git(&seed, &["push", "-q", "origin", "main"]);
    let head = creds.sync(&repo).await.unwrap();
    assert_eq!(head, git(&seed, &["rev-parse", "--short", "HEAD"]));
    assert!(repo.path.join("CHANGELOG.md").is_file());

    // The pull request goes through gh with the profile's token in its environment.
    let fake = tmp.path().join(if cfg!(windows) { "gh.cmd" } else { "gh" });
    let record = tmp.path().join("gh-call.txt");
    if cfg!(windows) {
        std::fs::write(
            &fake,
            format!("@echo off\r\necho %* > \"{0}\"\r\necho token=%GH_TOKEN% host=%GH_HOST%>> \"{0}\"\r\necho https://github.com/acme/app/pull/7\r\n", record.display()),
        )
        .unwrap();
    } else {
        std::fs::write(
            &fake,
            format!("#!/bin/sh\necho \"$@\" > '{0}'\necho \"token=$GH_TOKEN host=$GH_HOST\" >> '{0}'\necho https://github.com/acme/app/pull/7\n", record.display()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    // SAFETY: this test binary has a single test, so nothing reads the environment concurrently.
    unsafe { std::env::set_var("P2PAGENTS_GH", &fake) };
    let pr = creds
        .create_pr(&repo, "p2p/boss/1-1", "Add the feature", "Made by agents.")
        .await
        .unwrap();
    assert_eq!(pr, "https://github.com/acme/app/pull/7");
    let call = std::fs::read_to_string(&record).unwrap();
    for part in [
        "pr create",
        "--repo github.com/acme/app",
        "--base main",
        "--head p2p/boss/1-1",
        "token=t0k3n-never-stored",
        "host=github.com",
    ] {
        assert!(call.contains(part), "{part:?} in {call}");
    }

    let shares = unlink(&profile, "app", true).unwrap();
    assert!(shares.get("app").is_none() && !repo.path.exists());
    assert!(
        RepoStore::load(&profile.repos_path())
            .unwrap()
            .repos
            .is_empty()
    );
}

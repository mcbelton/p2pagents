//! Its own test binary because it changes this process's environment.

use proofkin::Profile;
use proofkin::Provider;
use proofkin::profile::SecretStore;

#[test]
fn file_secret_store_keeps_identity_and_tokens_in_the_profile() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: this binary has a single test, so nothing reads the environment concurrently.
    // The settings' names from before the rename still apply.
    unsafe {
        std::env::set_var("P2PAGENTS_HOME", home.path());
        std::env::set_var("P2PAGENTS_SECRET_STORE", "file");
    }
    assert_eq!(SecretStore::current(), SecretStore::Files);

    let profile = Profile::open("alice").unwrap();
    let id = profile.identity().unwrap().public();
    assert_eq!(
        profile.identity().unwrap().public(),
        id,
        "identity is stable"
    );
    let key_file = home.path().join("profiles/alice/secrets/identity");
    assert!(key_file.is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&key_file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    assert!(profile.secret(Provider::Claude).is_err());
    profile.set_secret(Provider::Claude, "sk-test").unwrap();
    assert_eq!(profile.secret(Provider::Claude).unwrap(), "sk-test");
    profile.delete_secret(Provider::Claude).unwrap();
    assert!(profile.secret(Provider::Claude).is_err());

    // The integration token for other agents is a secret like these, gone with the profile.
    assert_eq!(profile.integration_token().unwrap(), None);
    let token = profile.new_integration_token().unwrap();
    assert_eq!(profile.integration_token().unwrap(), Some(token.clone()));
    assert_ne!(profile.new_integration_token().unwrap(), token, "a new one");
    assert!(
        home.path()
            .join("profiles/alice/secrets/integration-token")
            .is_file()
    );

    profile.delete().unwrap();
    assert!(!home.path().join("profiles/alice").exists());
    assert_ne!(
        Profile::open("alice").unwrap().identity().unwrap().public(),
        id,
        "deleted identity is gone"
    );
}

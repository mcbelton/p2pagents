//! Its own test binary because it changes this process's environment.

use p2pagents::Profile;
use p2pagents::Provider;
use p2pagents::profile::SecretStore;

#[test]
fn file_secret_store_keeps_identity_and_tokens_in_the_profile() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: this binary has a single test, so nothing reads the environment concurrently.
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

    profile.delete().unwrap();
    assert!(!home.path().join("profiles/alice").exists());
    assert_ne!(
        Profile::open("alice").unwrap().identity().unwrap().public(),
        id,
        "deleted identity is gone"
    );
}

use crate::experts::catalog_tests::setup;

#[test]
fn broken_and_oversized_state_is_never_treated_as_an_empty_slot() {
    let (_directory, catalog, candidate, activation) = setup();
    let path = catalog.activations.path(&candidate.identity);
    for bytes in [b"not JSON".to_vec(), vec![b' '; 64 * 1024 + 1]] {
        std::fs::write(&path, &bytes).unwrap();
        assert!(catalog.activations.state(&candidate.identity).is_err());
        assert!(catalog.activations.commit(&activation, None).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[cfg(unix)]
#[test]
fn linked_state_and_lock_files_are_not_followed_or_replaced() {
    use std::os::unix::fs::symlink;
    let (directory, catalog, candidate, activation) = setup();
    let path = catalog.activations.path(&candidate.identity);
    let original = std::fs::read(&path).unwrap();
    let outside = directory.path().join("outside.json");
    std::fs::write(&outside, &original).unwrap();
    std::fs::remove_file(&path).unwrap();
    symlink(&outside, &path).unwrap();
    assert!(catalog.activations.state(&candidate.identity).is_err());
    assert!(catalog.activations.commit(&activation, None).is_err());
    assert!(std::fs::symlink_metadata(&path).unwrap().is_symlink());
    assert_eq!(std::fs::read(&outside).unwrap(), original);
    let lock = path.with_extension("lock");
    std::fs::remove_file(&lock).unwrap();
    symlink(&outside, &lock).unwrap();
    assert!(catalog.activations.lock(&candidate.identity).is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), original);
}

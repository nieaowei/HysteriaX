use super::*;
#[test]
fn restart_cleanup_preserves_other_live_and_paused_instances_and_removes_orphans() {
    let root = WorkDir(
        std::env::temp_dir().join(format!("hx-probe-cleanup-test-{}", Uuid::new_v4())),
        None,
    );
    std::fs::create_dir(&root.0).unwrap();
    let instance = Uuid::new_v4().to_string();
    let live = WorkDir::create(&root.0, "owned-", &instance).unwrap();
    let other = WorkDir::create(&root.0, "owned-", &Uuid::new_v4().to_string()).unwrap();
    let mut crashed = WorkDir::create(&root.0, "owned-", &instance).unwrap();
    let orphan = crashed.0.clone();
    // Model a terminated process: release the OS lock without running Drop.
    drop(crashed.1.take());
    std::mem::forget(crashed);
    let unrelated = root.0.join(format!("other-{}", Uuid::new_v4()));
    let legacy = root.0.join(format!("owned-{}", Uuid::new_v4()));
    for path in [&unrelated, &legacy] {
        std::fs::create_dir(path).unwrap();
    }
    cleanup_probe_directories_in(&root.0, "owned-").unwrap();
    assert!(!orphan.exists());
    assert!(live.0.exists());
    assert!(other.0.exists());
    assert!(unrelated.exists());
    assert!(legacy.exists());
    // A paused live process has the same lock behavior as the two live dirs.
    std::fs::write(other.0.join("client.key"), "still owned").unwrap();
}

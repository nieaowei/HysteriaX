use super::*;
#[test]
fn online_only_counts_known_users() {
    let ids = BTreeSet::from(["u".into()]);
    assert_eq!(
        real_connections(json!({"u":2,"monitor-node":1,"unknown":5}), &ids).unwrap(),
        BTreeMap::from([("u".into(), 2)])
    );
    assert!(real_connections(json!({"u":-1}), &ids).is_none());
}
#[test]
fn probe_requires_failure_and_recovery_thresholds() {
    let mut state = ("checking", 0, 0);
    for _ in 0..2 {
        state = next_probe_health(state.0, state.1, state.2, "failed");
        assert_ne!(state.0, "failed");
    }
    state = next_probe_health(state.0, state.1, state.2, "failed");
    assert_eq!(state.0, "failed");
    for _ in 0..20 {
        state = next_probe_health(state.0, state.1, state.2, "ok");
        assert_eq!(state.0, "failed");
        state = next_probe_health(state.0, state.1, state.2, "failed");
    }
    state = next_probe_health(state.0, state.1, state.2, "ok");
    state = next_probe_health(state.0, state.1, state.2, "ok");
    assert_eq!(state.0, "healthy");
}

use super::*;

#[test]
fn environment_child_fixture() {
    if std::env::var_os("PHI_PYTHON_LEASE_FIXTURE").is_some() {
        assert_eq!(
            std::env::var("PHI_PYTHON_LEASE_VALUE").unwrap(),
            "host-selected"
        );
        println!("lease environment reached child");
    }
}

#[tokio::test]
async fn lease_settings_reach_child_without_mutating_parent_environment() {
    let original = std::env::var_os("PHI_PYTHON_LEASE_VALUE");
    let directory = tempfile::tempdir().unwrap();
    let lease = PythonLease::new(std::env::current_exe().unwrap(), ()).with_environment(
        [
            ("PHI_PYTHON_LEASE_FIXTURE".into(), "1".into()),
            ("PHI_PYTHON_LEASE_VALUE".into(), "host-selected".into()),
        ]
        .into(),
    );
    let mut request = ProcessRequest::new(
        lease.interpreter().to_path_buf(),
        directory.path().to_path_buf(),
        Duration::from_secs(10),
        4096,
    );
    request.arguments = vec![
        "--exact".into(),
        "execution::lease_tests::environment_child_fixture".into(),
        "--nocapture".into(),
    ];
    request
        .environment
        .insert("PHI_PYTHON_LEASE_VALUE".into(), "wrong-value".into());
    lease.apply_environment(&mut request);
    let result = ProcessSupervisor
        .run(request, &TurnCancel::new())
        .await
        .unwrap();
    assert!(
        result.success,
        "{}{}",
        result.stdout.text, result.stderr.text
    );
    assert!(
        result
            .stdout
            .text
            .contains("lease environment reached child")
    );
    assert_eq!(std::env::var_os("PHI_PYTHON_LEASE_VALUE"), original);
}

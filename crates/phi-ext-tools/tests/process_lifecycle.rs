use phi_ext_tools::{ProcessRequest, ProcessSupervisor, ProcessTermination};
use phi_kernel::TurnCancel;
use std::{path::PathBuf, time::Duration};

#[test]
#[allow(clippy::zombie_processes)] // The supervisor, not this child fixture, must reap descendants.
fn child_fixture() {
    let Ok(mode) = std::env::var("PHI_PROCESS_FIXTURE") else {
        return;
    };
    match mode.as_str() {
        "invalid" => {
            std::io::Write::write_all(&mut std::io::stdout(), &[255; 2000]).unwrap();
        }
        "flood" => {
            for _ in 0..1000 {
                println!("{}", "界".repeat(1000));
                eprintln!("{}", "e".repeat(1000));
            }
        }
        "sleeper" => {
            std::thread::sleep(Duration::from_secs(2));
            std::fs::write(std::env::var_os("PHI_MARKER").unwrap(), "escaped").unwrap();
        }
        "tree" | "detach" => {
            let _child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "child_fixture", "--nocapture"])
                .env("PHI_PROCESS_FIXTURE", "sleeper")
                .spawn()
                .unwrap();
            std::fs::write(std::env::var_os("PHI_READY").unwrap(), "ready").unwrap();
            if mode == "tree" {
                std::thread::sleep(Duration::from_secs(20));
            }
        }
        _ => panic!("unknown fixture"),
    }
}

fn request(root: &std::path::Path, mode: &str, timeout: Duration) -> ProcessRequest {
    let mut request = ProcessRequest::new(
        std::env::current_exe().unwrap(),
        root.to_path_buf(),
        timeout,
        1024,
    );
    request.arguments = vec![
        "--exact".into(),
        "child_fixture".into(),
        "--nocapture".into(),
    ];
    request
        .environment
        .insert("PHI_PROCESS_FIXTURE".into(), mode.into());
    request
        .environment
        .insert("PHI_MARKER".into(), root.join("marker").into_os_string());
    request
        .environment
        .insert("PHI_READY".into(), root.join("ready").into_os_string());
    request
}

#[tokio::test]
async fn drains_output_without_unbounded_capture() {
    let root = tempfile::tempdir().unwrap();
    let result = ProcessSupervisor
        .run(
            request(root.path(), "flood", Duration::from_secs(10)),
            &TurnCancel::new(),
        )
        .await
        .unwrap();
    assert!(result.success);
    assert!(result.stdout.truncated);
    assert!(result.stderr.truncated);
    assert!(result.stdout.text.len() <= 1024);
    assert!(!result.stdout.invalid_utf8);
    let invalid = ProcessSupervisor
        .run(
            request(root.path(), "invalid", Duration::from_secs(10)),
            &TurnCancel::new(),
        )
        .await
        .unwrap();
    assert!(invalid.success);
    assert!(invalid.stdout.invalid_utf8);
    assert!(invalid.stdout.truncated);
    assert!(invalid.stdout.text.len() <= 1024);
}

#[tokio::test]
async fn cancellation_reaps_the_tree_before_returning() {
    let root = tempfile::tempdir().unwrap();
    let cancel = TurnCancel::new();
    let signal = cancel.clone();
    let ready: PathBuf = root.path().join("ready");
    let stopper = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !ready.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        signal.cancel();
    });
    let result = ProcessSupervisor
        .run(
            request(root.path(), "tree", Duration::from_secs(10)),
            &cancel,
        )
        .await
        .unwrap();
    stopper.await.unwrap();
    assert_eq!(result.termination, ProcessTermination::Cancelled);
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert!(!root.path().join("marker").exists());
}

#[tokio::test]
async fn normal_leader_exit_also_stops_remaining_children() {
    let root = tempfile::tempdir().unwrap();
    let result = ProcessSupervisor
        .run(
            request(root.path(), "detach", Duration::from_secs(10)),
            &TurnCancel::new(),
        )
        .await
        .unwrap();
    assert!(result.success);
    assert!(result.elapsed < Duration::from_secs(2));
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert!(!root.path().join("marker").exists());
}

#[tokio::test]
async fn deadline_kills_a_sleeping_tree() {
    let root = tempfile::tempdir().unwrap();
    let result = ProcessSupervisor
        .run(
            request(root.path(), "tree", Duration::from_millis(200)),
            &TurnCancel::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.termination, ProcessTermination::TimedOut);
    assert!(result.elapsed < Duration::from_secs(2));
}

#[tokio::test]
async fn dropping_the_supervisor_future_still_kills_its_group() {
    let root = tempfile::tempdir().unwrap();
    let process = request(root.path(), "tree", Duration::from_secs(10));
    let running =
        tokio::spawn(async move { ProcessSupervisor.run(process, &TurnCancel::new()).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !root.path().join("ready").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    running.abort();
    let _ = running.await;
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert!(!root.path().join("marker").exists());
}

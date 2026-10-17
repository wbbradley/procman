use std::process::{Command, Output};

fn run(dir: &tempfile::TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_procman"))
        .current_dir(dir.path())
        .arg("root.pman")
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn all_tasks_waits_for_root_imported_and_fan_out_tasks_after_failure() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("root.pman"),
        r#"
        import "child.pman" as child
        job setup { run "touch ready" }
        service support { run "sleep 3" }
        task fails {
            wait { after @setup }
            run "exit 7"
        }
        task finishes {
            wait { after @setup }
            run "sleep 0.2; echo done >> finished"
        }
        event dormant { run "touch event-ran" }
        "#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("child.pman"),
        r#"
        task imported { run "sleep 0.3; touch imported-ran" }
        task batch {
            for item in ["a", "b"] {
                env ITEM = item
                run "sleep 0.4; touch batch-$ITEM"
            }
        }
        "#,
    )
    .unwrap();

    let output = run(&dir, &["--all-tasks", "-t", "finishes"]);
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("finished")).unwrap(),
        "done\n"
    );
    for marker in ["ready", "imported-ran", "batch-a", "batch-b"] {
        assert!(dir.path().join(marker).exists(), "missing {marker}");
    }
    assert!(!dir.path().join("event-ran").exists());
}

#[test]
fn all_tasks_succeeds_when_every_task_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("root.pman"),
        r#"
        service support { run "sleep 3" }
        task first { run "touch first-ran" }
        task second { run "sleep 0.1; touch second-ran" }
        "#,
    )
    .unwrap();
    let output = run(&dir, &["--all-tasks"]);
    assert!(output.status.success(), "{output:?}");
    assert!(dir.path().join("first-ran").exists());
    assert!(dir.path().join("second-ran").exists());
}

#[test]
fn tasks_remain_dormant_without_selection() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("root.pman"),
        r#"
        job setup { run "touch ready" }
        task dormant { run "touch task-ran" }
        "#,
    )
    .unwrap();
    let output = run(&dir, &[]);
    assert!(output.status.success(), "{output:?}");
    assert!(dir.path().join("ready").exists());
    assert!(!dir.path().join("task-ran").exists());
}

#[test]
fn all_tasks_without_tasks_preserves_job_behavior() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("root.pman"),
        r#"job setup { run "touch ready" }"#,
    )
    .unwrap();
    let output = run(&dir, &["--all-tasks"]);
    assert!(output.status.success(), "{output:?}");
    assert!(dir.path().join("ready").exists());
}

#[test]
fn check_with_all_tasks_does_not_start_processes_and_validates_named_tasks() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("root.pman"),
        r#"task check_me { run "touch task-ran" }"#,
    )
    .unwrap();
    let output = run(&dir, &["--all-tasks", "--check"]);
    assert!(output.status.success(), "{output:?}");
    assert!(!dir.path().join("task-ran").exists());

    let output = run(&dir, &["--all-tasks", "--check", "-t", "missing"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown task 'missing'"));
}

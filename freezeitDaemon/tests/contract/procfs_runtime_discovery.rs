use std::{fs, os::unix::fs::PermissionsExt};

use freezeit_daemon::sys::procfs::{
    discover_managed_uid_processes_with_cgroup_roots, discover_package_processes,
    discover_uid_processes_with_cgroup_root, discover_uid_processes_with_cgroup_roots,
    recheck_process_identity,
};

#[test]
fn discovers_package_processes_by_pid_uid_and_cmdline() {
    let temp = tempfile::tempdir().expect("tempdir");
    let proc_root = temp.path();
    let pid_dir = proc_root.join("123");
    fs::create_dir(&pid_dir).expect("pid dir");
    fs::write(
        pid_dir.join("status"),
        "Name:\texample\nUid:\t10123\t10123\t10123\t10123\nvoluntary_ctxt_switches:\t7\nnonvoluntary_ctxt_switches:\t3\n",
    )
    .expect("status");
    fs::write(pid_dir.join("cmdline"), "com.example.app:remote\0").expect("cmdline");

    let ignored = proc_root.join("not-a-pid");
    fs::create_dir(&ignored).expect("ignored dir");
    fs::set_permissions(&ignored, fs::Permissions::from_mode(0o755)).expect("permissions");

    let processes =
        discover_package_processes(proc_root, "com.example.app", 10_123).expect("discover");

    assert_eq!(processes.len(), 1);
    assert_eq!(processes[0].pid, 123);
    assert_eq!(processes[0].process_name, "com.example.app:remote");
}

#[test]
fn discovers_uid_processes_with_cgroup_freeze_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    let proc_root = temp.path().join("proc");
    let cgroup_root = temp.path().join("cgroup");
    let pid_dir = proc_root.join("123");
    fs::create_dir_all(&pid_dir).expect("pid dir");
    fs::write(
        pid_dir.join("status"),
        "Name:\texample\nUid:\t10123\t10123\t10123\t10123\nvoluntary_ctxt_switches:\t7\nnonvoluntary_ctxt_switches:\t3\n",
    )
    .expect("status");
    fs::write(pid_dir.join("cmdline"), "com.example.app:remote\0").expect("cmdline");

    let freeze_file = cgroup_root
        .join("uid_10123")
        .join("pid_123")
        .join("cgroup.freeze");
    fs::create_dir_all(freeze_file.parent().unwrap()).expect("freeze dir");
    fs::write(&freeze_file, "0").expect("freeze file");

    let processes = discover_uid_processes_with_cgroup_root(&proc_root, &cgroup_root, 10_123)
        .expect("discover uid");

    assert_eq!(processes.len(), 1);
    assert_eq!(processes[0].package_name, "com.example.app");
    assert_eq!(
        processes[0].cgroup_freeze_path.as_deref(),
        Some(freeze_file.to_str().unwrap())
    );
    assert_eq!(
        processes[0].binder_state.as_deref(),
        Some("context_switches voluntary=7 nonvoluntary=3 total=10")
    );
}

#[test]
fn discovers_uid_processes_with_app_cgroup_root_before_system_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let proc_root = temp.path().join("proc");
    let app_cgroup_root = temp.path().join("cgroup").join("apps");
    let system_cgroup_root = temp.path().join("cgroup").join("system");
    let pid_dir = proc_root.join("123");
    fs::create_dir_all(&pid_dir).expect("pid dir");
    fs::write(
        pid_dir.join("status"),
        "Name:\texample\nUid:\t10123\t10123\t10123\t10123\nvoluntary_ctxt_switches:\t7\nnonvoluntary_ctxt_switches:\t3\n",
    )
    .expect("status");
    fs::write(pid_dir.join("cmdline"), "com.example.app\0").expect("cmdline");

    let app_freeze_file = app_cgroup_root
        .join("uid_10123")
        .join("pid_123")
        .join("cgroup.freeze");
    fs::create_dir_all(app_freeze_file.parent().unwrap()).expect("app freeze dir");
    fs::write(&app_freeze_file, "0").expect("app freeze file");

    let system_freeze_file = system_cgroup_root
        .join("uid_10123")
        .join("pid_123")
        .join("cgroup.freeze");
    fs::create_dir_all(system_freeze_file.parent().unwrap()).expect("system freeze dir");
    fs::write(&system_freeze_file, "0").expect("system freeze file");

    let processes = discover_uid_processes_with_cgroup_roots(
        &proc_root,
        &[app_cgroup_root.as_path(), system_cgroup_root.as_path()],
        10_123,
    )
    .expect("discover uid");

    assert_eq!(processes.len(), 1);
    assert_eq!(
        processes[0].cgroup_freeze_path.as_deref(),
        Some(app_freeze_file.to_str().unwrap())
    );
}

#[test]
fn discovers_multiple_managed_uid_processes_with_one_procfs_scan() {
    let temp = tempfile::tempdir().expect("tempdir");
    let proc_root = temp.path().join("proc");
    let cgroup_root = temp.path().join("cgroup").join("apps");

    write_proc_entry(&proc_root, 123, 10_123, "com.example.app:remote");
    write_proc_entry(&proc_root, 456, 10_456, "com.example.other");
    write_proc_entry(&proc_root, 789, 10_789, "com.example.ignored");

    let freeze_file = cgroup_root
        .join("uid_10123")
        .join("pid_123")
        .join("cgroup.freeze");
    fs::create_dir_all(freeze_file.parent().unwrap()).expect("freeze dir");
    fs::write(&freeze_file, "0").expect("freeze file");

    let managed_uids = [10_123, 10_456].into_iter().collect();
    let processes = discover_managed_uid_processes_with_cgroup_roots(
        &proc_root,
        &[cgroup_root.as_path()],
        &managed_uids,
    )
    .expect("discover managed uids");

    assert_eq!(processes.len(), 2);
    assert_eq!(processes[&10_123][0].process_name, "com.example.app:remote");
    assert_eq!(processes[&10_123][0].package_name, "com.example.app");
    assert_eq!(
        processes[&10_123][0].cgroup_freeze_path.as_deref(),
        Some(freeze_file.to_str().unwrap())
    );
    assert_eq!(processes[&10_456][0].process_name, "com.example.other");
    assert!(!processes.contains_key(&10_789));
}

#[test]
fn process_identity_recheck_requires_uid_cmdline_package_and_start_time() {
    let temp = tempfile::tempdir().expect("tempdir");
    let proc_root = temp.path().join("proc");
    write_proc_entry(&proc_root, 123, 10_123, "com.example.app:remote");
    let process = discover_package_processes(&proc_root, "com.example.app", 10_123)
        .expect("discover")
        .remove(0);

    assert!(recheck_process_identity(&proc_root, &process).expect("identity matches"));

    fs::write(proc_root.join("123/cmdline"), "com.other.shared\0").expect("replace cmdline");
    assert!(!recheck_process_identity(&proc_root, &process).expect("cmdline mismatch"));

    fs::write(proc_root.join("123/cmdline"), "com.example.app:remote\0").expect("restore cmdline");
    write_stat(&proc_root.join("123/stat"), 9999);
    assert!(!recheck_process_identity(&proc_root, &process).expect("start time mismatch"));
}

fn write_proc_entry(proc_root: &std::path::Path, pid: i32, uid: u32, cmdline: &str) {
    let pid_dir = proc_root.join(pid.to_string());
    fs::create_dir_all(&pid_dir).expect("pid dir");
    fs::write(
        pid_dir.join("status"),
        format!("Name:\texample\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\n"),
    )
    .expect("status");
    fs::write(pid_dir.join("cmdline"), format!("{cmdline}\0")).expect("cmdline");
    write_stat(&pid_dir.join("stat"), 4242);
}

fn write_stat(path: &std::path::Path, start_time: u64) {
    let mut fields = vec!["0"; 20];
    fields[0] = "S";
    let start_time = start_time.to_string();
    fields[19] = &start_time;
    fs::write(path, format!("123 (example worker) {}\n", fields.join(" "))).expect("stat");
}

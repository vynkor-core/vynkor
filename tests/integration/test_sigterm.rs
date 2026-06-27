use std::fs;
use std::path::Path;
use std::time::Duration;

const VYN: &str = env!("CARGO_BIN_EXE_vyn");

fn write_test_config(cfg_path: &str, socket: &str, port: u16, pid_file: &str) {
    fs::write(
        cfg_path,
        format!(
            "port: {port}\n\
             log_level: info\n\
             pid_file: {pid_file}\n\
             log_file: /tmp/veyron_sigterm_test.log\n\
             data_dir: /tmp/veyron_sigterm_test_data\n\
             socket_path: {socket}\n\
             allow_no_auth: true\n"
        ),
    )
    .unwrap();
}

#[test]
fn kernel_exits_cleanly_on_sigterm() {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;

    let cfg_path = "/tmp/veyron_sigterm_cfg.yaml";
    let socket = "/tmp/veyron_sigterm_test.sock";
    let pid_file = "/tmp/veyron_sigterm_test.pid";
    write_test_config(cfg_path, socket, 19301, pid_file);

    // Remove stale socket/pid from a previous failed run
    let _ = fs::remove_file(socket);
    let _ = fs::remove_file(pid_file);

    let mut child = std::process::Command::new(VYN)
        .args(["start", "--foreground", "--config", cfg_path])
        .spawn()
        .expect("failed to spawn vyn binary");

    // Wait up to 5 s for the UDS socket to appear (kernel ready)
    let ready = (0..100).any(|_| {
        std::thread::sleep(Duration::from_millis(50));
        Path::new(socket).exists()
    });
    assert!(
        ready,
        "kernel UDS socket never appeared — vyn start may have failed"
    );

    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).unwrap();

    // Kernel must exit within 5 s of SIGTERM (graceful shutdown)
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait().unwrap() {
            Some(s) => break s,
            None if start.elapsed() > Duration::from_secs(5) => {
                child.kill().unwrap();
                panic!("vyn did not exit within 5 s after SIGTERM");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };

    assert!(
        status.success(),
        "vyn must exit with code 0 after SIGTERM, got: {status}"
    );

    let _ = fs::remove_file(cfg_path);
    let _ = fs::remove_file(pid_file);
}

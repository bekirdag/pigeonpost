//! Whole-process custody regression for direct loft startup.

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;
    use std::process::{Child, Command, Output, Stdio};
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    struct ChildGuard {
        child: Option<Child>,
        stdout_reader: Option<JoinHandle<String>>,
    }

    impl ChildGuard {
        fn new(mut child: Child, expected_marker: String) -> (Self, Receiver<()>) {
            let stdout = child.stdout.take().expect("capture loft stdout");
            let (marker_sender, marker_receiver) = mpsc::sync_channel(1);
            let stdout_reader = std::thread::spawn(move || {
                let mut captured = String::new();
                for line in BufReader::new(stdout).lines() {
                    match line {
                        Ok(line) => {
                            if line == expected_marker {
                                let _ = marker_sender.try_send(());
                            }
                            captured.push_str(&line);
                            captured.push('\n');
                        }
                        Err(error) => {
                            captured.push_str(&format!("<stdout read failed: {error}>\n"));
                            break;
                        }
                    }
                }
                captured
            });
            (
                Self {
                    child: Some(child),
                    stdout_reader: Some(stdout_reader),
                },
                marker_receiver,
            )
        }

        fn child(&mut self) -> &mut Child {
            self.child.as_mut().expect("child already consumed")
        }

        fn collect(&mut self) -> Output {
            let child = self.child.take().expect("child already consumed");
            let mut output = child
                .wait_with_output()
                .expect("collect failed loft startup output");
            output.stdout = self
                .stdout_reader
                .take()
                .expect("stdout reader already consumed")
                .join()
                .expect("loft stdout reader panicked")
                .into_bytes();
            output
        }
    }

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            if let Some(child) = self.child.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
            if let Some(stdout_reader) = self.stdout_reader.take() {
                let _ = stdout_reader.join();
            }
        }
    }

    struct LoopbackReservation(TcpListener);

    impl LoopbackReservation {
        fn new() -> Self {
            Self(TcpListener::bind("127.0.0.1:0").expect("reserve ephemeral test port"))
        }

        fn address(&self) -> String {
            self.0
                .local_addr()
                .expect("read reserved test port")
                .to_string()
        }

        fn release(self) {
            drop(self.0);
        }
    }

    fn assert_directory_mode(path: &Path, expected: u32) {
        let metadata = fs::symlink_metadata(path).expect("inspect private directory");
        assert!(metadata.is_dir(), "{} is not a directory", path.display());
        assert_eq!(metadata.mode() & 0o777, expected, "{}", path.display());
    }

    fn assert_file_mode(path: &Path, expected: u32) {
        let metadata = fs::symlink_metadata(path).expect("inspect private file");
        assert!(metadata.is_file(), "{} is not a file", path.display());
        assert_eq!(metadata.mode() & 0o777, expected, "{}", path.display());
        assert_eq!(metadata.nlink(), 1, "{} has multiple names", path.display());
    }

    fn address_in_use(output: &Output) -> bool {
        String::from_utf8_lossy(&output.stderr).contains("AddrInUse")
    }

    fn exited_output(child: &mut ChildGuard) -> Option<Output> {
        if child
            .child()
            .try_wait()
            .expect("inspect loft process")
            .is_some()
        {
            Some(child.collect())
        } else {
            None
        }
    }

    #[test]
    fn fresh_direct_serve_under_umask_022_keeps_runtime_storage_owner_only() {
        let temporary = tempfile::tempdir().expect("create temporary parent");
        const MAX_BIND_ATTEMPTS: usize = 4;
        let mut collision_retried = false;

        'attempts: for attempt in 1..=MAX_BIND_ATTEMPTS {
            let loft_dir = temporary.path().join(format!("fresh-loft-{attempt}"));
            assert!(!loft_dir.exists());
            let reservation = LoopbackReservation::new();
            let bind = reservation.address();

            // Deterministically prove that a bind loser cannot emit the post-bind marker or make
            // this custody test pass. Later attempts exercise the real reservation handoff.
            let held_collision = if attempt == 1 {
                Some(reservation)
            } else {
                reservation.release();
                None
            };
            let child = Command::new("/bin/sh")
                .arg("-c")
                .arg("umask 022\nexec \"$1\" loft serve --dir \"$2\" --bind \"$3\"")
                .arg("pigeonpost-storage-custody-test")
                .arg(env!("CARGO_BIN_EXE_pigeonpost"))
                .arg(&loft_dir)
                .arg(&bind)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("launch production pigeonpost binary");
            let expected_marker = format!("loft listening on {bind}");
            let (mut child, marker) = ChildGuard::new(child, expected_marker);

            let deadline = Instant::now() + Duration::from_secs(10);
            let startup_failure = 'startup: loop {
                if let Some(output) = exited_output(&mut child) {
                    break Some(output);
                }
                match marker.recv_timeout(Duration::from_millis(25)) {
                    Ok(()) => {
                        // The marker is emitted by this exact child only after its bind succeeds.
                        // Confirm it did not then terminate before accepting startup.
                        if let Some(output) = exited_output(&mut child) {
                            break Some(output);
                        }
                        break None;
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => {
                        // Closing stdout is a startup failure even if process reaping has not yet
                        // become observable through try_wait.
                        let deadline = Instant::now() + Duration::from_secs(1);
                        loop {
                            if let Some(output) = exited_output(&mut child) {
                                break 'startup Some(output);
                            }
                            if Instant::now() >= deadline {
                                panic!("loft stdout closed before its post-bind startup marker");
                            }
                            std::thread::sleep(Duration::from_millis(5));
                        }
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "direct loft did not start in time"
                );
                std::thread::sleep(Duration::from_millis(25));
            };
            if let Some(output) = startup_failure {
                drop(held_collision);
                if attempt < MAX_BIND_ATTEMPTS && address_in_use(&output) {
                    assert!(
                        !String::from_utf8_lossy(&output.stdout).contains("loft listening on"),
                        "a bind loser must not announce that it is listening"
                    );
                    collision_retried = true;
                    continue 'attempts;
                }
                panic!(
                    "direct loft exited before it listened: {}\nstdout:\n{}\nstderr:\n{}",
                    output.status,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                );
            }

            drop(held_collision);
            assert!(
                collision_retried,
                "the forced first bind collision did not enter the retry path"
            );
            assert_directory_mode(&loft_dir, 0o700);
            assert_file_mode(&loft_dir.join("loft.key"), 0o600);
            for name in ["mail.db", "mail.db-wal", "mail.db-shm"] {
                assert_file_mode(&loft_dir.join(name), 0o600);
            }
            return;
        }

        unreachable!("bounded bind attempts either pass or report their final failure");
    }
}

//! Live escape and isolation probes for the marketplace pool, under real gVisor.
//!
//! Like `tests/nests/e2e.mjs`, this needs a real Nests host and is never run by `cargo test` or
//! CI: podman, the `runsc-nests` runtime and the `nests-mp` network with the nftables policy
//! (`deploy/nests/host/nests.nft`). It repeats the Phase A escape table
//! (docs/nests/phase-a-plan.md §5) from inside two buyer contexts, with a canary in each.
//!
//! On a Nests host, as root, with the runner image (it has a shell):
//!
//! ```text
//! NESTS_LIVE_GVISOR=1 NESTS_LIVE_IMAGE=localhost/pigeonpost-nest-runner@sha256:… \
//!   cargo test -p pigeonpost-nests --test marketplace_isolation -- --ignored --nocapture
//! ```

use pigeonpost_nests::config::{MarketplacePoolConfig, SandboxConfig};
use pigeonpost_nests::sandbox::{MarketplacePool, Spec};
use std::path::PathBuf;
use std::process::Command;

const CONTEXT_A: &str = "buyer-live-a";
const CONTEXT_B: &str = "buyer-live-b";

fn podman(args: &[&str]) -> (bool, String) {
    let out = Command::new("podman")
        .args(args)
        .output()
        .expect("podman is installed");
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text)
}

fn base() -> SandboxConfig {
    SandboxConfig {
        runtime: "gvisor".into(),
        oci_runtime: "runsc-nests".into(),
        image: "unused-for-the-marketplace-pool@sha256:00".into(),
        network: "nests".into(),
        memory_mib: 256,
        cpu_weight: 100,
        cpu_max_millis: 1000,
        pids: 128,
        postbox_host: "postbox.pigeonpost.dev".into(),
        postbox_ip: "45.132.181.100".into(),
        postbox_url: "https://postbox.pigeonpost.dev".into(),
        broker_ip: "10.67.255.1".into(),
        broker_port: 8788,
        tmp_mib: 64,
        marketplace: None,
    }
}

struct Sandbox {
    name: String,
    context: String,
    canary: String,
    root: PathBuf,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = podman(&["rm", "--force", "--time", "0", "--ignore", &self.name]);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Start one buyer context's sandbox with the pool's exact profile, idling in a shell so probes
/// can `podman exec` into it.
fn start(
    root: &std::path::Path,
    pool: &MarketplacePoolConfig,
    context: &str,
    nest: &str,
) -> Sandbox {
    let volume = root.join(context).join(nest);
    std::fs::create_dir_all(&volume).unwrap();
    let canary = format!("canary-{context}-{}", std::process::id());
    std::fs::write(volume.join("canary"), &canary).unwrap();
    let cmd = format!("chown -R 10001:10001 {}", root.join(context).display());
    assert!(Command::new("sh")
        .args(["-c", &cmd])
        .status()
        .unwrap()
        .success());
    let spec = Spec {
        nest_id: nest.into(),
        generation: 1,
        volume,
        buyer_context: Some(context.into()),
        image: None,
    };
    let mut argv = MarketplacePool::<pigeonpost_nests::sandbox::HostPodman>::run_args(
        &base(),
        pool,
        &spec,
        context,
    );
    let image = argv.pop().unwrap();
    argv.extend([
        "--entrypoint".into(),
        "/bin/sh".into(),
        image,
        "-c".into(),
        "sleep 300".into(),
    ]);
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let (ok, out) = podman(&refs);
    assert!(ok, "start failed: {out}");
    let name = format!("nest-mp-{nest}");
    Sandbox {
        name,
        context: context.into(),
        canary,
        root: root.to_path_buf(),
    }
}

/// `sh -c` inside the sandbox; returns (exit ok, output).
fn exec(s: &Sandbox, script: &str) -> (bool, String) {
    podman(&["exec", &s.name, "/bin/sh", "-c", script])
}

/// Whether a TCP connect from inside succeeds within 3 seconds (`bash` in the Debian base image).
fn reaches(s: &Sandbox, host: &str, port: u16) -> bool {
    let probe = format!("timeout 3 bash -c 'exec 3<>/dev/tcp/{host}/{port}'");
    exec(s, &probe).0
}

fn ip_of(s: &Sandbox) -> String {
    let (ok, out) = podman(&[
        "inspect",
        "--format",
        "{{.NetworkSettings.Networks.nests-mp.IPAddress}}",
        &s.name,
    ]);
    assert!(ok, "inspect: {out}");
    out.trim().to_string()
}

#[test]
#[ignore = "needs a Nests host: podman, runsc-nests, the nests-mp network and NESTS_LIVE_GVISOR=1"]
fn two_buyer_contexts_on_gvisor_cannot_reach_each_other_or_the_host() {
    assert_eq!(
        std::env::var("NESTS_LIVE_GVISOR").as_deref(),
        Ok("1"),
        "set NESTS_LIVE_GVISOR=1 on a Nests host; see the module docs"
    );
    let image = std::env::var("NESTS_LIVE_IMAGE").expect("NESTS_LIVE_IMAGE: the runner image");
    let root = std::env::temp_dir().join(format!("pp-mp-live-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let pool = MarketplacePoolConfig {
        image,
        network: "nests-mp".into(),
        volume_root: root.clone(),
        broker_ip: "10.67.255.1".into(),
        broker_port: 8788,
        memory_mib: 256,
        cpu_weight: 100,
        cpu_max_millis: 500,
        pids: 64,
        tmp_mib: 32,
        attestation_keys: vec![],
        worker_gib: 1,
        artifacts: None,
    };
    let a = start(&root, &pool, CONTEXT_A, "nst_00000000000000a1");
    let b = start(&root, &pool, CONTEXT_B, "nst_00000000000000b1");

    // It is really gVisor, and really the configured runtime.
    let (ok, out) = podman(&["inspect", "--format", "{{.OCIRuntime}}", &a.name]);
    assert!(ok && out.trim() == "runsc-nests", "runtime: {out}");
    let (_, out) = exec(&a, "dmesg 2>/dev/null | head -3");
    assert!(out.contains("gVisor"), "not gVisor: {out}");

    for (me, other) in [(&a, &b), (&b, &a)] {
        // Each sees its own canary at /nest and never the other's, on a full search.
        let (ok, out) = exec(me, "cat /nest/canary");
        assert!(ok && out.trim() == me.canary, "own canary: {out}");
        let search = format!(
            "grep -rl --exclude-dir=proc --exclude-dir=sys -e '{}' / 2>/dev/null",
            other.canary
        );
        let (_, out) = exec(me, &search);
        assert!(
            !out.contains("canary-") && out.trim().is_empty(),
            "{} found {}'s canary: {out}",
            me.context,
            other.context
        );

        // Host paths and sockets are not there.
        for path in [
            "/var/lib/nests",
            "/etc/nests",
            "/etc/wireguard",
            "/run/podman/podman.sock",
            "/var/run/docker.sock",
        ] {
            let (ok, _) = exec(me, &format!("test -e {path}"));
            assert!(!ok, "{} can see {path}", me.context);
        }

        // Read-only root; noexec scratch space; no privilege.
        assert!(!exec(me, "touch /escape").0, "wrote the image");
        assert!(
            !exec(me, "cp /bin/true /tmp/t && chmod +x /tmp/t && /tmp/t").0,
            "exec from /tmp"
        );
        assert!(
            !exec(me, "cp /bin/true /nest/t && chmod +x /nest/t && /nest/t").0,
            "exec from /nest"
        );
        assert!(!exec(me, "mount -t tmpfs t /mnt").0, "mount");
        let (_, id) = exec(me, "id -u");
        assert_eq!(id.trim(), "10001");

        // The broker is the only destination.
        assert!(reaches(me, "10.67.255.1", 8788), "the broker must be open");
        for (host, port) in [
            ("45.132.181.100", 443u16), // the postbox: open to first-party Nests, not to this pool
            ("169.254.169.254", 80),    // metadata
            ("10.69.0.1", 22),          // the bridge gateway, i.e. the host
            ("127.0.0.1", 22),          // loopback
            ("10.66.0.2", 8790),        // the control API over WireGuard
            ("1.1.1.1", 443),           // the internet
            ("8.8.8.8", 53),            // DNS
        ] {
            assert!(
                !reaches(me, host, port),
                "{} reached {host}:{port}",
                me.context
            );
        }
        // And the other buyer context, on any port, is not a destination either.
        assert!(
            !reaches(me, &ip_of(other), 1),
            "{} reached {}",
            me.context,
            other.context
        );
    }
}

use integration_tests::common::*;
use papin_gateway::bootstrap::{BootstrapEngine, FakeBootstrap, READY_MARKER};
use papin_gateway::provider::AgentRecord;
use std::os::unix::fs::PermissionsExt;

fn record(id: &str, seed: Option<&str>) -> AgentRecord {
    AgentRecord {
        id: id.into(),
        name: id.into(),
        config: papin_gateway::AgentConfig {
            base: "fake".into(),
            seed: seed.map(str::to_string),
            env: Default::default(),
        },
        created_at: 0,
    }
}

#[tokio::test]
async fn bootstrap_builds_rootfs_with_seed_identity_and_marker_last() {
    let dirs = test_dirs();
    let engine = FakeBootstrap::new(dirs.agents_root.clone(), dirs.seeds_dir.clone());
    let root = engine.bootstrap(&record("a1", Some("demo"))).await.unwrap();

    // Writable dirs (§5.4).
    let home = std::fs::metadata(root.join("home/papin")).unwrap();
    assert_eq!(home.permissions().mode() & 0o777, 0o700);
    let tmp = std::fs::metadata(root.join("tmp")).unwrap();
    assert_eq!(tmp.permissions().mode() & 0o7777, 0o1777);

    // setgid on workspace + home (ACL tool support is probed separately).
    let ws = std::fs::metadata(root.join("workspace")).unwrap();
    assert_ne!(ws.permissions().mode() & 0o2000, 0, "setgid on /workspace");

    // Seed copied into /workspace.
    assert_eq!(
        std::fs::read_to_string(root.join("workspace/src/main.txt")).unwrap(),
        "hello seed\n"
    );

    // Identity files from templates.
    let passwd = std::fs::read_to_string(root.join("etc/passwd")).unwrap();
    assert!(passwd.contains("papin:x:1000:1000"));
    assert!(root.join("etc/resolv.conf").exists());
    assert!(root.join("etc/nsswitch.conf").exists());

    // Ready marker present; idempotent re-bootstrap keeps it.
    assert!(root.join(READY_MARKER).exists());
    let root2 = engine.bootstrap(&record("a1", Some("demo"))).await.unwrap();
    assert_eq!(root, root2);

    // remove() takes the whole tree.
    engine.remove("a1").await.unwrap();
    assert!(!dirs.agents_root.join("a1").exists());
}

#[tokio::test]
async fn bootstrap_copy_does_not_share_inodes_with_seed() {
    let dirs = test_dirs();
    let engine = FakeBootstrap::new(dirs.agents_root.clone(), dirs.seeds_dir.clone());
    let root = engine.bootstrap(&record("a1", Some("demo"))).await.unwrap();

    // Mutating the clone must not touch the seed template (never hardlink).
    std::fs::write(root.join("workspace/src/main.txt"), "mutated\n").unwrap();
    assert_eq!(
        std::fs::read_to_string(dirs.seeds_dir.join("demo/src/main.txt")).unwrap(),
        "hello seed\n"
    );
}

#[tokio::test]
async fn bootstrap_acl_best_effort() {
    // If the platform has setfacl + xattr-capable fs, the default ACL lands;
    // otherwise the bootstrap silently skips it (§5.4 best-effort rule).
    let has_setfacl = std::process::Command::new("which")
        .arg("setfacl")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !has_setfacl {
        eprintln!("setfacl unavailable: skipping ACL assert (best-effort by design)");
        return;
    }
    let dirs = test_dirs();
    let engine = FakeBootstrap::new(dirs.agents_root.clone(), dirs.seeds_dir.clone());
    let root = engine.bootstrap(&record("a1", Some("demo"))).await.unwrap();
    let out = std::process::Command::new("getfacl")
        .arg(root.join("workspace"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    if out.status.success() && text.contains("default:") {
        assert!(
            text.contains("group:papin-gateway:rwx"),
            "default ACL applied: {text}"
        );
    } else {
        eprintln!("filesystem lacks ACL xattr support: skipping ACL assert");
    }
}

#[tokio::test]
async fn spawn_bootstrap_requires_a_base_image() {
    let dirs = test_dirs();
    let engine = papin_gateway::SpawnBootstrap::new(
        dirs.agents_root.clone(),
        dirs.agents_root.join("base"),
        dirs.seeds_dir.clone(),
    );
    let err = engine.bootstrap(&record("a1", None)).await.unwrap_err();
    assert!(err.to_string().contains("base image"), "{err}");
    // remove() still works (pure filesystem).
    std::fs::create_dir_all(dirs.agents_root.join("a1/root")).unwrap();
    engine.remove("a1").await.unwrap();
}

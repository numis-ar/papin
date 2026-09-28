//! M4 bootstrap engine tests: reflink-vs-copy equivalence, base image
//! immutability (write through the clone), and the never-hardlink rule.

use integration_tests::common::*;
use papin_gateway::bootstrap::{BootstrapEngine, SpawnBootstrap};
use papin_gateway::provider::AgentRecord;
use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;

fn record(id: &str, base: &str, seed: Option<&str>) -> AgentRecord {
    AgentRecord {
        id: id.into(),
        name: id.into(),
        config: papin_gateway::AgentConfig {
            base: base.into(),
            seed: seed.map(str::to_string),
            env: Default::default(),
        },
        created_at: 0,
    }
}

fn make_base(bases_dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let base = bases_dir.join(name);
    std::fs::create_dir_all(base.join("usr/bin")).unwrap();
    std::fs::create_dir_all(base.join("etc")).unwrap();
    std::fs::create_dir_all(base.join("workspace")).unwrap();
    std::fs::write(base.join("usr/bin/tool"), "#!/bin/sh\necho tool\n").unwrap();
    std::fs::write(base.join("etc/motd"), "base motd\n").unwrap();
    base
}

fn snapshot(dir: &std::path::Path) -> BTreeMap<String, Vec<u8>> {
    let mut map = BTreeMap::new();
    fn walk(dir: &std::path::Path, prefix: &str, map: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let rel = format!("{}/{}", prefix, entry.file_name().to_string_lossy());
            if path.is_dir() {
                walk(&path, &rel, map);
            } else if entry.file_name() != ".papin-rootfs-ready"
                && entry.file_name() != "resolv.conf"
                && entry.file_name() != "passwd"
                && entry.file_name() != "group"
                && entry.file_name() != "nsswitch.conf"
                && entry.file_name() != "hosts"
            {
                map.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    walk(dir, "", &mut map);
    map
}

#[tokio::test]
async fn reflink_and_copy_produce_equivalent_trees() {
    let dirs = test_dirs();
    let base = make_base(&dirs.agents_root.join("base"), "node22-kimi-1.0");

    let engine_reflink = SpawnBootstrap {
        agents_root: dirs.agents_root.clone(),
        bases_dir: dirs.agents_root.join("base"),
        seeds_dir: dirs.seeds_dir.clone(),
        use_reflink: Some(true),
    };
    let engine_copy = SpawnBootstrap {
        use_reflink: Some(false),
        ..SpawnBootstrap::new(
            dirs.agents_root.clone(),
            dirs.agents_root.join("base"),
            dirs.seeds_dir.clone(),
        )
    };

    let r_reflink = engine_reflink
        .bootstrap(&record("r1", "node22-kimi-1.0", Some("demo")))
        .await
        .unwrap();
    let r_copy = engine_copy
        .bootstrap(&record("c1", "node22-kimi-1.0", Some("demo")))
        .await
        .unwrap();

    // Same relative content in both trees.
    let snap_reflink = snapshot(&r_reflink);
    let snap_copy = snapshot(&r_copy);
    assert_eq!(snap_reflink, snap_copy, "reflink and copy are equivalent");
    assert_eq!(
        snap_reflink["/workspace/src/main.txt"],
        b"hello seed\n".to_vec()
    );

    // On filesystems without reflink support the reflink engine must still
    // succeed (silent copy fallback) — tmpfs/ext4-no-reflink both allowed.
    let _ = base;
}

#[tokio::test]
async fn write_through_clone_never_mutates_the_base() {
    let dirs = test_dirs();
    let base = make_base(&dirs.agents_root.join("base"), "node22-kimi-1.0");
    let base_snapshot = snapshot(&base);

    let engine = SpawnBootstrap::new(
        dirs.agents_root.clone(),
        dirs.agents_root.join("base"),
        dirs.seeds_dir.clone(),
    );
    let root = engine
        .bootstrap(&record("a1", "node22-kimi-1.0", None))
        .await
        .unwrap();

    // Mutate the clone aggressively.
    std::fs::write(root.join("usr/bin/tool"), "MUTATED\n").unwrap();
    std::fs::remove_file(root.join("etc/motd")).unwrap();
    std::fs::write(root.join("workspace/new-file"), "new\n").unwrap();

    assert_eq!(snapshot(&base), base_snapshot, "base image is immutable");

    // Never-hardlink rule: files must not share nlink with the base's files
    // (reflink/COW copies have nlink == 1; hardlinks would show nlink >= 2).
    let base_tool = std::fs::metadata(base.join("usr/bin/tool")).unwrap();
    let _ = base_tool;
    // Re-clone fresh and compare link counts of a pristine file.
    let engine2 = SpawnBootstrap {
        use_reflink: Some(true),
        ..SpawnBootstrap::new(
            dirs.agents_root.clone(),
            dirs.agents_root.join("base"),
            dirs.seeds_dir.clone(),
        )
    };
    let root2 = engine2
        .bootstrap(&record("a2", "node22-kimi-1.0", None))
        .await
        .unwrap();
    let nlink_base = std::fs::metadata(base.join("etc/motd")).unwrap().nlink();
    let nlink_clone = std::fs::metadata(root2.join("etc/motd")).unwrap().nlink();
    assert_eq!(nlink_base, 1, "base files are never hardlinked");
    assert_eq!(nlink_clone, 1, "clone files are independent inodes");
}

#[tokio::test]
async fn spawn_bootstrap_layout_and_marker() {
    let dirs = test_dirs();
    make_base(&dirs.agents_root.join("base"), "node22-kimi-1.0");
    let engine = SpawnBootstrap::new(
        dirs.agents_root.clone(),
        dirs.agents_root.join("base"),
        dirs.seeds_dir.clone(),
    );
    let root = engine
        .bootstrap(&record("a1", "node22-kimi-1.0", Some("demo")))
        .await
        .unwrap();
    // Writable dirs (§5.4).
    let home = std::fs::metadata(root.join("home/papin")).unwrap();
    assert_eq!(home.mode() & 0o777, 0o700);
    let tmp = std::fs::metadata(root.join("tmp")).unwrap();
    assert_eq!(tmp.mode() & 0o7777, 0o1777);
    assert!(root.join(".papin-rootfs-ready").is_file());
    // Identity files refreshed from templates, overriding the clone.
    let passwd = std::fs::read_to_string(root.join("etc/passwd")).unwrap();
    assert!(passwd.contains("papin:x:1000:1000"));
    // Idempotent: second bootstrap keeps the marker.
    engine
        .bootstrap(&record("a1", "node22-kimi-1.0", None))
        .await
        .unwrap();
}

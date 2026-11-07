use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture {
    path: PathBuf,
    server: Server,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let path = base.join(format!(
            "lh-fs-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        let mut server = Server::default();
        server.configure(json!({"workspace_root":path})).unwrap();
        Self { path, server }
    }
    fn call(&mut self, name: &str, args: Value) -> Result<String, String> {
        self.server.call(name, args)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn write_read_edit_list() {
    let mut f = Fixture::new();
    f.call(
        "write_file",
        json!({"path":"nested/a.txt","content":"one\ntwo\nthree\n"}),
    )
    .unwrap();
    assert_eq!(
        f.call(
            "read_file",
            json!({"path":"nested/a.txt","offset":2,"limit":1})
        )
        .unwrap(),
        "2|two\n[truncated: line limit; increase offset to continue]\n"
    );
    let diff = f
        .call(
            "edit_file",
            json!({"path":"nested/a.txt","old_string":"two","new_string":"second"}),
        )
        .unwrap();
    assert!(diff.contains("-two\n+second"));
    assert_eq!(
        fs::read_to_string(f.path.join("nested/a.txt")).unwrap(),
        "one\nsecond\nthree\n"
    );
    assert_eq!(f.call("list_dir", json!({})).unwrap(), "nested/\n");
    assert!(
        f.call("read_file", json!({"path":"nested/a.txt","offset":0}))
            .is_err()
    );
}

#[test]
fn exact_edits_fail_without_mutation() {
    let mut f = Fixture::new();
    fs::write(f.path.join("a"), "aaa").unwrap();
    for (old, new) in [("aa", "b"), ("missing", "b"), ("", "b"), ("aaa", "aaa")] {
        assert!(
            f.call(
                "edit_file",
                json!({"path":"a","old_string":old,"new_string":new})
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(f.path.join("a")).unwrap(), "aaa");
    }
}

#[test]
fn confinement_and_secret_components() {
    let mut f = Fixture::new();
    for path in [
        "../escape",
        "sub/../escape",
        "/absolute",
        ".env",
        ".env.local/a",
        "key.pem/a",
        "id_rsa/a",
        "sub/.env",
    ] {
        assert!(
            f.call("write_file", json!({"path":path,"content":"secret"}))
                .is_err(),
            "{path}"
        );
        assert!(f.call("read_file", json!({"path":path})).is_err(), "{path}");
        assert!(f.call("list_dir", json!({"path":path})).is_err(), "{path}");
        assert!(
            f.call(
                "search_files",
                json!({"path":path,"pattern":"*","target":"name"})
            )
            .is_err(),
            "{path}"
        );
    }
    f.server
        .configure(json!({"workspace_root":f.path,"allow_secrets":true}))
        .unwrap();
    f.call("write_file", json!({"path":".env","content":"yes"}))
        .unwrap();
    assert_eq!(
        f.call("read_file", json!({"path":".env"})).unwrap(),
        "1|yes\n"
    );
    assert!(
        f.call("write_file", json!({"path":"../escape","content":"no"}))
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn rejects_symlink_files_directories_and_dangling_links() {
    use std::os::unix::fs::symlink;
    let mut f = Fixture::new();
    let outside = Fixture::new();
    fs::write(outside.path.join("a"), "untouched").unwrap();
    symlink(&outside.path, f.path.join("link")).unwrap();
    symlink(outside.path.join("a"), f.path.join("file")).unwrap();
    symlink(outside.path.join("missing"), f.path.join("dangling")).unwrap();
    for path in ["link/a", "link/new/a", "file", "dangling"] {
        assert!(
            f.call("write_file", json!({"path":path,"content":"changed"}))
                .is_err()
        );
        assert!(f.call("read_file", json!({"path":path})).is_err());
    }
    assert!(f.call("list_dir", json!({})).unwrap().is_empty());
    assert!(
        f.call("search_files", json!({"target":"name","pattern":"*"}))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fs::read_to_string(outside.path.join("a")).unwrap(),
        "untouched"
    );
}

#[test]
fn search_globs_regex_ignores_and_limits() {
    let mut f = Fixture::new();
    for path in [
        "a.rs",
        "b.txt",
        "nested/c.rs",
        ".env",
        "key.pem",
        "id_private",
        "ignored.txt",
        "target/a.rs",
        "node_modules/a.rs",
        ".git/a.rs",
    ] {
        let path = f.path.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "alpha\nbeta 42\n").unwrap();
    }
    fs::write(f.path.join(".gitignore"), "ignored.txt\n").unwrap();
    assert_eq!(
        f.call("search_files", json!({"target":"name","pattern":"*.rs"}))
            .unwrap(),
        "a.rs\nnested/c.rs\n"
    );
    let results = f
        .call("search_files", json!({"pattern":"beta [0-9]+"}))
        .unwrap();
    assert_eq!(
        results,
        "a.rs:2:beta 42\nb.txt:2:beta 42\nnested/c.rs:2:beta 42\n"
    );
    assert!(
        f.call("search_files", json!({"pattern":"alpha","limit":1}))
            .unwrap()
            .contains("truncated: match limit")
    );
    assert!(f.call("search_files", json!({"pattern":"["})).is_err());
    assert!(
        f.call("search_files", json!({"pattern":"[","target":"name"}))
            .is_err()
    );
}

#[test]
fn caps_and_invalid_inputs() {
    let mut f = Fixture::new();
    fs::write(f.path.join("long"), "x".repeat(OUTPUT_CAP + 100)).unwrap();
    let out = f.call("read_file", json!({"path":"long"})).unwrap();
    assert!(out.len() <= OUTPUT_CAP && out.contains("truncated"));
    fs::write(f.path.join("huge"), vec![b'a'; FILE_CAP + 1]).unwrap();
    assert!(f.call("read_file", json!({"path":"huge"})).is_err());
    fs::write(f.path.join("binary"), [0xff]).unwrap();
    assert!(f.call("read_file", json!({"path":"binary"})).is_err());
    assert!(f.call("read_file", json!({"path":"."})).is_err());
    assert!(
        f.call("write_file", json!({"path":"","content":""}))
            .is_err()
    );
    assert!(f.call("unknown", json!({})).is_err());
    assert!(
        f.server
            .configure(json!({"workspace_root":f.path,"allow_secrets":"yes"}))
            .is_err()
    );
    assert_eq!(f.server.tools().as_array().unwrap().len(), 5);
}

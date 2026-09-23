//! Native capability registry — hardened runtime kernels with metadata.

mod kernels;
mod registry;
mod sandbox;

pub use kernels::execute;
pub use registry::{
    CapValue, CapabilitySpec, Purity, REGISTRY, get, json_catalog, names, spec_json,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{ExecutionContext, SelectionMode, new_published_buffer};

    fn ctx_with_buffer() -> ExecutionContext {
        ExecutionContext {
            policy: None,
            input: None,
            working_dir: None,
            selection_mode: SelectionMode::Greedy,
            selection_epsilon: 0.10,
            published: Some(new_published_buffer()),
        }
    }

    fn ctx_no_buffer() -> ExecutionContext {
        ExecutionContext {
            policy: None,
            input: None,
            working_dir: None,
            selection_mode: SelectionMode::Greedy,
            selection_epsilon: 0.10,
            published: None,
        }
    }

    #[test]
    fn runtime_publish_roundtrips_each_supported_type() {
        let ctx = ctx_with_buffer();

        execute(
            "runtime.publish",
            &[CapValue::Str("forecast".into()), CapValue::Float(142.7)],
            Some(&ctx),
        )
        .expect("publish float");

        execute(
            "runtime.publish",
            &[CapValue::Str("recommended_count".into()), CapValue::Int(7)],
            Some(&ctx),
        )
        .expect("publish int");

        execute(
            "runtime.publish",
            &[
                CapValue::Str("policy".into()),
                CapValue::Str("forecast_match".into()),
            ],
            Some(&ctx),
        )
        .expect("publish str");

        execute(
            "runtime.publish",
            &[CapValue::Str("override".into()), CapValue::Bool(true)],
            Some(&ctx),
        )
        .expect("publish bool");

        execute(
            "runtime.publish",
            &[CapValue::Str("unset".into()), CapValue::Null],
            Some(&ctx),
        )
        .expect("publish null");

        let buf = ctx.published.as_ref().expect("buffer present");
        let map = buf.borrow();

        assert_eq!(map.get("forecast"), Some(&serde_json::json!(142.7)));
        assert_eq!(map.get("recommended_count"), Some(&serde_json::json!(7)));
        assert_eq!(
            map.get("policy"),
            Some(&serde_json::json!("forecast_match"))
        );
        assert_eq!(map.get("override"), Some(&serde_json::json!(true)));
        assert_eq!(map.get("unset"), Some(&serde_json::Value::Null));
        // BTreeMap => deterministic, sorted-by-name iteration
        let keys: Vec<&String> = map.keys().collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }

    #[test]
    fn runtime_publish_overwrites_same_key() {
        let ctx = ctx_with_buffer();
        execute(
            "runtime.publish",
            &[CapValue::Str("k".into()), CapValue::Int(1)],
            Some(&ctx),
        )
        .unwrap();
        execute(
            "runtime.publish",
            &[CapValue::Str("k".into()), CapValue::Int(2)],
            Some(&ctx),
        )
        .unwrap();
        let buf = ctx.published.as_ref().unwrap();
        assert_eq!(buf.borrow().get("k"), Some(&serde_json::json!(2)));
    }

    #[test]
    fn runtime_publish_is_noop_when_buffer_absent() {
        let ctx = ctx_no_buffer();
        let r = execute(
            "runtime.publish",
            &[CapValue::Str("forecast".into()), CapValue::Float(1.0)],
            Some(&ctx),
        );
        assert!(matches!(r, Ok(CapValue::Null)));
    }

    #[test]
    fn runtime_publish_noop_with_no_context() {
        let r = execute(
            "runtime.publish",
            &[CapValue::Str("forecast".into()), CapValue::Float(1.0)],
            None,
        );
        assert!(matches!(r, Ok(CapValue::Null)));
    }

    #[test]
    fn runtime_publish_rejects_array_values() {
        let ctx = ctx_with_buffer();
        let r = execute(
            "runtime.publish",
            &[
                CapValue::Str("xs".into()),
                CapValue::Array(vec![CapValue::Int(1), CapValue::Int(2)]),
            ],
            Some(&ctx),
        );
        match r {
            Err(msg) => assert!(
                msg.to_lowercase().contains("array"),
                "expected 'array' in error, got: {msg}"
            ),
            Ok(_) => panic!("expected error for array value"),
        }
    }

    #[test]
    fn runtime_publish_rejects_non_finite_float() {
        let ctx = ctx_with_buffer();
        let r = execute(
            "runtime.publish",
            &[CapValue::Str("nan".into()), CapValue::Float(f64::NAN)],
            Some(&ctx),
        );
        assert!(r.is_err(), "NaN must be rejected");
        let r2 = execute(
            "runtime.publish",
            &[CapValue::Str("inf".into()), CapValue::Float(f64::INFINITY)],
            Some(&ctx),
        );
        assert!(r2.is_err(), "Infinity must be rejected");
    }

    #[test]
    fn runtime_publish_requires_two_args() {
        let ctx = ctx_with_buffer();
        let r = execute(
            "runtime.publish",
            &[CapValue::Str("only_name".into())],
            Some(&ctx),
        );
        assert!(r.is_err());
    }

    #[test]
    fn runtime_publish_registered_in_catalog() {
        assert!(get("runtime.publish").is_some());
        assert!(names().iter().any(|n| *n == "runtime.publish"));
    }

    // ── file.writeText sandbox tests ──
    //
    // The write handler must defeat symlink-escape: an existing symlink
    // inside the sandbox pointing at a file outside the sandbox must not
    // be writable through the capability.

    use crate::context::ExecutionPolicy;

    fn fresh_tempdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lycan-cap-write-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create tempdir");
        dir
    }

    fn sandboxed_ctx(root: &std::path::Path) -> ExecutionContext {
        let mut policy = ExecutionPolicy::default();
        policy.allow_file_read = true;
        policy.allow_file_write = true;
        ExecutionContext {
            policy: Some(policy),
            input: None,
            working_dir: Some(root.to_path_buf()),
            selection_mode: SelectionMode::Greedy,
            selection_epsilon: 0.10,
            published: None,
        }
    }

    #[test]
    fn sandbox_root_must_stay_inside_working_dir() {
        let root = fresh_tempdir("root-rules");
        std::fs::write(root.join("in.txt"), "inside").unwrap();

        // Absolute file_root built in code is still refused at call time.
        let mut ctx = sandboxed_ctx(&root);
        ctx.policy.as_mut().unwrap().file_root = Some("/".into());
        let err = execute(
            "file.readText",
            &[CapValue::Str("etc/hosts".into())],
            Some(&ctx),
        )
        .unwrap_err();
        assert!(err.contains("relative path"), "{err}");

        // `..` in file_root is refused.
        let mut ctx = sandboxed_ctx(&root);
        ctx.policy.as_mut().unwrap().file_root = Some("../..".into());
        let err = execute(
            "file.readText",
            &[CapValue::Str("in.txt".into())],
            Some(&ctx),
        )
        .unwrap_err();
        assert!(err.contains("'..'"), "{err}");

        // No working dir means no file access under a policy.
        let mut ctx = sandboxed_ctx(&root);
        ctx.working_dir = None;
        let err = execute(
            "file.readText",
            &[CapValue::Str("in.txt".into())],
            Some(&ctx),
        )
        .unwrap_err();
        assert!(err.contains("no working_dir"), "{err}");

        // A symlinked file_root component cannot lift the root out.
        #[cfg(unix)]
        {
            let outside = fresh_tempdir("root-outside");
            std::fs::write(outside.join("secret.txt"), "outside").unwrap();
            std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
            let mut ctx = sandboxed_ctx(&root);
            ctx.policy.as_mut().unwrap().file_root = Some("link".into());
            let err = execute(
                "file.readText",
                &[CapValue::Str("secret.txt".into())],
                Some(&ctx),
            )
            .unwrap_err();
            assert!(err.contains("escapes the working directory"), "{err}");
            let _ = std::fs::remove_dir_all(&outside);
        }

        // Control: a relative subdirectory root works.
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/a.txt"), "sub-file").unwrap();
        let mut ctx = sandboxed_ctx(&root);
        ctx.policy.as_mut().unwrap().file_root = Some("sub".into());
        let out = execute(
            "file.readText",
            &[CapValue::Str("a.txt".into())],
            Some(&ctx),
        )
        .unwrap();
        assert!(matches!(out, CapValue::Str(ref s) if s == "sub-file"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_in_sandbox_succeeds() {
        let root = fresh_tempdir("ok");
        let ctx = sandboxed_ctx(&root);

        let result = execute(
            "file.writeText",
            &[
                CapValue::Str("hello.txt".into()),
                CapValue::Str("greetings".into()),
            ],
            Some(&ctx),
        );
        assert!(
            matches!(result, Ok(CapValue::Bool(true))),
            "expected Ok(true), got {result:?}"
        );

        let body = std::fs::read_to_string(root.join("hello.txt")).expect("file written");
        assert_eq!(body, "greetings");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn write_through_symlink_pointing_outside_fails() {
        // Layout:
        //   tempdir/
        //     secret.txt             ← outside sandbox, must NOT be modified
        //     sandbox/                ← sandbox root
        //       escape.txt → ../secret.txt
        let base = fresh_tempdir("symlink");
        let sandbox = base.join("sandbox");
        std::fs::create_dir_all(&sandbox).expect("create sandbox");
        let secret = base.join("secret.txt");
        std::fs::write(&secret, "original").expect("seed secret");

        let escape = sandbox.join("escape.txt");
        std::os::unix::fs::symlink(std::path::Path::new("../secret.txt"), &escape)
            .expect("create symlink");

        let ctx = sandboxed_ctx(&sandbox);

        let result = execute(
            "file.writeText",
            &[
                CapValue::Str("escape.txt".into()),
                CapValue::Str("evil".into()),
            ],
            Some(&ctx),
        );

        assert!(
            result.is_err(),
            "expected Err for symlink escape, got {result:?}"
        );
        let msg = result.err().unwrap();
        assert!(
            msg.contains("escapes sandbox"),
            "expected 'escapes sandbox' in error, got: {msg}"
        );

        // The crucial assertion: the file outside the sandbox MUST be untouched.
        let after = std::fs::read_to_string(&secret).expect("secret still readable");
        assert_eq!(
            after, "original",
            "symlink target was clobbered — sandbox escape!"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn write_to_nonexistent_path_creates_file_inside_sandbox() {
        let root = fresh_tempdir("nonexistent");
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).expect("create nested dir");

        let ctx = sandboxed_ctx(&root);

        let result = execute(
            "file.writeText",
            &[
                CapValue::Str("nested/new.txt".into()),
                CapValue::Str("fresh".into()),
            ],
            Some(&ctx),
        );
        assert!(
            matches!(result, Ok(CapValue::Bool(true))),
            "expected Ok(true), got {result:?}"
        );

        let body = std::fs::read_to_string(nested.join("new.txt")).expect("file written");
        assert_eq!(body, "fresh");

        let _ = std::fs::remove_dir_all(&root);
    }
}

//! Language decisions of 2026-09-08, pinned end-to-end:
//! overflow policy (checked, both backends), `!type` alignment,
//! unknown-builtin fail-closed, builtin arity parity, `F!` removal,
//! `!abs`/`!atan2` finiteness, capability float->int range guard.
//!
//! Each case runs the SAME source through the tree-walker (`lycan file.lycs`)
//! and the compiled graph (`lycan compile` + `lycan file.lyc`) and asserts
//! agreement: same exit class, and for value cases, byte-identical stdout.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    // target/{debug,release}/lycan sits two levels under the binary; fall
    // back to a cargo-built debug binary.
    let mut p = std::env::current_exe().unwrap();
    p.pop(); // deps
    p.pop(); // debug|release
    let cand = p.join("lycan");
    if cand.exists() {
        cand
    } else {
        PathBuf::from("target/debug/lycan")
    }
}

struct Out {
    ok: bool,
    stdout: String,
    stderr: String,
}

impl Out {
    fn combined(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

fn run_src(tag: &str, src: &str) -> (Out, Option<PathBuf>) {
    let dir = std::env::temp_dir();
    let l = dir.join(format!("sempar_{}.lycs", tag));
    fs::write(&l, src).unwrap();
    let r = Command::new(bin()).arg(&l).output().expect("lycan");
    (
        Out {
            ok: r.status.success(),
            stdout: String::from_utf8_lossy(&r.stdout).into(),
            stderr: String::from_utf8_lossy(&r.stderr).into(),
        },
        Some(l),
    )
}

fn run_cmp(tag: &str, src: &str) -> (Out, Option<PathBuf>) {
    let dir = std::env::temp_dir();
    let l = dir.join(format!("sempar_{}.lycs", tag));
    fs::write(&l, src).expect("write");
    let c = Command::new(bin()).args(["compile", l.to_str().unwrap()]).output().expect("lycan compile");
    if !c.status.success() {
        return (
            Out {
                ok: false,
                stdout: String::from_utf8_lossy(&c.stdout).into(),
                stderr: String::from_utf8_lossy(&c.stderr).into(),
            },
            None,
        );
    }
    let b = l.with_extension("lyc");
    let r = Command::new(bin()).arg(&b).output().expect("lyc run");
    (
        Out {
            ok: r.status.success(),
            stdout: String::from_utf8_lossy(&r.stdout).into(),
            stderr: String::from_utf8_lossy(&r.stderr).into(),
        },
        Some(b),
    )
}

/// Same source, both backends: agree on success/failure, and identical stdout.
fn both_agree(tag: &str, src: &str) -> (bool, String) {
    let (s, _) = run_src(tag, src);
    let (c, _) = run_cmp(tag, src);
    (
        s.ok == c.ok && s.stdout == c.stdout,
        format!("src={:?}|{:?} cmp={:?}|{:?}", s.ok, s.combined(), c.ok, c.combined()),
    )
}

fn assert_parity(tag: &str, src: &str) {
    let (ok, detail) = both_agree(tag, src);
    assert!(ok, "[{tag}] backends diverge: {detail}");
}

const MAX: &str = "9223372036854775808"; // 2^63 as magnitude (literal is INT_MIN-abs)

#[test]
fn overflow_errors_identically_on_both_backends() {
    // (overflow policy DECIDED 2026-09-08: named runtime error, never wrap)
    assert_parity("ovf_add", "(+ 9223372036854775807 1)");
    assert_parity("ovf_sub", "(- -9223372036854775808 1)");
    assert_parity("ovf_mul", "(* 9223372036854775807 3)");
    assert_parity("ovf_neg", "(neg -9223372036854775808)");
    assert_parity("ovf_abs", "(!abs -9223372036854775808)");
    // INT_MIN / -1 and % -1: the `x % y` divisibility guard itself overflows;
    // checked_rem must run first or the guard panics/wraps before checked_div.
    assert_parity("ovf_div", &format!("($ lo (- 0 {}))\n(/ lo (neg 1))", "9223372036854775807"));
    assert_parity("ovf_mod", &format!("($ lo (- 0 9223372036854775807))\n(% (neg (+ lo 1)) (neg 1))"));
    // Error text is the same string on both sides.
    for (tag, src, msg) in [
        ("ovf_add", "(+ 9223372036854775807 1)", "integer overflow in +"),
        ("ovf_mul", "(* 9223372036854775807 3)", "integer overflow in *"),
        ("ovf_neg", "(neg -9223372036854775808)", "integer overflow in neg"),
        ("ovf_abs", "(!abs -9223372036854775808)", "integer overflow in !abs"),
    ] {
        let (s, _) = run_src(tag, src);
        assert!(!s.ok && s.combined().contains(msg), "src {tag}: expected `{msg}` in {s:?}", s = s.combined());
        let (c, _) = run_cmp(tag, src);
        assert!(!c.ok && c.combined().contains(msg), "cmp {tag}: expected `{msg}` in {}", c.combined());
    }
    // And in the DEBUG profile too (overflow-checks used to make this a
    // process abort, exit 101; checked ops make it the same error).
    let (d, _) = {
        let dir = std::env::temp_dir();
        let l = dir.join("sempar_ovf_dbg.lycs");
        fs::write(&l, "(+ 9223372036854775807 1)").unwrap();
        let exe = bin();
        // only meaningful if we actually have a debug build
        let dbg = exe.to_string_lossy().contains("debug");
        if !dbg { (None, ()) } else {
            let r = Command::new(&exe).arg(&l).output().expect("debug lycan");
            (Some((r.status.code(), String::from_utf8_lossy(&r.stdout).into_owned() + &String::from_utf8_lossy(&r.stderr))), ())
        }
    };
    if let Some((code, out)) = d {
        assert_eq!(code, Some(1), "debug-profile overflow must exit 1, got {code:?}: {out}");
        assert!(out.contains("integer overflow in +"), "debug profile diverged from release: {out}");
    }
    let _ = MAX;
}

#[test]
fn type_returns_type_names_on_both_backends() {
    // (decision 2026-09-08: dedicated TypeOf opcode; was compiled->ToString)
    let src = "(!p (!type 5) (!type \"s\") (!type 1.5) (!type true) (!type (A 1 2)))\n";
    let (s, _) = run_src("ty_all", src);
    assert!(s.ok, "src: {}", s.combined());
    assert_eq!(s.stdout.trim(), "int str float bool array");
    let (c, _) = run_cmp("ty_all", src);
    assert!(c.ok, "cmp: {}", c.combined());
    assert_eq!(c.stdout.trim(), "int str float bool array");
    assert_parity("ty_fn", "(F f () 1)\n(!p (!type f))\n");
    assert_parity("ty_null", "(!p (!type (B)))\n"); // (B) empty -> Null
}

#[test]
fn unknown_builtin_fails_closed_on_both_paths() {
    // decision 2026-09-08: was compiled Noop -> Null, exit 0 (silent)
    let (s, _) = run_src("unk", "(!p (!frobnicate 1))\n");
    assert!(!s.ok && s.combined().contains("unknown builtin '!frobnicate'"), "src: {}", s.combined());
    let (c, _) = run_cmp("unk", "(!p (!frobnicate 1))\n");
    assert!(!c.ok && c.combined().contains("unknown builtin '!frobnicate'"),
        "compiled must REFUSE to compile: {}", c.combined());
    // `!neg` is not a builtin either (operator form is `(neg x)`)
    assert_parity("unk_neg", "(!p (!neg 5))\n");
}

#[test]
fn builtin_arity_table_shared_across_backends() {
    // decision 2026-09-08: interpreter arity table derived from the shared
    // graph::builtin_fixed_arity table; compiled rejects at verify time.
    assert_parity("ar_len0", "(!p (!len))\n");
    assert_parity("ar_len2", "(!p (!len \"abc\" \"x\"))\n");
    assert_parity("ar_at2_1", "(!p (!atan2 1))\n");
    assert_parity("ar_type0", "(!p (!type))\n");
    assert_parity("ar_ok", "(!p (!len \"abc\") (!atan2 1 2))\n");
}

#[test]
fn fbang_is_rejected_at_parse() {
    // decision 2026-09-08: F! removed from the grammar (was inert alias of F).
    // The file MUST be written before the first run: the original version
    // spawned the first case before any write and passed only where a
    // previous session had left the same fixed path in /tmp (a clean Linux
    // container exposed it: `error reading ... No such file`, exit 1 but
    // without the F! message).
    fs::write("/tmp/sempar_fbang.lycs", "(F! s (n) (!p n))\n").unwrap();
    let r = Command::new(bin()).arg("/tmp/sempar_fbang.lycs").output().unwrap();
    assert!(!r.status.success(), "run must reject F!");
    let out = format!("{}{}", String::from_utf8_lossy(&r.stdout), String::from_utf8_lossy(&r.stderr));
    assert!(out.contains("'F!' has no semantics"), "run: {out}");
    let c = Command::new(bin()).args(["compile", "/tmp/sempar_fbang.lycs"]).output().unwrap();
    assert!(!c.status.success(), "compile must reject F!");
    assert!(String::from_utf8_lossy(&c.stderr).contains("'F!' has no semantics"), "compile: {c:?}");
}

#[test]
fn abs_and_atan2_finiteness_parity() {
    // decision 2026-09-08: `!abs` finite-only on both backends (source used
    // to pass inf through); `!atan2` non-numeric is a type error (both used
    // to coerce silently to 0.0 on source / 0.0 operands on compiled).
    assert_parity("abs_inf", "(!p (!abs (!exp 1000)))\n"); // !exp -> inf, no guard, then !abs must error
    assert_parity("atan2_str", "(!p (!atan2 \"x\" 1))\n");
    assert_parity("atan2_nan", "(!p (!atan2 (/ 0.0 0.0) 1))\n");
    assert_parity("atan2_ok", "(!p (!atan2 1 2))\n");
}

#[test]
fn capability_float_int_range_guard() {
    // decision 2026-09-08: out-of-range float args to int-taking capabilities
    // error instead of saturating to i64::MAX (Rust `as i64`).
    assert_parity("cap_range",
        "(!p (!cap \"stats.percentile\" (A 1.0 2.0 3.0) 1e300))\n");
    // normal in-range calls still work identically
    assert_parity("cap_ok",
        "(!p (!cap \"stats.percentile\" (A 1.0 2.0 3.0) 95.0))\n");
}

#[test]
fn structural_equality_decided_identically_on_both_backends() {
    // DECIDED 2026-09-08: Array gains a recursive deep-equality arm;
    // Int/Float stay non-coercing (the former "(== 1 1.0) false" stands);
    // NaN keeps IEEE semantics at depth; Fn stays false (closure register).
    // Note the FLIP: "(== (A) (A))" and "(== (A 1) (A 1))" used to be
    // false (no Array arm); both backends must now say true.
    let src = "\
(!p (== (A 1 2) (A 1 2)))
(!p (== (A (A 1)) (A (A 1))))
(!p (== (A) (A)))
(!p (== (A 1) (A 1.0)))
(!p (== (A 1) (A 2)))
(!p (== (A 1 2) (A 1)))
(!p (!= (A 1) (A 1)))
(!p (== (A 1) \"x\"))
(!p (== 1 1.0))
(!p (== (A (/ 0.0 0.0)) (A (/ 0.0 0.0))))
(!p (== (A (A \"a\") 3) (A (A \"a\") 3)))
";
    assert_parity("eq_struct", src);
    let (s, _) = run_src("eq_struct_vals", src);
    assert!(s.ok, "value probe must run: {:?}", s.combined());
    assert_eq!(
        s.stdout,
        "true\ntrue\ntrue\nfalse\nfalse\nfalse\nfalse\nfalse\nfalse\nfalse\ntrue\n",
        "equality values drifted from the decided table"
    );
}

#[test]
fn equality_depth_limit_boundary_identical() {
    // The cap is the fail-closed answer to unbounded recursion: W-loops can
    // construct arrays with arbitrarily deep nesting, and unbounded
    // equality recursion is a stack overflow. 65 nesting levels (innermost
    // compared at recursion depth 64) compare; 66+ raise the named error.
    fn nest(d: usize) -> String {
        format!("{}{}", "(A ".repeat(d), ")".repeat(d))
    }
    assert_parity("eq_d65", &format!("(== {} {})\n", nest(65), nest(65)));
    let deep = format!("(== {} {})\n", nest(66), nest(66));
    assert_parity("eq_d66", &deep);
    let (s, _) = run_src("eq_d66_msg", &deep);
    assert!(!s.ok);
    assert!(
        s.combined().contains("structural equality depth limit (64) exceeded"),
        "src: {:?}",
        s.combined()
    );
    let (c, _) = run_cmp("eq_d66_msg_c", &deep);
    assert!(!c.ok);
    assert_eq!(s.combined(), c.combined(), "error text must match byte-for-byte");
}

#[test]
fn string_ordering_parity_closes_str_arm_divergence() {
    // Divergence closed 2026-09-08: the compiled executor had no Str arm in
    // gval_cmp, so (< "a" "b") ran in source and errored once compiled.
    // Now byte-order lexicographic on both (matches !len's byte semantics).
    let src = "\
(!p (< \"a\" \"b\"))
(!p (>= \"b\" \"a\"))
(!p (<= \"a\" \"a\"))
(!p (< \"B\" \"a\"))
(!p (< \"z\" \"é\"))
";
    assert_parity("strord", src);
    let (s, _) = run_src("strord_vals", src);
    assert_eq!(s.stdout, "true\ntrue\ntrue\ntrue\ntrue\n");
    // mixed ordering still errors identically
    assert_parity("strord_mix", "(!p (< \"a\" 1))\n");
}

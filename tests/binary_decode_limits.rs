use syntra::binary::{decode, encode};

const DEPTH_LIMIT: usize = 64;

fn program(node: &[u8]) -> Vec<u8> {
    let mut bytes = b"LYCAN\0\x01".to_vec();
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(node);
    bytes
}

fn returns(count: usize, leaf: &[u8]) -> Vec<u8> {
    let mut node = vec![0x34; count];
    node.extend_from_slice(leaf);
    node
}

fn binding(array_depth: usize) -> Vec<u8> {
    let mut node = vec![0x10, 0, 0, 0, 0, 0]; // bind, empty name, immutable
    node.extend(std::iter::repeat_n(0x06, array_depth));
    node.extend_from_slice(&[0x01, 0x05]); // int type, null value
    node
}

fn assert_depth_error(bytes: &[u8]) {
    let err = decode(bytes).expect_err("excessive nesting must be rejected");
    assert!(
        err.to_string()
            .contains("binary decode depth limit (64) exceeded")
    );
}

#[test]
fn rejects_ast_nesting_beyond_limit() {
    assert_depth_error(&program(&returns(DEPTH_LIMIT, &[0x05])));
}

#[test]
fn accepts_ast_nesting_at_limit() {
    let bytes = program(&returns(DEPTH_LIMIT - 1, &[0x05]));
    assert_eq!(encode(&decode(&bytes).unwrap()), bytes);
}

#[test]
fn rejects_array_type_nesting_beyond_limit() {
    assert_depth_error(&program(&binding(DEPTH_LIMIT - 1)));
}

#[test]
fn accepts_array_type_nesting_at_limit() {
    let bytes = program(&binding(DEPTH_LIMIT - 2));
    assert_eq!(encode(&decode(&bytes).unwrap()), bytes);
}

#[test]
fn ast_and_type_nesting_share_the_budget() {
    assert_depth_error(&program(&returns(
        DEPTH_LIMIT / 2,
        &binding(DEPTH_LIMIT / 2 - 1),
    )));
    let bytes = program(&returns(DEPTH_LIMIT / 2, &binding(DEPTH_LIMIT / 2 - 2)));
    assert_eq!(encode(&decode(&bytes).unwrap()), bytes);
}

#[test]
fn sibling_nodes_do_not_consume_each_others_depth_budget() {
    let child = returns(DEPTH_LIMIT - 2, &[0x05]);
    let mut node = vec![0x40]; // array
    node.extend_from_slice(&2u32.to_le_bytes());
    node.extend_from_slice(&child);
    node.extend_from_slice(&child);
    let bytes = program(&node);
    assert_eq!(encode(&decode(&bytes).unwrap()), bytes);
}

#[test]
fn rejects_ci_stack_overflow_regression() {
    assert_depth_error(include_bytes!(
        "../fuzz/corpus/binary_decode/deep-ast-nesting"
    ));
}

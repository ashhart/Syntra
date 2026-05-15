# Values and Types

Lycan has six value types. Type inference is automatic — no annotations required.

## Types

| Type | Literal | Rust equivalent | Notes |
|---|---|---|---|
| Integer | `42`, `-7` | `i64` | 64-bit signed |
| Float | `3.14`, `-0.5` | `f64` | 64-bit IEEE 754 |
| String | `"hello"` | `String` | UTF-8 |
| Boolean | `true`, `false` | `bool` | |
| Null | `null` | — | Absence of value |
| Array | `(A 1 2 3)` | `Vec<Value>` | Heterogeneous |

## Type coercion

Arithmetic between int and float promotes to float:

```lisp
(+ 3 0.5)    ;; 3.5 (float)
(/ 7 2)      ;; 3.5 (float, because not evenly divisible)
(/ 6 2)      ;; 3   (int, evenly divisible)
```

String concatenation via `+`:

```lisp
(+ "hello " "world")   ;; "hello world"
(+ "count: " 42)       ;; "count: 42"
```

## Truthiness

| Value | Truthy? |
|---|---|
| `true` | Yes |
| `false` | No |
| `null` | No |
| `0` (int) | No |
| `""` (empty string) | No |
| `(A)` (empty array) | No |
| Everything else | Yes |

## Arrays

Arrays are ordered, heterogeneous, and zero-indexed:

```lisp
($ arr (A 1 "two" 3.0 true null))
(I arr 0)       ;; 1
(I arr 1)       ;; "two"
(!len arr)      ;; 5
```

## Functions

Functions are first-class values:

```lisp
(F double (x) (* x 2))
($ fn double)
(fn 21)          ;; 42
```

Lambdas:

```lisp
($ sq (\ (x) (* x x)))
(sq 7)           ;; 49
```

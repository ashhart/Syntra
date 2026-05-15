# Lycan Syntax

Lycan uses S-expression syntax. Every construct is a tagged, parenthesized list.

## Values

```lisp
42              ;; integer (i64)
3.14            ;; float (f64)
"hello"         ;; string (UTF-8)
true / false    ;; boolean
null            ;; null
(A 1 2 3)       ;; array
```

## Bindings

```lisp
($ x 42)        ;; immutable binding
($! x 0)        ;; mutable binding
(= x 10)        ;; reassign mutable
```

## Arithmetic and comparison

All operators are prefix:

```lisp
(+ 2 3)         ;; 5
(- 10 4)        ;; 6
(* 7 8)         ;; 56
(/ 7 2)         ;; 3.5
(% 17 5)        ;; 2
(== 5 5)        ;; true
(!= 5 3)        ;; true
(< 3 5)         ;; true
(&& true false)  ;; false
(|| false true)  ;; true
(not true)       ;; false
```

## Functions

```lisp
;; Named function
(F add (a b) (+ a b))
(!p (add 3 4))          ;; prints 7

;; Lambda
($ square (\ (x) (* x x)))

;; Recursive
(F fib (n)
  (? (<= n 1) n
    (+ (fib (- n 1)) (fib (- n 2)))))
```

## Control flow

```lisp
;; If/else (expression — returns a value)
(? (> x 10) "big" "small")

;; Block (evaluates all, returns last)
(B expr1 expr2 expr3)

;; While loop
($! i 0)
(W (< i 10) (!p i) (= i (+ i 1)))

;; For-each
(each x (A 1 2 3 4 5) (!p x))

;; Repeat N times
(# 5 (!p "hello"))
```

## Collections

```lisp
($ arr (A 10 20 30))     ;; array literal
(I arr 1)                ;; index access: 20
(!len arr)               ;; length: 3
(.. 1 5)                 ;; range: (A 1 2 3 4)
(+ (A 1 2) (A 3 4))     ;; concat: (A 1 2 3 4)
```

## Pipelines

```lisp
(|? data (\ (x) (> x 3)))           ;; filter
(|* data (\ (x) (* x 2)))           ;; map
(|+ data (\ (a b) (+ a b)) 0)       ;; reduce
```

## Strategy nodes

```lisp
($ result (strategy
  (implementation_a args)
  (implementation_b args)
  (implementation_c args)))
```

The runtime explores all options, measures timing, validates correctness contracts, rewards winners, and persists learned weights.

## Capabilities

```lisp
(!cap "stats.mean" data)
(!cap "file.readText" "config.json")
(!cap "http.get" "https://api.example.com/data")
(!cap "runtime.inputGet" "request.body.field")
```

## Output

```lisp
(!p "hello" name value)   ;; print space-separated values
```

## Comments

```lisp
;; This is a comment (to end of line)
```

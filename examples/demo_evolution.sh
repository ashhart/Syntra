#!/bin/bash
# LYCAN SELF-LEARNING SHOWCASE
# Shows exactly what the self-optimization does and why it matters
cd "$(dirname "$0")/.."
L="./target/release/lycan"

echo ""
echo "  LYCAN SELF-LEARNING DEMONSTRATION"
echo "  ================================="
echo ""
echo "  What you're about to see:"
echo "  1. A program compiled fresh — all branches weighted equally"
echo "  2. The same program run repeatedly — learning from execution"
echo "  3. Weights converging — the program discovers its own hot paths"
echo "  4. Nodes specialized — generic ops become type-specific"
echo "  5. The binary file itself changes — the program evolves on disk"
echo ""
echo "  Press Enter to start..."
read

echo ""
echo "  === TEST 1: Request Router ==="
echo ""

# Compile fresh
$L compile examples/demo_router.lycs 2>/dev/null
initial_size=$(wc -c < examples/demo_router.lyc | tr -d ' ')

echo "  FRESH COMPILE:"
$L stats examples/demo_router.lyc 2>/dev/null | grep -E "Nodes:|Edges:|Binary|Branch|fired|Converged"
echo ""

echo "  Running 10 rounds of traffic simulation..."
for i in $(seq 1 10); do
    $L examples/demo_router.lyc > /dev/null 2>/dev/null
    printf "  Run %2d complete\n" $i
done
echo ""

final_size=$(wc -c < examples/demo_router.lyc | tr -d ' ')

echo "  AFTER 10 RUNS:"
$L stats examples/demo_router.lyc 2>/dev/null | grep -E "Nodes:|Edges:|Binary|Branch|fired|Converged|Specialized"
echo ""
echo "  Binary: $initial_size -> $final_size bytes"
echo ""

echo "  === TEST 2: Fraud Classifier ==="
echo ""

$L compile examples/demo_classifier.lycs 2>/dev/null
initial_size=$(wc -c < examples/demo_classifier.lyc | tr -d ' ')

echo "  FRESH COMPILE:"
$L stats examples/demo_classifier.lyc 2>/dev/null | grep -E "Nodes:|Branch|Converged"
echo ""

echo "  Running 10 rounds of transaction processing..."
for i in $(seq 1 10); do
    $L examples/demo_classifier.lyc > /dev/null 2>/dev/null
    printf "  Run %2d complete\n" $i
done
echo ""

final_size=$(wc -c < examples/demo_classifier.lyc | tr -d ' ')

echo "  AFTER 10 RUNS:"
$L stats examples/demo_classifier.lyc 2>/dev/null | grep -E "Nodes:|Binary|Branch|fired|Converged|Specialized"
echo ""
echo "  Binary: $initial_size -> $final_size bytes"

echo ""
echo "  === WHAT JUST HAPPENED ==="
echo ""
echo "  Both programs CHANGED themselves. Not the source code."
echo "  Not a config file. The BINARY ITSELF evolved."
echo ""
echo "  The router learned that GET requests dominate."
echo "  The classifier learned that most transactions are safe."
echo ""
echo "  In a traditional language, you'd need:"
echo "    - A profiler to identify hot paths"
echo "    - A developer to read the profile"
echo "    - Manual code changes to optimize"
echo "    - A recompile and redeploy"
echo ""
echo "  In Lycan, the program does all of that itself."
echo "  Every. Single. Run."
echo ""

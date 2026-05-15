# .lyc Binary Graph Format

A `.lyc` file is a compiled Lycan graph — the executable program.

## Header

```
Bytes 0-3:  Magic "LYCN" (0x4C 0x59 0x43 0x4E)
Byte 4:     Format version (currently 5)
Bytes 5-8:  Node count (u32 LE)
Bytes 9-12: Edge count (u32 LE)
Bytes 13-16: State vector size (u32 LE)
Bytes 17-20: String table entry count (u32 LE)
Bytes 21-24: Flags (u32 LE, reserved)
Byte 25:    Entry node ID (u32 LE)
```

## Nodes

Each node contains:
- `id` (u32): unique node identifier
- `op` (u8): opcode
- `operand_count` (u32): number of operands
- `operands`: array of tagged operands (NodeRef, Immediate, StringRef, VarSlot, StateRef)
- `weight_count` (u32): number of learnable weights
- `weights`: array of f64
- `bias` (f64): type specialization hint
- `activation_count` (u64): how many times this node has fired
- `state_slot` (Option<u32>): index into state vector
- `weight_kind` (u8): Observational, Adaptive, TypeHint, Strategy, or Decision
- `annotation` (Option<u32>): string table index
- `contract` (u8): None, SameOutput, Validated, or WithinTolerance
- `objective` (u8): None, Speed, Accuracy, Reliability, Cost, Risk, Confidence, Reward, MultiObjective

## Opcodes

| Range | Category | Examples |
|---|---|---|
| 0x01-0x07 | Values | ConstInt, ConstFloat, ConstStr, ConstBool, ConstNull, LoadVar, StoreVar |
| 0x10-0x15 | Arithmetic | Add, Sub, Mul, Div, Mod, Neg |
| 0x20-0x25 | Comparison | Eq, Neq, Lt, Gt, Lte, Gte |
| 0x30-0x32 | Logic | And, Or, Not |
| 0x40-0x47 | Control | Branch, Merge, Loop, Sequence, AdaptiveChoice, Guard, ForEach, Repeat |
| 0x50-0x53 | Functions | Define, Call, Return, Lambda |
| 0x60-0x64 | Collections | Array, Index, Range, Length, Chars |
| 0x70-0x7D | IO/Math | Print, ReadLine, ParseNum, Split, ToString, Sin, Cos, Abs, Floor, Round, Sqrt, Ln, Exp, Atan2 |
| 0x80-0x86 | Adaptive | Adapt, Weight, Predict, Feedback, Spawn, Prune, Strategy |
| 0x90-0x93 | Pipeline | Pipe, Filter, Map, Reduce |
| 0xA0 | Native | Capability |

## State vector

A mutable `Vec<f64>` storing per-strategy option stats:
- Layout per strategy: `[tries_0, time_ns_0, correct_0, tries_1, time_ns_1, correct_1, ...]`
- 3 slots per option
- Updated after each execution run

## Journal

Append-only evolution history entries:
- `run_number` (u64)
- `node_id` (u32)
- `mutation` (u8): WeightUpdate, TypeSpecialized, ConstantFolded, PathPruned, GuardInserted, NodeSpawned, FeedbackReceived, EvolutionStarted, ProposalAccepted, ProposalRejected, EvolutionCompleted
- `reason` (u32): string table index

## Serialization

Binary format uses little-endian encoding. All writes are atomic (write to temp + fsync + rename).

# IRL

## Introduction

This project implement some technical aspects of IR (intermediate representation) language, including compilation, analysis, optimization, execution, etc. The functionality is quite similar to [LLVM](https://www.llvm.org), but substantially simplified. This project is written in pure and safe Rust, except for the VM, where some `unsafe` code appears, but safe indeed. Some of the implementation is ported and improved from my previous project [GoCompiler](https://github.com/wzh99/GoCompiler). 

## Language

The language involved is an CFG-based, register-to-register model IR. Phi instruction is provided to build SSA form, but is not mandatory. The following is an example to show the structure of a simple program. The program is not very practical, but should suffice to show some characteristics of this language. This example can also be seen in [example.ir](test/example.ir)

```
type @Foo = { i16, { [2][4]i8 }, *@Bar }
type @Bar = { *i64, *@Foo }

@g: i64 <- 0;

fn @main() {
%Begin:
    @g <- call i64 @max(1, 2);
    $b <- alloc [4]i64;
    $p <- ptr *i64 $b [@g];
    $v <- ld i64 $p;
    $q <- ptr *i64 $p, 1;
    st i64 $v -> $q;
    ret;
}

fn @max($a: i64, $b: i64) -> i64 {
%Begin:
    $c <- ge i64 $a, $b;
    br $c ? %True : %False;
%True:
    $x.0 <- mov i64 $a;
    jmp %End;
%False:
    $x.1 <- mov i64 $b;
    jmp %End;
%End:
    $x.2 <- phi i64 [%True: $x.0] [%False: $x.1];
    ret $x.2;
}
```

It could be seen from the example that the syntax is a bit similar to [LLVM IR](https://www.llvm.org/docs/LangRef.html), but adopts some syntax features commonly seen in higher level programming languages. It tries to reduce type annotation required in the language, as long as it can be inferred from context or expressions. Also, it tries to make some of the instructions more informative, such as `st`, `br`, `phi` and `ptr`.

The type system and instruction set are all quite simple, but they are fairly enough support most of the following work. For type definition, see [`lang::val::Type`](src/lang/value.rs). For instruction set, see [`lang::instr`](src/lang/instr.rs).

## Compilation

This project supports reading a text source of the language and convert it to memory representation. It covers all the front-end procedures of a common compiler, including lexical, syntactical and semantical analysis.

### Parsing

The lexer and parser are all written by hand. The lexical and syntactical rules can be seen in [`compile::syntax`](src/compile/syntax.rs). The grammar is an LL(1) one. The lexer creates a token one at a time. The recursive-descent parser keeps a buffer for the incoming token stream, either peeks to see which rule to use, or consumes token in the buffer to progress. The parsing is rather efficient.

### Construction and Verification

After parsing, the memory representation will be constructed, and the semantic correctness will be checked along the way. This process is divided into several passes: the first one deals with type aliases, global variable declarations and function signatures, and the second deal with basic blocks inside each function. 

If a function contains one or more phi instructions, *or* if any versioned symbol appears in this function, it is assumed to be in SSA form, and another pass is required to verify this assumption. To be in SSA form, the following requirement should be satisfied: 

* Each local variable should be defined only once in the static program.

* Each local variable is defined before used.

* Each phi instruction has source operands for all predecessors.

## Optimization

Optimizations are implemented as passes of transforms on the program, which is usually the case in modern compilers. Most of the optimizations are based on the SSA form, so transformation to that form is mandatory. At present, the following optimizations are supported:

### Global Value Numbering

Detect fully redundant computations by finding congruent variables. Implementation at [`opt::gvn::GvnOpt`](src/opt/gvn.rs).

### Sparse Conditional Constant Propagation

Replace later uses of compile-time constants with their corresponding values. It applies this transformation by symbolic execution of the function using both control flow graph and SSA value graph. Implementation at [`opt::sccp:SccpOpt`](src/opt/sccp.rs).

### Partial Redundancy Elimination

Place each (binary) computation at its optimal position. GVN-PRE algorithm is adopted, which utilizes GVN as a subroutine to better handle expressions that may not be lexically equivalent. Algebraic simplification is also applied during optimization. Implementation at [`opt::pre::PreOpt`](src/opt/pre.rs).

### Dead Code Elimination

Conventional mark-sweep algorithm to find instructions that define unused variables. Can serve as a subroutine for other optimizations. It is implemented as a method of [`lang::func::Func`](src/lang/ssa.rs).

### Aggressive DCE

Take an aggressive approach to Dead Code Elimination. It only keep instructions that contribute to the returned result, and remove the rest. Note that this may alter the runtime behavior of a function. Implementation at [`opt::simple::AdceOpt`](src/opt/simple.rs).

### Copy Propagation

Replace later uses of copied values with their original ones. Can serve as a subroutine for other optimizations. Implementation at [`opt::simple::CopyProp`](src/opt/simple.rs).

Other optimizations will be added to this project successively.

## Execution

To be done.

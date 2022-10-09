#![feature(allocator_api)]

use air::BrainfuckAir;
use air::ExecutionInfo;
use ark_ff_optimized::fp64::Fp;
use mini_stark::Matrix;
use mini_stark::ProofOptions;
use mini_stark::Prover;
use mini_stark::Trace;
use std::time::Instant;
use vm::compile;
use vm::simulate;

mod air;
mod tables;
mod vm;

const HELLO_WORLD_SOURCE: &str = "
+++++ +++++             initialize counter (cell #0) to 10
[                       use loop to set 70/100/30/10
    > +++++ ++              add  7 to cell #1
    > +++++ +++++           add 10 to cell #2
    > +++                   add  3 to cell #3
    > +                     add  1 to cell #4
<<<< -                  decrement counter (cell #0)
]
> ++ .                  print 'H'
> + .                   print 'e'
+++++ ++ .              print 'l'
.                       print 'l'
+++ .                   print 'o'
> ++ .                  print ' '
<< +++++ +++++ +++++ .  print 'W'
> .                     print 'o'
+++ .                   print 'r'
----- - .               print 'l'
----- --- .             print 'd'
> + .                   print '!'
> .                     print '\n'
";

struct BrainfuckTrace(Matrix<Fp>);

impl Trace for BrainfuckTrace {
    type Fp = Fp;

    const NUM_BASE_COLUMNS: usize = 35;

    fn len(&self) -> usize {
        println!("YOOO {}", self.0.num_rows());
        self.0.num_rows()
    }

    fn base_columns(&self) -> &Matrix<Self::Fp> {
        &self.0
    }
}

struct BrainfuckProver(ProofOptions);

impl Prover for BrainfuckProver {
    type Fp = Fp;
    type Air = BrainfuckAir;
    type Trace = BrainfuckTrace;

    fn new(options: ProofOptions) -> Self {
        BrainfuckProver(options)
    }

    fn options(&self) -> ProofOptions {
        self.0
    }

    fn get_pub_inputs(&self, trace: &BrainfuckTrace) -> ExecutionInfo {
        ExecutionInfo {
            execution_len: trace.base_columns().num_rows(),
            // TODO: add inputs
            input: Vec::new(),
            output: Vec::new(),
        }
    }
}

fn main() {
    let program = compile(HELLO_WORLD_SOURCE);
    let mut output = Vec::new();
    let trace = simulate::<Fp>(&program, &mut std::io::empty(), &mut output);

    let now = Instant::now();
    let options = ProofOptions::new(32, 8);
    let prover = BrainfuckProver::new(options);
    let proof = prover.generate_proof(BrainfuckTrace(trace));
    println!("Runtime: {:?}", now.elapsed());
    println!("Result: {:?}", proof.unwrap());
}

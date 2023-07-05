#![feature(allocator_api)]

use ark_ff::One;
use ark_poly::EvaluationDomain;
use ark_poly::Radix2EvaluationDomain;
use ministark::air::AirConfig;
use ministark::constraints::AlgebraicItem;
use ministark::constraints::Constraint;
use ministark::constraints::ExecutionTraceColumn;
use ministark::hints::Hints;
use ministark::utils::FieldVariant;
use ministark::utils::GpuAllocator;
use ministark::Matrix;
use ministark::ProofOptions;
use ministark::Provable;
use ministark::Verifiable;
use ministark::Witness;
use ministark_gpu::fields::p18446744069414584321::ark::Fp;
use num_traits::Pow;
use sha2::Sha256;
use std::time::Instant;

struct FibTrace(Matrix<Fp>);

impl FibTrace {
    fn last_value(&self) -> Fp {
        *(self.0).0[7].last().unwrap()
    }
}

impl Witness for FibTrace {
    type Fp = Fp;
    type Fq = Fp;

    fn trace_len(&self) -> usize {
        self.0.num_rows()
    }

    fn base_columns(&self) -> &Matrix<Self::Fp> {
        &self.0
    }
}

enum FibHint {
    ClaimedNthFibNum = 0,
}

struct FibAirConfig;

impl AirConfig for FibAirConfig {
    const NUM_BASE_COLUMNS: usize = 8;
    type Fp = Fp;
    type Fq = Fp;
    type PublicInputs = Fp;

    fn gen_hints(
        _trace_len: usize,
        claimed_nth_fib_number: &Fp,
        _: &ministark::challenges::Challenges<Self::Fq>,
    ) -> ministark::hints::Hints<Self::Fq> {
        Hints::new(vec![(
            FibHint::ClaimedNthFibNum as usize,
            *claimed_nth_fib_number,
        )])
    }

    fn constraints(trace_len: usize) -> Vec<Constraint<FieldVariant<Self::Fp, Self::Fq>>> {
        use AlgebraicItem::*;
        let trace_xs = Radix2EvaluationDomain::<Fp>::new(trace_len).unwrap();
        // NOTE: =1
        let first_trace_x = Constant(FieldVariant::Fp(trace_xs.element(0)));
        // NOTE: =trace_xs.group_gen_inv()
        let last_trace_x = Constant(FieldVariant::Fp(trace_xs.element(trace_len - 1)));
        let one = Constant(FieldVariant::Fp(Fp::one()));

        let boundary_constraints = {
            let v0 = AlgebraicItem::Constant(FieldVariant::Fp(Fp::one()));
            let v1 = v0 + v0;
            let v2 = &v1 * v0;
            let v3 = &v1 * &v2;
            let v4 = &v2 * &v3;
            let v5 = &v3 * &v4;
            let v6 = &v4 * &v5;
            let v7 = &v5 * &v6;

            vec![
                0.curr() - v0,
                1.curr() - v1,
                2.curr() - v2,
                3.curr() - v3,
                4.curr() - v4,
                5.curr() - v5,
                6.curr() - v6,
                7.curr() - v7,
            ]
        }
        .into_iter()
        .map(|constraint| {
            // ensure constraint holds in the first row
            // symbolically divide `(x - t_0)`
            constraint / (X - first_trace_x)
        });

        let transition_constraints = vec![
            0.next() - 6.curr() * 7.curr(),
            1.next() - 7.curr() * 0.next(),
            2.next() - 0.next() * 1.next(),
            3.next() - 1.next() * 2.next(),
            4.next() - 2.next() * 3.next(),
            5.next() - 3.next() * 4.next(),
            6.next() - 4.next() * 5.next(),
            7.next() - 5.next() * 6.next(),
        ]
        .into_iter()
        .map(|constraint| {
            // ensure constraints hold in all rows except the last
            // multiply by `(x - t_(n-1))` to remove the last term
            // NOTE: `x^trace_len - 1 = (x - t_0)(x - t_1)...(x - t_(n-1))`
            // NOTE: `t^(n-1) = t^(-1)`
            constraint * ((X - last_trace_x) / (X.pow(trace_len) - one))
        });

        let terminal_constraints =
            vec![7.curr() - AlgebraicItem::Hint(FibHint::ClaimedNthFibNum as usize)]
                .into_iter()
                .map(|constraint| {
                    // ensure constraint holds in the last row
                    // symbolically divide `(x - t_0)`
                    constraint / (X - last_trace_x)
                });

        boundary_constraints
            .chain(terminal_constraints)
            .chain(transition_constraints)
            .map(Constraint::new)
            .collect()
    }
}

struct FibClaim(Fp);

impl Verifiable for FibClaim {
    type Fp = Fp;
    type Fq = Fp;
    type AirConfig = FibAirConfig;
    type Digest = Sha256;

    fn get_public_inputs(&self) -> Fp {
        self.0
    }
}

impl Provable for FibClaim {
    type Witness = FibTrace;
}

fn gen_trace(n: usize) -> FibTrace {
    assert!(n.is_power_of_two());
    assert!(n > 8);

    let num_rows = n / 8;

    let mut col0 = Vec::with_capacity_in(num_rows, GpuAllocator);
    let mut col1 = Vec::with_capacity_in(num_rows, GpuAllocator);
    let mut col2 = Vec::with_capacity_in(num_rows, GpuAllocator);
    let mut col3 = Vec::with_capacity_in(num_rows, GpuAllocator);
    let mut col4 = Vec::with_capacity_in(num_rows, GpuAllocator);
    let mut col5 = Vec::with_capacity_in(num_rows, GpuAllocator);
    let mut col6 = Vec::with_capacity_in(num_rows, GpuAllocator);
    let mut col7 = Vec::with_capacity_in(num_rows, GpuAllocator);

    let mut v0 = Fp::one();
    let mut v1 = v0 + v0;
    let mut v2 = v0 * v1;
    let mut v3 = v1 * v2;
    let mut v4 = v2 * v3;
    let mut v5 = v3 * v4;
    let mut v6 = v4 * v5;
    let mut v7 = v5 * v6;

    for _ in 0..num_rows {
        col0.push(v0);
        col1.push(v1);
        col2.push(v2);
        col3.push(v3);
        col4.push(v4);
        col5.push(v5);
        col6.push(v6);
        col7.push(v7);

        v0 = v6 * v7;
        v1 = v7 * v0;
        v2 = v0 * v1;
        v3 = v1 * v2;
        v4 = v2 * v3;
        v5 = v3 * v4;
        v6 = v4 * v5;
        v7 = v5 * v6;
    }

    FibTrace(Matrix::new(vec![
        col0, col1, col2, col3, col4, col5, col6, col7,
    ]))
}

fn main() {
    let options = ProofOptions::new(32, 4, 8, 8, 64);

    let now = Instant::now();
    let trace = gen_trace(1048576 * 32);
    println!("Trace generated in: {:?}", now.elapsed());

    let claim = FibClaim(trace.last_value());

    let now = Instant::now();
    let proof = pollster::block_on(claim.generate_proof(options, trace)).expect("prover failed");
    println!("Proof generated in: {:?}", now.elapsed());

    let now = Instant::now();
    claim.verify(proof).expect("verification failed");
    println!("Proof generated in: {:?}", now.elapsed());
}

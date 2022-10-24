use crate::channel::ProverChannel;
use crate::composer::ConstraintComposer;
use crate::composer::DeepPolyComposer;
use crate::fri::FriOptions;
use crate::fri::FriProof;
use crate::fri::FriProver;
use crate::merkle::MerkleTree;
use crate::trace::Queries;
use crate::utils::Timer;
use crate::Air;
use crate::Constraint;
use crate::Matrix;
use crate::Trace;
use crate::TraceInfo;
use ark_ff::Field;
use ark_poly::domain::Radix2EvaluationDomain;
use ark_serialize::CanonicalDeserialize;
use ark_serialize::CanonicalSerialize;
use fast_poly::GpuField;
use sha2::Sha256;

// TODO: include ability to specify:
// - base field
// - extension field
// - hashing function
// - fri folding factor
// - fri max remainder size
#[derive(Debug, Clone, Copy, CanonicalSerialize, CanonicalDeserialize)]
pub struct ProofOptions {
    pub num_queries: u8,
    // would be nice to make this clear as LDE blowup factor vs constraint blowup factor
    pub blowup_factor: u8,
    pub grinding_factor: u8,
}

impl ProofOptions {
    pub const MIN_NUM_QUERIES: u8 = 1;
    pub const MAX_NUM_QUERIES: u8 = 128;
    pub const MIN_BLOWUP_FACTOR: u8 = 2;
    pub const MAX_BLOWUP_FACTOR: u8 = 64;

    pub fn new(num_queries: u8, blowup_factor: u8, grinding_factor: u8) -> Self {
        assert!(num_queries >= Self::MIN_NUM_QUERIES);
        assert!(num_queries <= Self::MAX_NUM_QUERIES);
        assert!(blowup_factor.is_power_of_two());
        assert!(blowup_factor >= Self::MIN_BLOWUP_FACTOR);
        assert!(blowup_factor <= Self::MAX_BLOWUP_FACTOR);
        ProofOptions {
            num_queries,
            blowup_factor,
            grinding_factor,
        }
    }

    pub fn into_fri_options(self) -> FriOptions {
        // TODO: move fri params into struct
        FriOptions::new(self.blowup_factor.into(), 8, 64)
    }
}

/// A proof generated by a mini-stark prover
#[derive(CanonicalSerialize, CanonicalDeserialize, Clone)]
pub struct Proof<F: GpuField> {
    pub options: ProofOptions,
    pub trace_info: TraceInfo,
    pub base_trace_commitment: Vec<u8>,
    pub extension_trace_commitment: Option<Vec<u8>>,
    pub composition_trace_commitment: Vec<u8>,
    pub fri_proof: FriProof<F>,
    pub trace_queries: Queries<F>,
}

/// Errors that can occur during the proving stage
#[derive(Debug)]
pub enum ProvingError {
    Fail,
    // /// This error occurs when a transition constraint evaluated over a specific execution
    // trace /// does not evaluate to zero at any of the steps.
    // UnsatisfiedTransitionConstraintError(usize),
    // /// This error occurs when polynomials built from the columns of a constraint evaluation
    // /// table do not all have the same degree.
    // MismatchedConstraintPolynomialDegree(usize, usize),
}

pub trait Prover {
    type Fp: GpuField;
    type Air: Air<Fp = Self::Fp>;
    type Trace: Trace<Fp = Self::Fp>;

    fn new(options: ProofOptions) -> Self;

    fn get_pub_inputs(&self, trace: &Self::Trace) -> <Self::Air as Air>::PublicInputs;

    fn options(&self) -> ProofOptions;

    /// Return value is of the form `(lde, polys, merkle_tree)`
    fn build_trace_commitment(
        &self,
        trace: &Matrix<Self::Fp>,
        trace_domain: Radix2EvaluationDomain<Self::Fp>,
        lde_domain: Radix2EvaluationDomain<Self::Fp>,
    ) -> (Matrix<Self::Fp>, Matrix<Self::Fp>, MerkleTree<Sha256>) {
        let _timer = Timer::new("trace extension");
        let trace_polys = trace.interpolate_columns(trace_domain);
        let trace_lde = trace_polys.evaluate(lde_domain);
        drop(_timer);
        let _timer = Timer::new("trace commitment");
        let merkle_tree = trace_lde.commit_to_rows();
        drop(_timer);
        (trace_lde, trace_polys, merkle_tree)
    }

    fn generate_proof(&self, trace: Self::Trace) -> Result<Proof<Self::Fp>, ProvingError> {
        let _timer = Timer::new("proof generation");

        let options = self.options();
        let trace_info = trace.info();
        let pub_inputs = self.get_pub_inputs(&trace);
        let air = Self::Air::new(trace_info, pub_inputs, options);
        let mut channel = ProverChannel::<Self::Air, Sha256>::new(&air);

        {
            // TODO: move into validation section
            let ce_blowup_factor = air.ce_blowup_factor();
            let lde_blowup_factor = air.lde_blowup_factor();
            assert!(
                ce_blowup_factor <= lde_blowup_factor,
                "constraint evaluation blowup factor {ce_blowup_factor} is 
                larger than the lde blowup factor {lde_blowup_factor}"
            );
        }

        let trace_domain = air.trace_domain();
        let lde_domain = air.lde_domain();
        let (base_trace_lde, base_trace_polys, base_trace_lde_tree) =
            self.build_trace_commitment(trace.base_columns(), trace_domain, lde_domain);

        channel.commit_base_trace(base_trace_lde_tree.root());
        let num_challenges = air.num_challenges();
        let challenges = channel.get_challenges::<Self::Fp>(num_challenges);

        #[cfg(debug_assertions)]
        let mut execution_trace = trace.base_columns().clone();
        let mut execution_trace_lde = base_trace_lde;
        let mut execution_trace_polys = base_trace_polys;
        let mut extension_trace_tree = None;

        if let Some(extension_trace) = trace.build_extension_columns(&challenges) {
            let (extension_trace_lde, extension_trace_polys, extension_trace_lde_tree) =
                self.build_trace_commitment(&extension_trace, trace_domain, lde_domain);
            channel.commit_extension_trace(extension_trace_lde_tree.root());
            #[cfg(debug_assertions)]
            execution_trace.append(extension_trace);
            execution_trace_lde.append(extension_trace_lde);
            execution_trace_polys.append(extension_trace_polys);
            extension_trace_tree = Some(extension_trace_lde_tree);
        }

        #[cfg(debug_assertions)]
        air.validate_constraints(&challenges, &execution_trace);

        let _timer = Timer::new("Quadratic constraints");
        let quadratic_constraints = Constraint::into_quadratic_constraints(
            &challenges,
            air.transition_constraints(),
            air.lde_blowup_factor(),
            &mut execution_trace_lde,
        );

        // let quadratic_constraints = air.transition_constraints().to_vec();

        drop(_timer);

        println!("quad cons len {}", quadratic_constraints.len());

        let _timer = Timer::new("Composition trace");
        let composition_coeffs = channel.get_constraint_composition_coeffs();
        let constraint_coposer =
            ConstraintComposer::new(&air, composition_coeffs, quadratic_constraints);
        // TODO: move commitment here
        let (composition_trace_lde, composition_trace_polys, composition_trace_lde_tree) =
            constraint_coposer.build_commitment(&challenges, &execution_trace_lde);
        channel.commit_composition_trace(composition_trace_lde_tree.root());
        drop(_timer);

        let _timer = Timer::new("OOD evals");
        let g = trace_domain.group_gen;
        let z = channel.get_ood_point();
        let ood_execution_trace_evals = execution_trace_polys.evaluate_at(z);
        let ood_execution_trace_evals_next = execution_trace_polys.evaluate_at(z * g);
        channel.send_ood_trace_states(&ood_execution_trace_evals, &ood_execution_trace_evals_next);
        let z_n = z.pow([execution_trace_polys.num_cols() as u64]);
        let ood_composition_trace_evals = composition_trace_polys.evaluate_at(z_n);
        channel.send_ood_constraint_evaluations(&ood_composition_trace_evals);
        drop(_timer);

        let deep_coeffs = channel.get_deep_composition_coeffs();
        let _timer = Timer::new("DEEP composition");
        let mut deep_poly_composer = DeepPolyComposer::new(&air, deep_coeffs, z);
        deep_poly_composer.add_execution_trace_polys(
            execution_trace_polys,
            ood_execution_trace_evals,
            ood_execution_trace_evals_next,
        );
        deep_poly_composer
            .add_composition_trace_polys(composition_trace_polys, ood_composition_trace_evals);
        let deep_composition_poly = deep_poly_composer.into_deep_poly();
        let deep_composition_lde = deep_composition_poly.evaluate(lde_domain);
        drop(_timer);

        let _timer = Timer::new("FRI");
        let mut fri_prover = FriProver::<Self::Fp, Sha256>::new(air.options().into_fri_options());
        fri_prover.build_layers(&mut channel, deep_composition_lde.try_into().unwrap());

        channel.grind_fri_commitments();

        let query_positions = channel.get_fri_query_positions();
        let fri_proof = fri_prover.into_proof(&query_positions);

        let queries = Queries::new(
            &execution_trace_lde,
            &composition_trace_lde,
            base_trace_lde_tree,
            extension_trace_tree,
            composition_trace_lde_tree,
            &query_positions,
        );
        drop(_timer);

        Ok(channel.build_proof(queries, fri_proof))
    }
}

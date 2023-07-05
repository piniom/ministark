use crate::air::AirConfig;
use crate::challenges::Challenges;
use crate::channel::ProverChannel;
use crate::composer::DeepPolyComposer;
use crate::fri::FriProver;
use crate::hints::Hints;
use crate::utils::GpuAllocator;
use crate::utils::GpuVec;
use crate::witness::Queries;
use crate::Air;
use crate::Matrix;
use crate::Proof;
use crate::ProofOptions;
use crate::Verifiable;
use crate::Witness;
use alloc::vec::Vec;
use ark_poly::EvaluationDomain;
use sha2::Sha256;
use std::time::Instant;

/// Errors that can occur during the proving stage
#[derive(Debug)]
pub enum ProvingError {
    Fail,
    // TODO
}

pub trait Provable: Verifiable {
    type Witness: Witness<Fp = Self::Fp, Fq = Self::Fq>;

    async fn generate_proof(
        &self,
        options: ProofOptions,
        witness: Self::Witness,
    ) -> Result<Proof<Self::Fp, Self::Fq>, ProvingError> {
        let now = Instant::now();
        let air = Air::new(witness.trace_len(), self.get_public_inputs(), options);
        let mut channel = ProverChannel::<Self::AirConfig, Sha256>::new(&air);
        println!("Init air: {:?}", now.elapsed());

        let now = Instant::now();
        let trace_xs = air.trace_domain();
        let lde_xs = air.lde_domain();
        let base_trace = witness.base_columns();
        assert_eq!(Self::AirConfig::NUM_BASE_COLUMNS, base_trace.num_cols());
        let base_trace_polys = base_trace.interpolate(trace_xs);
        let base_trace_lde = base_trace_polys.evaluate(lde_xs);
        let base_trace_lde_tree = base_trace_lde.commit_to_rows::<Sha256>();
        channel.commit_base_trace(base_trace_lde_tree.root());
        let challenges = air.gen_challenges(&mut channel.public_coin);
        let hints = air.gen_hints(&challenges);
        println!("Base trace: {:?}", now.elapsed());

        let now = Instant::now();
        let extension_trace = witness.build_extension_columns(&challenges);
        let num_extension_cols = extension_trace.as_ref().map_or(0, Matrix::num_cols);
        assert_eq!(Self::AirConfig::NUM_EXTENSION_COLUMNS, num_extension_cols);
        let extension_trace_polys = extension_trace.as_ref().map(|t| t.interpolate(trace_xs));
        let extension_trace_lde = extension_trace_polys.as_ref().map(|p| p.evaluate(lde_xs));
        let extension_trace_tree = extension_trace_lde.as_ref().map(Matrix::commit_to_rows);
        if let Some(t) = extension_trace_tree.as_ref() {
            channel.commit_extension_trace(t.root());
        }
        println!("Extension trace: {:?}", now.elapsed());

        #[cfg(debug_assertions)]
        self.validate_constraints(&challenges, &hints, base_trace, extension_trace.as_ref());
        drop((base_trace, extension_trace));

        let now = Instant::now();
        let composition_constraint_coeffs =
            air.gen_composition_constraint_coeffs(&mut channel.public_coin);
        let x_lde = lde_xs.elements().collect::<Vec<_>>();
        println!("X lde: {:?}", now.elapsed());
        let now = Instant::now();
        let composition_evals = Self::AirConfig::eval_constraint(
            air.composition_constraint(),
            &challenges,
            &hints,
            &composition_constraint_coeffs,
            air.lde_blowup_factor(),
            x_lde.to_vec_in(GpuAllocator),
            &base_trace_lde,
            extension_trace_lde.as_ref(),
        );
        println!("Constraint eval: {:?}", now.elapsed());
        let now = Instant::now();
        let composition_poly = composition_evals.into_polynomials(air.lde_domain());
        let composition_trace_cols = air.ce_blowup_factor();
        let composition_trace_polys = Matrix::from_rows(
            GpuVec::try_from(composition_poly)
                .unwrap()
                .chunks(composition_trace_cols)
                .map(<[Self::Fq]>::to_vec)
                .collect(),
        );
        let composition_trace_lde = composition_trace_polys.evaluate(air.lde_domain());
        let composition_trace_lde_tree = composition_trace_lde.commit_to_rows();
        channel.commit_composition_trace(composition_trace_lde_tree.root());
        println!("Constraint composition polys: {:?}", now.elapsed());

        let now = Instant::now();
        let mut deep_poly_composer = DeepPolyComposer::new(
            &air,
            channel.get_ood_point(),
            &base_trace_polys,
            extension_trace_polys.as_ref(),
            composition_trace_polys,
        );
        let (execution_trace_oods, composition_trace_oods) = deep_poly_composer.get_ood_evals();
        channel.send_execution_trace_ood_evals(execution_trace_oods);
        channel.send_composition_trace_ood_evals(composition_trace_oods);
        let deep_coeffs = air.gen_deep_composition_coeffs(&mut channel.public_coin);
        let deep_composition_poly = deep_poly_composer.into_deep_poly(deep_coeffs);
        let deep_composition_lde = deep_composition_poly.into_evaluations(lde_xs);
        println!("Deep composition: {:?}", now.elapsed());

        let now = Instant::now();
        let mut fri_prover = FriProver::<Self::Fq, Sha256>::new(options.into_fri_options());
        #[cfg(feature = "std")]
        let now = std::time::Instant::now();
        fri_prover.build_layers(&mut channel, deep_composition_lde.try_into().unwrap());
        #[cfg(feature = "std")]
        println!("yo {:?}", now.elapsed());

        channel.grind_fri_commitments();

        let query_positions = channel.get_fri_query_positions();
        let fri_proof = fri_prover.into_proof(&query_positions);
        println!("FRI: {:?}", now.elapsed());

        let queries = Queries::new(
            &base_trace_lde,
            extension_trace_lde.as_ref(),
            &composition_trace_lde,
            &base_trace_lde_tree,
            extension_trace_tree.as_ref(),
            &composition_trace_lde_tree,
            &query_positions,
        );
        Ok(channel.build_proof(queries, fri_proof))
    }

    /// Check the AIR constraints are valid
    fn validate_constraints(
        &self,
        _challenges: &Challenges<Self::Fq>,
        _hints: &Hints<Self::Fq>,
        _base_trace: &crate::Matrix<Self::Fp>,
        _extension_trace: Option<&crate::Matrix<Self::Fq>>,
    ) {
        // TODO: move constraint checking from air.rs into here
        // #[cfg(all(feature = "std", debug_assertions))]
        // fn validate_constraints(
        //     &self,
        //     challenges: &Challenges<C::Fq>,
        //     hints: &Hints<C::Fq>,
        //     base_trace: &crate::Matrix<C::Fp>,
        //     extension_trace: Option<&crate::Matrix<C::Fq>>,
        // ) {
        //     use AlgebraicItem::*;
        //     use Expr::*;

        //     let num_execution_trace_columns = C::NUM_BASE_COLUMNS +
        // C::NUM_EXTENSION_COLUMNS;     let mut col_indicies = vec![false;
        // num_execution_trace_columns];     let mut challenge_indicies =
        // vec![false; challenges.len()];     let mut hint_indicies =
        // vec![false; hints.len()];

        //     for constraint in &self.constraints {
        //         constraint.traverse(&mut |node| match node {
        //             Leaf(Challenge(i)) => challenge_indicies[*i] = true,
        //             Leaf(Trace(i, _)) => col_indicies[*i] = true,
        //             Leaf(Hint(i)) => hint_indicies[*i] = true,
        //             _ => {}
        //         })
        //     }

        //     for (index, exists) in col_indicies.into_iter().enumerate() {
        //         if !exists {
        //             // TODO: make assertion
        //             println!("WARN: no constraints for execution trace column
        // {index}");         }
        //     }

        //     for (index, exists) in challenge_indicies.into_iter().enumerate()
        // {         if !exists {
        //             // TODO: make assertion
        //             println!("WARN: challenge at index {index} never used");
        //         }
        //     }

        //     for (index, exists) in hint_indicies.into_iter().enumerate() {
        //         if !exists {
        //             // TODO: make assertion
        //             println!("WARN: hint at index {index} never used");
        //         }
        //     }

        //     let trace_domain = self.trace_domain();
        //     let base_column_range = Self::base_column_range();
        //     let extension_column_range = Self::extension_column_range();

        //     // helper function to get a value from the execution trace
        //     let get_trace_value = |row: usize, col: usize, offset: isize| {
        //         let pos = (row as isize +
        // offset).rem_euclid(trace_domain.size() as isize) as usize;
        // if base_column_range.contains(&col) {
        // FieldVariant::Fp(base_trace.0[col][pos])         } else if
        // extension_column_range.contains(&col) {             let col =
        // col - C::NUM_BASE_COLUMNS;
        // FieldVariant::Fq(extension_trace.unwrap().0[col][pos])
        //         } else {
        //             unreachable!("requested column {col} does not exist")
        //         }
        //     };

        //     for (c_idx, constraint) in
        // self.constraints().into_iter().enumerate() {         for
        // (row, x) in trace_domain.elements().enumerate() {
        // let is_valid = constraint                 .check(&mut |leaf|
        // match leaf {                     X => FieldVariant::Fp(x),
        //                     &Hint(i) => FieldVariant::Fq(hints[i]),
        //                     &Challenge(i) => FieldVariant::Fq(challenges[i]),
        //                     &Trace(col, offset) => get_trace_value(row, col,
        // offset),                     &Constant(c) => c,
        //                 })
        //                 .is_some();

        //             if !is_valid {
        //                 let mut vals = vec![format!("x = {x}")];
        //                 constraint.traverse(&mut |node| match *node {
        //                     // get a description of each leaf node
        //                     Leaf(Trace(col, offset)) => vals.push(format!(
        //                         "Trace(col={col:0>3}, offset={offset:0>3}) =
        // {}",                         get_trace_value(row, col,
        // offset)                     )),
        //                     Leaf(Challenge(i)) => {
        //                         vals.push(format!("Challenge({i}) = {}",
        // challenges[i]))                     }
        //                     Leaf(Hint(i)) => vals.push(format!("Hint({i}) =
        // {}", hints[i])),                     // skip tree nodes
        //                     _ => (),
        //                 });

        //                 vals.sort();
        //                 vals.dedup();

        //                 // TODO: display constraint? eprintln!("Constraint
        // is:\n{constraint}\n");                 #[cfg(feature = "std")]
        //                 eprint!("Constraint {c_idx} does not evaluate to a
        // low degree polynomial. ");                 #[cfg(feature =
        // "std")]                 eprintln!("Divide by zero occurs at
        // row {row}.\n");                 #[cfg(feature = "std")]
        //                 eprintln!("Expression values:\n{}", vals.join("\n"));
        //                 panic!();
        //             }
        //         }
        //     }
        // }
    }
}

use crate::curve_handlers::basis::BSplineBasis;
use crate::curve_handlers::csr_tools::CSR;
use crate::curve_handlers::Error;

use itertools::Itertools;
use ndarray::prelude::*;
use ndarray::{concatenate, Order};
use ndarray_einsum_beta::{einsum, tensordot};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct SplineObject {
    pub bases: Vec<BSplineBasis>,
    pub controlpoints: ArrayD<f64>,
    pub dimension: usize,
    pub rational: bool,
}

impl SplineObject {
    pub fn new(
        bases: Vec<BSplineBasis>,
        controlpoints: Option<Array2<f64>>,
        rational: Option<bool>,
    ) -> Result<Self, Error> {
        let mut cpts = match controlpoints {
            Some(controlpoints) => controlpoints,
            None => default_control_points(&bases)?,
        };
        let rtnl = rational.unwrap_or(false);

        if cpts.slice(s![0, ..]).shape()[0] == 1 {
            cpts = concatenate(
                Axis(1),
                &[
                    cpts.view(),
                    Array1::<f64>::zeros(cpts.shape()[0])
                        .insert_axis(Axis(1))
                        .view(),
                ],
            )?;
        }

        if rtnl == true {
            cpts = concatenate(
                Axis(1),
                &[
                    cpts.view(),
                    Array1::<f64>::ones(cpts.shape()[0])
                        .insert_axis(Axis(1))
                        .view(),
                ],
            )?;
        }

        let dim = &cpts.shape()[1] - (rtnl as usize);
        let shp = determine_shape(&bases)?;
        let ncomps = dim + (rtnl as usize);
        let cpts_shaped = reshaper(cpts, shp, ncomps)?;

        Ok(SplineObject {
            bases,
            controlpoints: cpts_shaped,
            dimension: dim,
            rational: rtnl,
        })
    }

    /// Check whether the given evaluation parameters are valid
    fn validate_domain(&self, t: &mut [&mut Array1<f64>]) -> Result<(), Error> {
        for (basis, params) in self.bases.iter().zip(t.iter_mut()) {
            if basis.periodic < 0 {
                basis.snap(*params);
                let p_max = &params.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                let p_min = &params.iter().copied().fold(f64::INFINITY, f64::min);
                if *p_min < basis.start() || basis.end() < *p_max {
                    return Result::Err(Error::InvalidDomainError);
                }
            }
        }

        Ok(())
    }

    /// Evaluate the object at given parametric values.
    ///
    /// If *tensor* is true, evaluation will take place on a tensor product
    /// grid, i.e. it will return an *n1* × *n2* × ... × *dim* array, where
    /// *ni* is the number of evaluation points in direction *i*, and *dim* is
    /// the physical dimension of the object.
    ///
    /// If *tensor* is false, there must be an equal number *n* of evaluation
    /// points in all directions, and the return value will be an *n* × *dim*
    /// array.
    /// ## parameters
    /// * t: collection of parametric coordinates in which to evaluate
    /// * tensor: whether to evaluate on a tensor product grid
    /// ## returns
    /// * Array (shape as describe above)
    pub fn evaluate(
        &self,
        t: &mut Vec<&mut Array1<f64>>,
        tensor: Option<bool>,
    ) -> Result<ArrayD<f64>, Error> {
        self.validate_domain(t)?;
        let tnsr = tensor.unwrap_or(true);

        let all_equal_length = match &t[..] {
            [] => true,
            [_one] => true,
            [first, remaining @ ..] => remaining.iter().all(|v| v.len() == first.len()),
        };

        if !tnsr && !all_equal_length {
            return Result::Err(Error::InvalidDomainError);
        }

        let evals = &mut &self
            .bases
            .iter()
            .zip(t.iter_mut())
            .map(|e| {
                let res = e.0.evaluate(e.1, 0, true)?;
                Ok(res)
            })
            .collect::<Result<Vec<_>, Error>>()?;

        let evals = &mut evals.clone();
        let out = self.tensor_evaluate(evals, tnsr).unwrap();

        Ok(out)
    }

    fn tensor_evaluate(
        &self,
        eval_bases: &mut Vec<CSR>,
        tensor: bool,
    ) -> Result<ArrayD<f64>, Error> {
        // *** BEGIN KLUDGE ****
        // owing to the fact that the conventional ellipsis notation is not yet
        // implemented for einsum, we use this workaround that should cover us.
        // If not, just grow the maps as needed or address the issue:
        // https://github.com/oracleofnj/einsum/issues/6
        let init_map: HashMap<usize, &str> = [
            (2, "ij,jp->ip"),
            (3, "ij,jpq->ipq"),
            (4, "ij,jpqr->ipqr"),
            (5, "ij,jpqrs->ipqrs"),
            (6, "ij,jpqrst->ipqrst"),
        ]
        .iter()
        .cloned()
        .collect();

        let iter_map: HashMap<usize, &str> = [
            (3, "ij,ijp->ip"),
            (4, "ij,ijpq->ipq"),
            (5, "ij,ijpqr->ipqr"),
            (6, "ij,ijpqrs->ipqrs"),
        ]
        .iter()
        .cloned()
        .collect();

        let mut out;
        if tensor {
            eval_bases.reverse();
            let cpts = self.controlpoints.clone().to_owned();
            let idx = eval_bases.len() - 1;
            out = eval_bases.iter().fold(cpts, |e, tns| {
                tensordot(&tns.todense(), &e, &[Axis(1)], &[Axis(idx)])
            });
        } else {
            let pos = 0;
            let mut key = self.bases.len() + 1;
            let mut val = match init_map.get(&key) {
                Some(val) => Ok(val),
                _ => Result::Err(Error::EinsumOperandError),
            }?;
            out = einsum(val, &[&eval_bases[pos].todense(), &self.controlpoints])
                .map_err(|_| Error::EinsumError)?;

            for _ in eval_bases.iter().skip(1) {
                key += 1;
                val = match iter_map.get(&key) {
                    Some(val) => Ok(val),
                    _ => Result::Err(Error::EinsumOperandError),
                }?;
                let temp = out.clone();
                out = einsum(val, &[&eval_bases[pos].todense(), &temp])
                    .map_err(|_| Error::EinsumError)?;
            }
        }
        // *** END KLUDGE ****

        Ok(out)
    }

    pub fn derivative(&self) {
        todo!()
    }
}

fn default_control_points(bases: &Vec<BSplineBasis>) -> Result<Array2<f64>, Error> {
    let mut temp = bases
        .into_iter()
        .rev()
        .map(|b| {
            let mut v = b.greville().into_raw_vec();
            v.reverse();
            v
        })
        .multi_cartesian_product()
        .collect::<Vec<_>>();
    temp.reverse();

    // because the above is just a little bit incorrect...
    for elem in temp.iter_mut() {
        elem.reverse();
    }

    let mut data = Vec::new();
    let ncols = temp.first().map_or(0, |row| row.len());
    let mut nrows = 0;

    for i in 0..temp.len() {
        data.extend_from_slice(&temp[i]);
        nrows += 1;
    }

    let out = Array2::from_shape_vec((nrows, ncols), data)?;

    Ok(out)
}

/// Custom reshaping function to preserve control points of several
/// dimensions that are stored contiguously.
///
/// The return value has shape (*newshape, ncomps), where ncomps is
/// the number of components per control point, as inferred by the
/// size of `arr` and the desired shape.
fn reshaper(
    arr: Array2<f64>,
    mut newshape: Vec<usize>,
    ncomps: usize,
) -> Result<ArrayD<f64>, Error> {
    newshape.reverse();
    newshape.push(ncomps);

    let mut spec: Vec<usize> = (0..newshape.len() - 1).collect();
    spec.reverse();
    spec.push(newshape.len() - 1);

    let tmp = arr.to_shape((&newshape[..], Order::C))?;
    let tmp = tmp.to_owned().into_dyn();

    let out = tmp.view().permuted_axes(&spec[..]).to_owned();

    Ok(out)
}

fn determine_shape(bases: &Vec<BSplineBasis>) -> Result<Vec<usize>, Error> {
    let out = bases
        .into_iter()
        .map(|e| e.num_functions())
        .collect::<Vec<usize>>();

    Ok(out)
}

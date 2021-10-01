use crate::curve_handlers::basis::BSplineBasis;
use crate::curve_handlers::csr_tools::CSR;
use crate::curve_handlers::utils::*;
use crate::curve_handlers::Error;

use itertools::Itertools;
use ndarray::prelude::*;
use ndarray::{concatenate, Order};
use ndarray_einsum_beta::{einsum, tensordot};
use ndarray_linalg::*;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct SplineObject {
    pub bases: Vec<BSplineBasis>,
    pub controlpoints: ArrayD<f64>,
    pub dimension: usize,
    pub rational: bool,
    pub pardim: usize,
}

/// Master struct for spline objects with arbitrary dimensions.
///
/// This class should be composed instead of used directly.
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
        let rational = rational.unwrap_or(false);

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

        if rational {
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

        let dim = cpts.shape()[1] - (rational as usize);
        let bases_shape = determine_shape(&bases)?;
        let ncomps = dim + (rational as usize);
        let cpts_shaped = reshaper(cpts, bases_shape, ncomps)?;
        let pardim = cpts_shaped.shape().len() - 1;

        Ok(SplineObject {
            bases,
            controlpoints: cpts_shaped,
            dimension: dim,
            rational,
            pardim,
        })
    }

    /// Check whether the given evaluation parameters are valid
    pub fn validate_domain(&self, t: &mut [&mut Array1<f64>]) -> Result<(), Error> {
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
    ///
    /// ### parameters
    /// * t: collection of parametric coordinates in which to evaluate
    /// * tensor: whether to evaluate on a tensor product grid
    ///
    /// ### returns
    /// * Array (shape as describe above)
    pub fn evaluate(
        &self,
        t: &mut Vec<&mut Array1<f64>>,
        tensor: Option<bool>,
    ) -> Result<ArrayD<f64>, Error> {
        self.validate_domain(t)?;
        let tensor = tensor.unwrap_or(true);

        let all_equal_length = match &t[..] {
            [] => true,
            [_one] => true,
            [first, remaining @ ..] => remaining.iter().all(|v| v.len() == first.len()),
        };

        if !tensor && !all_equal_length {
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
        let out = self.tensor_evaluate(evals, tensor).unwrap();

        Ok(out)
    }

    fn tensor_evaluate(
        &self,
        eval_bases: &mut Vec<CSR>,
        tensor: bool,
    ) -> Result<ArrayD<f64>, Error> {
        // KLUDGE!
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
            let mut pos = 0;
            let mut key = self.bases.len() + 1;
            let mut val = match init_map.get(&key) {
                Some(val) => Ok(val),
                _ => Result::Err(Error::EinsumOperandError),
            }?;

            out = einsum(val, &[&eval_bases[pos].todense(), &self.controlpoints])
                .map_err(|_| Error::EinsumError)?;

            for _ in eval_bases.iter().skip(1) {
                pos += 1;
                val = match iter_map.get(&key) {
                    Some(val) => Ok(val),
                    _ => Result::Err(Error::EinsumOperandError),
                }?;
                let temp = out.clone().to_owned();

                out = einsum(val, &[&eval_bases[pos].todense(), &temp])
                    .map_err(|_| Error::EinsumError)?;

                key -= 1;
            }
        }
        // *** END KLUDGE ****

        Ok(out)
    }

    pub fn derivative(&self) {
        // need this if we want to allow the user to explore the payout curve,
        // but not to get things moving.
        todo!()
    }

    /// Return knots vector
    ///
    /// If `direction` is given, returns the knots in that direction only.
    /// Otherwise, specifying direction as a negative value  returns the
    /// knots of all directions.
    ///
    /// ### parameters
    /// * direction: Direction number (axis) in which to get the knots.
    /// * with_multiplicities: If true, return knots with multiplicities \
    /// (i.e. repeated).
    pub fn knots(
        &self,
        direction: isize,
        with_multiplicities: Option<bool>,
    ) -> Result<Vec<Array1<f64>>, Error> {
        let with_multiplicities = with_multiplicities.unwrap_or(false);
        let out;

        if direction < 0 {
            if with_multiplicities {
                out = self
                    .bases
                    .iter()
                    .map(|e| e.knots.clone().to_owned())
                    .collect::<Vec<_>>();
            } else {
                out = self
                    .bases
                    .iter()
                    .map(|e| e.knot_spans(false).to_owned())
                    .collect::<Vec<_>>();
            }
        } else {
            let p = direction as usize;
            if with_multiplicities {
                out = (&[self.bases[p].knots.clone().to_owned()]).to_vec();
            } else {
                out = (&[self.bases[p].knot_spans(false).to_owned()]).to_vec();
            }
        }

        Ok(out)
    }

    /// This will manipulate one or both to ensure that they are both rational
    /// or nonrational, and that they lie in the same physical space.
    pub fn make_splines_compatible(&mut self, otherspline: &mut SplineObject) -> Result<(), Error> {
        if self.rational {
            otherspline.force_rational()?;
        } else if otherspline.rational {
            self.force_rational()?;
        }

        if self.dimension > otherspline.dimension {
            otherspline.set_dimension(self.dimension)?;
        } else {
            self.set_dimension(otherspline.dimension)?;
        }

        Ok(())
    }

    /// Force a rational representation of the object.
    pub fn force_rational(&mut self) -> Result<(), Error> {
        if !self.rational {
            self.controlpoints = self.insert_phys(&self.controlpoints, 1f64)?;
        }

        Ok(())
    }

    /// Sets the physical dimension of the object. If increased, the new
    /// components are set to zero.
    ///
    /// ### parameters
    /// * new_dim: New dimension
    pub fn set_dimension(&mut self, new_dim: usize) -> Result<(), Error> {
        let mut dim = self.dimension;

        while new_dim > dim {
            self.controlpoints = self.insert_phys(&self.controlpoints, 0f64)?;
            dim += 1;
        }

        while new_dim < dim {
            let axis = if self.rational { -2 } else { -1 };
            self.controlpoints = self.delete_phys(&self.controlpoints, axis)?;
            dim -= 1;
        }

        self.dimension = new_dim;

        Ok(())
    }

    fn insert_phys(&self, arr: &ArrayD<f64>, insert_value: f64) -> Result<ArrayD<f64>, Error> {
        let mut arr_shape = arr.shape().to_vec();
        let n = arr_shape[arr_shape.len() - 1];
        let arr_prod = arr_shape.iter().product();

        let raveled = arr.to_shape(((arr_prod,), Order::C))?;
        let mut new_arr = Array1::<f64>::zeros(0);

        for i in (0..raveled.len()).step_by(n) {
            let new_row = concatenate(
                Axis(0),
                &[
                    raveled.slice(s![i..i + n]).view(),
                    (insert_value * Array1::<f64>::ones(1)).view(),
                ],
            )?;
            new_arr = concatenate(Axis(0), &[new_arr.view(), new_row.view()])?;
        }

        arr_shape[n] += 1;
        let out = new_arr.to_shape((&arr_shape[..], Order::C))?.to_owned();

        Ok(out)
    }

    fn delete_phys(&self, arr: &ArrayD<f64>, axis: isize) -> Result<ArrayD<f64>, Error> {
        let mut arr_shape = arr.shape().to_vec();
        let n = arr_shape[arr_shape.len() - 1];
        let step = (n as isize + axis) as usize;
        let arr_prod = arr_shape.iter().product();

        let raveled = arr.to_shape(((arr_prod,), Order::C))?;
        let mut new_arr = Array1::<f64>::zeros(0);

        for i in (0..raveled.len()).step_by(n) {
            let new_row;
            if axis < -1 {
                let front = raveled.slice(s![i..i + step]).clone().to_owned();
                let tail = raveled.slice(s![i + step + 1..i + n]).clone().to_owned();
                new_row = concatenate(Axis(0), &[front.view(), tail.view()])?;
            } else {
                new_row = raveled.slice(s![i..i + step]).clone().to_owned();
            }
            new_arr = concatenate(Axis(0), &[new_arr.view(), new_row.view()])?;
        }

        arr_shape[n - 1] -= 1;
        let out = new_arr.to_shape((&arr_shape[..], Order::C))?.to_owned();

        Ok(out)
    }

    /// Return polynomial order (degree + 1).
    ///
    /// If `direction` is given, returns the order in that direction only.
    /// Otherwise, specifying direction as a negative value  returns the
    /// order of all directions.
    ///
    /// ### parameters
    ///  * direction: Direction in which to get the order.
    pub fn order(&self, direction: isize) -> Result<Vec<usize>, Error> {
        let out;
        if direction < 0 {
            out = self.bases.iter().map(|e| e.order).collect::<Vec<_>>();
        } else {
            let p = direction as usize;
            out = (&[self.bases[p].order]).to_vec();
        }

        Ok(out)
    }

    /// Raise the polynomial order of the object. If only one argument is
    /// given in `raises`, the order is raised equally over all directions.
    /// The explicit version is only implemented on open knot vectors. The
    /// function `raise_order_implicit` is used otherwise.
    ///
    /// ### parameters
    /// * raises: Number of times to raise the order in a given direction.
    pub fn raise_order(&mut self, raises: Vec<usize>) -> Result<(), Error> {
        let raises_used;
        if raises.len() == 1 {
            raises_used = vec![raises[0]; self.pardim];
        } else {
            raises_used = raises.clone();
        }

        if raises_used.len() != self.pardim {
            return Result::Err(Error::InvalidRaisesValueError);
        }

        if raises_used.iter().sum::<usize>() == 0 {
            return Ok(());
        }

        // I don't know how to not use the .unwrap() here, but it is technically
        // fine in this case as there is no way for `.continuity()` to error
        // in the following construct
        let test = self
            .bases
            .iter()
            .any(|x| x.continuity(x.knots[0]).unwrap() < x.order as isize || x.periodic > -1);
        if test {
            self.raise_order_implicit(raises_used)?;
            return Ok(());
        }

        let mut controlpoints = self.controlpoints.clone();
        for i in 0..self.pardim {
            let dimensions = &controlpoints.shape().to_vec();
            let mut indices = Array1::<usize>::from_vec((0..self.pardim + 1).into_iter().collect());
            indices[i] = self.pardim;
            indices[self.pardim] = i;

            let slice = &indices.to_vec()[..indices.len() - 1];
            let newshape_0 = &slice
                .iter()
                .map(|e| dimensions[*e])
                .collect::<Vec<_>>()
                .iter()
                .product::<usize>();
            let newshape_1 = &dimensions[0];

            let cpoints_permuted = controlpoints.view().permuted_axes(&indices.to_vec()[..]);
            let cpoints_reshaped = cpoints_permuted
                .to_shape(((*newshape_0, *newshape_1), Order::C))?
                .to_owned();

            controlpoints = raise_order_1d(
                cpoints_reshaped.shape()[1] - 1,
                self.order(i as isize)?[0],
                self.bases[i].knots.clone().to_owned(),
                cpoints_reshaped.clone().into_dyn().to_owned(),
                raises[i],
                self.bases[i].periodic,
            )?;
        }
        self.controlpoints = controlpoints;

        let new_bases = &self
            .bases
            .clone()
            .into_iter()
            .zip(raises_used.into_iter())
            .map(|(b, r)| {
                let mut new_basis = b;
                new_basis.raise_order(r);
                new_basis
            })
            .collect::<Vec<_>>();

        self.bases = new_bases.to_vec();

        Ok(())
    }

    /// Raise the polynomial order of the object. If only one argument is
    /// given, the order is raised equally over all directions.
    ///
    /// ### parameters
    /// * raises: Number of times to raise the order in a given direction.
    fn raise_order_implicit(&mut self, raises: Vec<usize>) -> Result<(), Error> {
        let raises_used;
        if raises.len() == 1 {
            raises_used = vec![raises[0]; self.pardim];
        } else {
            raises_used = raises;
        }

        if raises_used.len() != self.pardim {
            return Result::Err(Error::InvalidRaisesValueError);
        }

        if raises_used.iter().sum::<usize>() == 0 {
            return Ok(());
        }

        let new_bases = &self
            .bases
            .clone()
            .into_iter()
            .zip(raises_used.into_iter())
            .map(|(b, r)| {
                let mut new_basis = b;
                new_basis.raise_order(r);
                new_basis
            })
            .collect::<Vec<_>>();

        // Set up an interpolation problem. This works in projective space,
        // so no special handling for rational objects
        let interpolation_pts = &new_bases.iter().map(|b| b.greville()).collect::<Vec<_>>();

        let n_old = &mut self
            .bases
            .clone()
            .iter()
            .zip(&interpolation_pts.to_vec()[..])
            .map(|(b, pts)| {
                let mut t = pts.clone().to_owned();
                let res = b.evaluate(&mut t, 0, true)?;
                Ok(res.todense())
            })
            .collect::<Result<Vec<_>, Error>>()?;

        let n_new = &mut new_bases
            .iter()
            .zip(&interpolation_pts.to_vec()[..])
            .map(|(b, pts)| {
                let mut t = pts.clone().to_owned();
                let res = b.evaluate(&mut t, 0, true)?;
                Ok(res.todense())
            })
            .collect::<Result<Vec<_>, Error>>()?;

        // Calculate the projective interpolation points
        let mut res = self.controlpoints.clone();
        n_old.reverse();
        for elem in n_old.iter() {
            res = tensordot(elem, &res, &[Axis(1)], &[Axis(self.pardim - 1)]);
        }

        // Solve the interpolation problem
        n_new.reverse();
        for elem in n_new.iter() {
            let inv = elem.clone().inv_into().map_err(Error::CannotInvertMatrix)?;
            res = tensordot(&inv, &res, &[Axis(1)], &[Axis(self.pardim - 1)]);
        }

        self.controlpoints = res;
        self.bases = new_bases.to_vec();

        Ok(())
    }
}

fn default_control_points(bases: &[BSplineBasis]) -> Result<Array2<f64>, Error> {
    let mut temp = bases
        .iter()
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

    for elem in temp.iter() {
        data.extend_from_slice(elem);
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

fn determine_shape(bases: &[BSplineBasis]) -> Result<Vec<usize>, Error> {
    let out = bases
        .iter()
        .map(|e| e.num_functions())
        .collect::<Vec<usize>>();

    Ok(out)
}

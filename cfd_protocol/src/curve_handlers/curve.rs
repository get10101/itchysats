use crate::curve_handlers::utils::*;
use crate::curve_handlers::basis::BSplineBasis;
use crate::curve_handlers::splineobject::SplineObject;
use crate::curve_handlers::Error;

use ndarray::prelude::*;
use ndarray::s;
use ndarray_linalg::Solve;
use std::cmp::max;

use super::utils::GaussLegendreQuadrature;

fn default_basis() -> Result<Vec<BSplineBasis>, Error> {
    let out = vec![BSplineBasis::new(None, None, None)?];
    Ok(out)
}

#[derive(Clone, Debug)]
pub struct Curve {
    spline: SplineObject,
}

/// Represents a curve: an object with a one-dimensional parameter space.
impl Curve {
    /// Construct a curve with the given basis and control points. In theory,
    /// the curve could be defined in some Euclidean space of dimension N, but
    /// for the moment only curves in E^2 are supported via hard-coding. That is,
    /// any valid basis set can be passed in when instantiating this object,
    /// but only the first one is every considered in the methods provided.
    ///
    /// The default is to create a linear one-element mapping from (0,1) to the
    /// unit interval.
    ///
    /// ### parameters
    /// * basis: The underlying B-Spline basis
    /// * controlpoints: An *n* × *d* matrix of control points
    /// * rational: Whether the curve is rational (in which case the \
    /// control points are interpreted as pre-multiplied with the weight, \
    /// which is the last coordinate)
    pub fn new(
        bases: Option<Vec<BSplineBasis>>,
        controlpoints: Option<Array2<f64>>,
        rational: Option<bool>,
    ) -> Result<Self, Error> {
        let bases = bases.unwrap_or(default_basis()?);
        let spline = SplineObject::new(bases, controlpoints, rational)?;

        Ok(Curve { spline })
    }

    // def evaluate(self, *params):
    //     """  Evaluate the object at given parametric values.

    //     This function returns an *n1* × *n2* × ... × *dim* array, where *ni* is
    //     the number of evaluation points in direction *i*, and *dim* is the
    //     physical dimension of the object.

    //     If there is only one evaluation point, a vector of length *dim* is
    //     returned instead.

    //     :param u,v,...: Parametric coordinates in which to evaluate
    //     :type u,v,...: float or [float]
    //     :return: Geometry coordinates
    //     :rtype: numpy.array
    //     """
    //     squeeze = is_singleton(params[0])
    //     params = [ensure_listlike(p) for p in params]

    //     self._validate_domain(*params)

    //     # Evaluate the derivatives of the corresponding bases at the corresponding points
    //     # and build the result array
    //     N = self.bases[0].evaluate(params[0], sparse=True)
    //     result = N @ self.controlpoints

    //     # For rational objects, we divide out the weights, which are stored in the
    //     # last coordinate
    //     if self.rational:
    //         for i in range(self.dimension):
    //             result[..., i] /= result[..., -1]
    //         result = np.delete(result, self.dimension, -1)

    //     # Squeeze the singleton dimensions if we only have one point
    //     if squeeze:
    //         result = result.reshape(self.dimension)

    //     return result

    /// Evaluate the object at given parametric values.
    ///
    /// ### parameters
    /// * t: collection of parametric coordinates in which to evaluate
    /// Realistically, this should actually be an Array1 object, but for
    /// consistency with the underlying SplineObject methods the collection
    /// is used instead.
    ///
    /// ### returns
    /// * 2D array
    pub fn evaluate(&self, t: &mut Vec<&mut Array1<f64>>) -> Result<Array2<f64>, Error> {
        self.spline.validate_domain(t)?;

        let n_csr = self.spline.bases[0].evaluate(t[0], 0, true)?;

        // kludge...
        let raveled = self.spline.controlpoints.clone().into_raw_vec();
        let n0 = self.spline.controlpoints.shape()[0];
        let n1 = self.spline.controlpoints.shape()[1];
        let arr = Array2::<f64>::from_shape_vec((n0, n1), raveled.to_vec())?;
        let mut result = n_csr.todense().dot(&arr);

        // if the spline is rational, we apply the weights and omit the weights column
        if self.spline.rational {
            let wpos = result.shape()[1] - 1;
            let weights = &&result.slice(s![.., wpos]);
            let mut temp = Array2::<f64>::zeros((result.shape()[0], wpos));

            for i in 0..self.spline.dimension {
                {
                    let mut slice = temp.slice_mut(s![.., i]);
                    let update = result.slice(s![.., i]).to_owned() / weights.to_owned();
                    slice.assign(&update.view());
                }
            }
            result = temp;
        }

        Ok(result)
    }

    /// Extend the curve by merging another curve to the end of it.
    ///
    /// The curves are glued together in a C0 fashion with enough repeated
    /// knots. The function assumes that the end of this curve perfectly
    /// matches the start of the input curve.
    ///
    /// Obviously, neither curve can be periodic, which enables us
    /// to assume all knot vectors are open.
    ///
    /// ### parameters
    /// * curve: Another curve
    ///
    /// ### returns
    /// * self.spline.bases and self.spline.controlpoints are updated inplace
    pub fn append(&mut self, othercurve: Curve) -> Result<(), Error> {
        if self.spline.bases[0].periodic > -1 || othercurve.spline.bases[0].periodic > -1 {
            return Result::Err(Error::IncompatibleCurvesError);
        };

        let mut extending_curve = othercurve;

        // make sure both are in the same space, and (if needed) have rational weights
        self.spline
            .make_splines_compatible(&mut extending_curve.spline)?;
        let p1 = self.spline.order(0)?[0];
        let p2 = extending_curve.spline.order(0)?[0];

        if p1 < p2 {
            self.spline.raise_order(vec![p2 - p1])?;
        } else {
            extending_curve.spline.raise_order(vec![p1 - p2])?;
        }

        let p = max(p1, p2);

        let old_knot = self.spline.knots(0, Some(true))?[0].clone();
        let mut add_knot = extending_curve.spline.knots(0, Some(true))?[0].clone();
        add_knot -= add_knot[0];
        add_knot += old_knot[old_knot.len() - 1];

        let mut new_knot = Array1::<f64>::zeros(add_knot.len() + old_knot.len() - p - 1);
        {
            let mut slice = new_knot.slice_mut(s![..old_knot.len() - 1]);
            let update = old_knot.slice(s![..old_knot.len() - 1]);
            slice.assign(&update.view());
        }
        {
            let mut slice = new_knot.slice_mut(s![old_knot.len() - 1..]);
            let update = add_knot.slice(s![p..]);
            slice.assign(&update.view());
        }

        let rational = self.spline.rational as usize;
        let n1 = self.spline.controlpoints.shape()[0];
        let n2 = extending_curve.spline.controlpoints.shape()[0];
        let n3 = self.spline.dimension + rational;
        let mut new_controlpoints = Array2::<f64>::zeros((n1 + n2 - 1, n3));
        {
            let mut slice = new_controlpoints.slice_mut(s![..n1, ..]);
            let update = self.spline.controlpoints.slice(s![.., ..]);
            slice.assign(&update.view());
        }
        {
            let mut slice = new_controlpoints.slice_mut(s![n1.., ..]);
            let update = extending_curve.spline.controlpoints.slice(s![1.., ..]);
            slice.assign(&update.view());
        }

        self.spline.bases = vec![BSplineBasis::new(Some(p), Some(new_knot), None)?];
        self.spline.controlpoints = new_controlpoints.into_dyn().to_owned();

        Ok(())
    }

    /// Raise the polynomial order of the curve.
    ///
    /// ### parameters
    /// * amount: Number of times to raise the order
    pub fn raise_order(&mut self, amount: usize) -> Result<(), Error> {
        if amount == 0 {
            return Ok(());
        }

        // work outside of self, copy back in at the end
        let mut new_basis = self.spline.bases[0].clone();
        new_basis.raise_order(amount);

        // set up an interpolation problem. This is in projective space,
        // so no problems for rational cases
        // let old_controlpoints = self.spline.controlpoints.clone().into_dyn().to_owned();
        let mut interpolation_pts_t = new_basis.greville();
        let n_old = self.spline.bases[0].evaluate(&mut interpolation_pts_t, 0, true)?;
        let n_new = new_basis.evaluate(&mut interpolation_pts_t, 0, true)?;

        // Some kludge required to enable .dot(), which doesn't work on dynamic
        // arrays. Surely a better way to do this, but this is quick and dirty
        // and valid since we're in curve land
        let raveled = self.spline.controlpoints.clone().into_raw_vec();
        let n0 = self.spline.controlpoints.shape()[0];
        let n1 = self.spline.controlpoints.shape()[1];
        let arr = Array2::<f64>::from_shape_vec((n0, n1), raveled.to_vec())?;
        let interpolation_pts_x = n_old.todense().dot(&arr);

        // solve the interpolation problem:
        // more kludge; solve_into() only handles systems of the form Ax=b,
        // so we need to interate through the columns of B in AX=B instead
        let n_new_dense = n_new.todense().to_owned();
        let ncols = interpolation_pts_x.shape()[1];
        let mut temp = (0..ncols)
            .rev()
            .map(|e| {
                let b = interpolation_pts_x.slice(s![.., e]).to_owned();
                let sol = n_new_dense
                    .solve_into(b)
                    .map_err(|_| Error::UnsolvableSystemError)?;
                Ok(sol.to_vec())
            })
            .collect::<Result<Vec<_>, Error>>()?;

        let nrows = temp[0].len();
        let mut flattened = Vec::with_capacity(nrows * temp.len());

        for _ in 0..nrows {
            for vec in &mut temp {
                flattened.push(vec.pop().unwrap());
            }
        }

        flattened.reverse();

        let res = Array2::<f64>::from_shape_vec((nrows, ncols), flattened)?;

        self.spline.controlpoints = res.into_dyn().to_owned();
        self.spline.bases = vec![new_basis];

        Ok(())
    }

    // def length(self, t0=None, t1=None):
    //     """ Computes the euclidian length of the curve in geometric space

    //     .. math:: \\int_{t_0}^{t_1}\\sqrt{x(t)^2 + y(t)^2 + z(t)^2} dt

    //     """
    //     (x,w) = np.polynomial.legendre.leggauss(self.order(0)+1)
    //     knots = self.knots(0)
    //     # keep only integration boundaries within given start (t0) and stop (t1) interval
    //     if t0 is not None:
    //         i = bisect_left(knots, t0)
    //         knots = np.insert(knots, i, t0)
    //         knots = knots[i:]
    //     if t1 is not None:
    //         i = bisect_right(knots, t1)
    //         knots = knots[:i]
    //         knots = np.insert(knots, i, t1)

    //     t = np.array([ (x+1)/2*(t1-t0)+t0 for t0,t1 in zip(knots[:-1], knots[1:]) ])
    //     w = np.array([     w/2*(t1-t0)    for t0,t1 in zip(knots[:-1], knots[1:]) ])
    //     t = np.ndarray.flatten(t)
    //     w = np.ndarray.flatten(w)
    //     dx = self.derivative(t)
    //     detJ = np.sqrt(np.sum(dx**2, axis=1))
    //     return np.dot(detJ, w)
    pub fn length(&self) {
        todo!()
    }

    /// Computes the L2 (squared and per knot span) and max error between
    /// this curve and a target curve
    ///
    /// .. math:: ||\\boldsymbol{x_h}(t)-\\boldsymbol{x}(t)||_{L^2(t_1,t_2)}^2 = \\int_{t_1}^{t_2}
    ///     |\\boldsymbol{x_h}(t)-\\boldsymbol{x}(t)|^2 dt, \\quad \\forall \\;\\text{knots}\\;t_1 < t_2
    ///
    /// .. math:: ||\\boldsymbol{x_h}(t)-\\boldsymbol{x}(t)||_{L^\\infty} = \\max_t |\\boldsymbol{x_h}(t)-\\boldsymbol{x}(t)|
    ///
    /// ### parameters
    /// * function target: callable function which takes as input a vector \
    /// of evaluation points t and gives as output a matrix x where
    /// x[i,j] is component j evaluated at point t[i]
    ///
    /// ### returns
    /// * L2 error per knot-span
    pub fn error(&self, target: Curve) -> Result<Array1<f64>, Error> {
        let knots = &self.spline.knots(0, Some(false))?[0];
        let n = self.spline.order(0)?[0];
        let legendre = GaussLegendreQuadrature::new(n + 1)?;

        let mut error_l2 = Vec::with_capacity(knots.len() - 1);

        for (t0, t1) in knots.to_vec()[..knots.len() - 1]
            .iter()
            .zip(&mut knots.to_vec()[1..].iter())
        {
            let mut tg = (legendre.sample_points.clone() + 1.) / 2. * (*t1 - *t0) + *t0;
            let mut eval = vec![&mut tg];
            let wg = &legendre.weights / 2. * (t1 - t0);

            let error = self.evaluate(&mut eval)? - target.evaluate(&mut eval)?;
            let error_2 = (&error * &error).sum_axis(Axis(1));
            let error_val = error_2.dot(&wg);

            error_l2.push(error_val);
        }

        Ok(Array1::<f64>::from_vec(error_l2))
    }

    /// Get the L_infinity error. No error handling required as the default is 0f64
    pub fn max_error(&self, error_l2: &Array1<f64>) -> f64 {
        let mut vec = error_l2.to_vec()[..].iter().map(|e| e.sqrt()).collect::<Vec<_>>();
        vec.sort_by(cmp_f64);
        vec.reverse();

        let out;
        if vec[0] > 0f64 {
            out = vec[0];
        } else {
            out = 0f64;
        }
        
        out
    }
}

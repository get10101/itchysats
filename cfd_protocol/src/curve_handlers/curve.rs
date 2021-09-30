use crate::curve_handlers::basis::BSplineBasis;
use crate::curve_handlers::splineobject::SplineObject;
use crate::curve_handlers::Error;

use ndarray::prelude::*;
use ndarray::s;
use ndarray_linalg::Solve;
use std::cmp::max;

fn default_basis() -> Result<Vec<BSplineBasis>, Error> {
    let out = vec![BSplineBasis::new(None, None, None)?];
    Ok(out)
}

#[derive(Clone, Debug)]
pub struct Curve {
    spline: SplineObject,
}

impl Curve {
    /// Construct a curve with the given basis and control points.
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

    //     # solve the interpolation problem
    //     self.controlpoints = np.array(splinalg.spsolve(N_new, interpolation_pts_x))
    //     self.bases = [newBasis]

    //     return self

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
        let arr = Array2::<f64>::from_shape_vec((n0, n1), raveled)?;
        let interpolation_pts_x = n_old.todense().dot(&arr);

        // solve the interpolation problem:
        // more kludge; solve_into() only handles systems of the form Ax=b,
        // so we need to interate through the columns of B in AX=B instead
        let n_new_dense = n_new.todense().to_owned();
        let ncols = interpolation_pts_x.shape()[1];
        let mut temp = (0..ncols)
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

    pub fn length(&self) {
        todo!()
    }

    pub fn error(&self) {
        todo!()
    }
}

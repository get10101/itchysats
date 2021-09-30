use crate::curve_handlers::basis::BSplineBasis;
use crate::curve_handlers::splineobject::SplineObject;
// use crate::curve_handlers::csr_tools::CSR;
use crate::curve_handlers::Error;

use ndarray::prelude::*;
use ndarray::s;
use std::cmp::max;

fn default_basis() -> Result<Vec<BSplineBasis>, Error> {
    let out = vec![BSplineBasis::new(None, None, None)?];
    Ok(out)
}

#[derive(Clone, Debug)]
pub struct Curve {
    spline: SplineObject,
    // bases: Vec<BSplineBasis>,
    // controlpoints: ArrayD<f64>,
    // dimension: usize,
    // rational: bool,
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
        let mut bases = bases.unwrap_or(default_basis()?);
        let spline = SplineObject::new(bases, controlpoints, rational)?;
        // bases = spline.bases.clone();

        // let controlpoints = spline.controlpoints.clone().to_owned();
        // let dimension = spline.dimension.clone();
        // let rational = spline.rational.clone();

        Ok(Curve {
            spline,
            // bases,
            // controlpoints: spline.controlpoints.clone(),
            // dimension: spline.dimension.clone(),
            // rational: spline.rational.clone(),
        })
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
    pub fn append(&mut self, mut othercurve: Curve) -> Result<(), Error> {
        if self.spline.bases[0].periodic > -1 || othercurve.spline.bases[0].periodic > -1 {
            return Result::Err(Error::IncompatibleCurvesError);
        };

        // copy input curve so we don't change that one directly
        let mut extending_curve = othercurve.clone();

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

        let mut old_knot = self.spline.knots(0, Some(true))?[0].clone();
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

    pub fn knots(&self, direction: usize, with_multiplicities: bool) -> Result<Array1<f64>, Error> {
        todo!()
    }

    fn order(&self, direction: usize) {
        todo!()
    }

    fn raise_order(&self, amount: usize, direction: usize) -> Result<(), Error> {
        todo!()
    }

    pub fn length(&self) {
        todo!()
    }

    pub fn error(&self) {
        todo!()
    }
}

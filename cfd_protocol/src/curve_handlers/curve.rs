use crate::curve_handlers::basis::BSplineBasis;
use crate::curve_handlers::splineobject::SplineObject;
// use crate::curve_handlers::csr_tools::CSR;
use crate::curve_handlers::Error;

use ndarray::prelude::*;

fn default_basis() -> Result<Vec<BSplineBasis>, Error> {
    let out = vec![BSplineBasis::new(None, None, None)?];
    Ok(out)
}

#[derive(Clone, Debug)]
pub struct Curve {
    bases: Vec<BSplineBasis>,
    controlpoints: ArrayD<f64>,
    dimension: usize,
    rational: bool,
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

        Ok(Curve {
            bases: spline.bases,
            controlpoints: spline.controlpoints,
            dimension: spline.dimension,
            rational: spline.rational,
        })
    }

    pub fn append(&self) {
        todo!()
    }

    pub fn length(&self) {
        todo!()
    }

    pub fn error(&self) {
        todo!()
    }
}

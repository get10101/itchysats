use core::cmp::max;
use ndarray::{concatenate, s, Array1, Axis};

use crate::curve_handlers::basis_eval::{bisect_left, knot_tolerance, snap, Basis};
use crate::curve_handlers::csr_tools::CSR;
use crate::curve_handlers::Error;

#[derive(Clone, Debug)]
pub struct BSplineBasis {
    pub knots: Array1<f64>,
    pub order: usize,
    pub periodic: isize,
    pub knot_tol: f64,
}

impl BSplineBasis {
    pub fn new(
        order: Option<usize>,
        knots: Option<Array1<f64>>,
        periodic: Option<isize>,
    ) -> Result<Self, Error> {
        let ord = order.unwrap_or(2);
        let prd = periodic.unwrap_or(-1);
        let knt = match knots {
            Some(knots) => knots,
            None => default_knot(ord, prd)?,
        };
        let ktol = knot_tolerance(None);

        Ok(BSplineBasis {
            order: ord,
            knots: knt,
            periodic: prd,
            knot_tol: ktol,
        })
    }

    pub fn num_functions(&self) -> usize {
        let p = (self.periodic + 1) as usize;

        self.knots.len() - self.order - p
    }

    /// Start point of parametric domain. For open knot vectors, this is the
    /// first knot.
    pub fn start(&self) -> f64 {
        self.knots[self.order - 1]
    }

    /// End point of parametric domain. For open knot vectors, this is the
    /// last knot.
    pub fn end(&self) -> f64 {
        self.knots[self.knots.len() - self.order]
    }

    /// Fetch greville points, also known as knot averages
    /// over entire knot vector:
    /// .. math:: \\sum_{j=i+1}^{i+p-1} \\frac{t_j}{p-1}
    pub fn greville(&self) -> Array1<f64> {
        let n = self.num_functions() as i32;

        (0..n).map(|idx| self.greville_single(idx)).collect()
    }

    fn greville_single(&self, index: i32) -> f64 {
        let p = self.order as i32;
        let den = (self.order - 1) as f64;

        self.knots.slice(s![index + 1..index + p]).sum() / den
    }

    /// Evaluate all basis functions in a given set of points.
    /// ## parameters:
    /// * t: The parametric coordinate(s) in which to evaluate
    /// * d: Number of derivatives to compute
    /// * from_right: true if evaluation should be done in the limit from above
    /// ## returns:
    /// * CSR (sparse) matrix N\[i,j\] of all basis functions j evaluated in all points j
    pub fn evaluate(&self, t: &mut Array1<f64>, d: usize, from_right: bool) -> Result<CSR, Error> {
        let basis = Basis::new(
            self.order,
            self.knots.clone().to_owned(),
            Some(self.periodic),
            Some(self.knot_tol),
        );
        snap(t, &basis.knots, Some(basis.ktol));

        if self.order <= d {
            let csr = CSR::new(
                Array1::<f64>::zeros(0),
                Array1::<usize>::zeros(0),
                Array1::<usize>::zeros(t.len() + 1),
                (t.len(), self.num_functions()),
            )?;

            return Ok(csr);
        }

        let out = basis.evaluate(t, d, Some(from_right))?;

        Ok(out)
    }

    /// Snap evaluation points to knots if they are sufficiently close
    /// as given in by knot_tolerance.
    ///
    /// * t: The parametric coordinate(s) in which to evaluate
    pub fn snap(&self, t: &mut Array1<f64>) {
        snap(t, &self.knots, Some(self.knot_tol))
    }
}

fn default_knot(order: usize, periodic: isize) -> Result<Array1<f64>, Error> {
    let prd = max(periodic, -1);
    let p = (prd + 1) as usize;
    let mut knots = concatenate(
        Axis(0),
        &[
            Array1::<f64>::zeros(order).view(),
            Array1::<f64>::ones(order).view(),
        ],
    )
    .unwrap();

    for i in 0..p {
        knots[i] = -1.;
        knots[2 * order - i - 1] = 2.;
    }

    Ok(knots)
}

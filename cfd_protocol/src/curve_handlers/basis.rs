use core::cmp::max;
use ndarray::prelude::*;
use ndarray::{concatenate, s};

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

    // def raise_order(self, amount):

    //     """
    //     if type(amount) is not int:
    //         raise TypeError('amount needs to be a non-negative integer')
    //     if amount < 0:
    //         raise ValueError('amount needs to be a non-negative integer')
    //     if amount == 0:
    //         return self.clone()
    //     knot_spans = list(self.knot_spans(True))  # list of unique knots
    //

    /// Create a knot vector with higher order.
    ///
    /// The continuity at the knots are kept unchanged by increasing their
    /// multiplicities.
    ///
    /// ### parameters
    /// * amount: relative (polynomial) degree to raise the basis function by
    pub fn raise_order(&mut self, amount: usize) -> Result<(), Error> {
        if amount > 0 {
            let knot_spans = get_unique_f64(&self.knots.clone().into_dyn().to_owned())?;
            // knot_spans = list(self.knot_spans(True))  # list of unique knots
            // # For every degree we raise, we need to increase the multiplicity by one
            // knots = list(self.knots) + knot_spans * amount
            // # make it a proper knot vector by ensuring that it is non-decreasing
            // knots.sort()
            // if self.periodic > -1:
            //     # remove excessive ghost knots which appear at both ends of the knot vector
            //     n0 =                   bisect_left(knot_spans, self.start())
            //     n1 = len(knot_spans) - bisect_left(knot_spans, self.end())   - 1
            //     knots = knots[n0*amount : -n1*amount]

            // return BSplineBasis(self.order + amount, knots, self.periodic)
        };

        Ok(())
    }

    /// Return the set of unique knots in the knot vector.
    ///
    /// ### parameters:
    /// * include_ghosts: if knots outside start/end are to be included. These \
    /// knots are used by periodic basis.
    ///
    /// ### returns:
    /// * 1-D array of unique knots
    pub fn knot_spans(&self, include_ghosts: bool) -> Array1<f64> {
        let p = &self.order;

        // TODO: this is VERY sloppy!
        let mut res: Vec<f64> = vec![];
        if include_ghosts {
            res.push(self.knots[0]);
            for elem in self.knots.slice(s![1..]).iter() {
                if (elem - res[res.len() - 1]).abs() > self.knot_tol {
                    res.push(*elem);
                }
            }
        } else {
            res.push(self.knots[p - 1]);
            for elem in self.knots.slice(s![p - 1..p + 1]).iter() {
                if (elem - res[res.len() - 1]).abs() > self.knot_tol {
                    res.push(*elem);
                }
            }
        }

        Array1::<f64>::from_vec(res)
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

fn get_unique_f64(arr: &ArrayD<f64>) -> Result<Array1<f64>, Error> {
    let mut tmp = arr.iter().copied().collect::<Vec<f64>>();
    tmp.sort_by(|a, b| a.partial_cmp(b).expect("No NaNs"));
    tmp.dedup();

    let out = Array1::<f64>::from_vec(tmp);

    Ok(out)
}

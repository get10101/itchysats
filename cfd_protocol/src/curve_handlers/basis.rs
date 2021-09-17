use crate::curve_handlers::basis_eval::Basis;
use crate::curve_handlers::csr_tools::CSR;
use ndarray::{s, Array1};

pub struct BSplineBasis {
    knots: Array1<f64>,
    order: usize,
    periodic: isize,
}

impl BSplineBasis {
    pub fn new(order: usize, knots: Array1<f64>, periodic: Option<isize>) -> Self {
        BSplineBasis {
            order,
            knots,
            periodic: periodic.unwrap_or(-1),
        }
    }

    fn num_functions(&self) -> usize {
        let p = (self.periodic + 1) as usize;

        self.knots.len() - self.order - p
    }

    /// Start point of parametric domain. For open knot vectors, this is the
    /// first knot.
    fn start(&self) -> f64 {
        self.knots[self.order - 1]
    }

    /// End point of parametric domain. For open knot vectors, this is the
    /// last knot.
    fn end(&self) -> f64 {
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
    ///
    /// :param t: The parametric coordinate(s) in which to evaluate
    /// :param d: Number of derivatives to compute
    /// :param from_right: true if evaluation should be done in the limit
    ///     from above
    /// :return: tuple:
    ///      - the (possibly modified) coordinate vector t,
    ///      - A CSR (sparse) matrix *N[i,j]* of all basis functions *j*
    ///        evaluated in all points *i*
    pub fn evaluate(&self, t: &Array1<f64>, d: usize, from_right: bool) -> (Array1<f64>, CSR) {
        let basis = Basis::new(
            self.order,
            self.knots.clone().to_owned(),
            Some(self.periodic),
            None,
        );
        let out = basis.snap(t);

        if self.order <= d {
            let data = Array1::<f64>::zeros(0);
            let indices = Array1::<usize>::zeros(0);
            let indptr = Array1::<usize>::zeros(out.len() + 1);
            let shape = (out.len(), self.num_functions());

            return (out, CSR::new(data, indices, indptr, shape));
        }

        basis.evaluate(&out, d, Some(from_right))
    }
}

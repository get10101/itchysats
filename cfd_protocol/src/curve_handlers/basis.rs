use crate::curve_handlers::basis_tools::Basis;
use ndarray::{s, Array1, Array2};

pub struct BSplineBasis {
    knots: Array1<f64>,
    order: usize,
    periodic: isize,
}

impl BSplineBasis {
    fn new(order: usize, knots: Vec<f64>, periodic: Option<isize>) -> Self {
        let p = periodic.unwrap_or_else(|| -1);
        BSplineBasis {
            order,
            knots: Array1::from_vec(knots),
            periodic: periodic.unwrap_or_else(|| -1),
        }
    }

    fn num_functions(&self) -> usize {
        let p = (self.periodic + 1) as usize;

        self.knots.len() - self.order - p
    }

    fn start(&self) -> f64 {
        // Start point of parametric domain. For open knot vectors, this is the
        // first knot.
        self.knots[self.order - 1]
    }

    fn end(&self) -> f64 {
        // End point of parametric domain. For open knot vectors, this is the
        // last knot.
        self.knots[self.knots.len() - self.order]
    }

    fn greville_single(&self, index: i32) -> f64 {
        // Fetch greville point,also known as knot averages
        // at a single knot index:
        // .. math:: \\sum_{j=i+1}^{i+p-1} \\frac{t_j}{p-1}
        let p = self.order as i32;
        let den = (self.order - 1) as f64;

        self.knots.slice(s![index + 1..index + p]).sum() / den
    }

    pub fn greville(&self) -> Array1<f64> {
        // Fetch greville points, also known as knot averages
        // over entire knot vector:
        // .. math:: \\sum_{j=i+1}^{i+p-1} \\frac{t_j}{p-1}
        let n = self.num_functions() as i32;

        (0..n).map(|idx| self.greville_single(idx)).collect()
    }

    pub fn evaluate(&self, t: &Array1<f64>, d: usize, from_right: bool) -> Array2<f64> {
        // Evaluate all basis functions in a given set of points.
        //
        // :param t: The parametric coordinate(s) in which to evaluate
        // :param d: Number of derivatives to compute
        // :param from_right: true if evaluation should be done in the limit
        //     from above
        // :return: tuple:
        //      - the (possibly modified) coordinate vector t,
        //      - A CSR (sparse) matrix *N[i,j]* of all basis functions *j*
        //        evaluated in all points *i*,
        //      - the size of N
        let basis = Basis::new(self.order, self.knots, Some(self.periodic), None);
        let mut out = basis.snap(&t);

        if self.order <= d {
            let csr = (
                Array1::<f64>::zeros(0),
                Array1::<usize>::zeros(0),
                Array1::<usize>::zeros(out.len() + 1),
            );
            let size = (out.len(), self.num_functions());
        } else {
            let (out, csr, size) = basis.evaluate(&out, d, from_right);
        };

        (out, csr, size)
    }
}

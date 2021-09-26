use crate::curve_handlers::basis::BSplineBasis;
use crate::curve_handlers::csr_tools::CSR;
use crate::curve_handlers::curve::Curve;
use ndarray::{stack, Array1, Array2, Axis};

/// Perform general spline interpolation on a provided basis.
///
/// ### parameters
/// * x: Matrix *X\[i,j\]* of interpolation points *x_i* with components *j*
/// * basis: Basis on which to interpolate
/// * t: parametric values at interpolation points; defaults to \
/// Greville points if not provided
///
/// ### returns
/// * Interpolated curve
pub fn interpolate(x: Array2<f64>, basis: BSplineBasis, pts: Option<Array1<f64>>) -> Curve {
    todo!()
    // let t = pts.unwrap_or(basis.greville());
    // let (t_pts, csr) = basis.evaluate(&t, 0, true);

    // // ah crap! I need to use SuperLU or some other solver here,
    // // which is not implemented in rust yet!

    // Curve::new(basis, cp);
}

/// Computes an interpolation for a parametric curve up to a specified
/// tolerance. The method will iteratively refine parts where needed
/// resulting in a non-uniform knot vector with as optimized knot
/// locations as possible.
///
/// ### parameters
/// * x: callable function which takes as input a vector of \
/// evaluation points t and gives as output a matrix x where \
/// x\[i,j\] is component j evaluated at point t\[i\]
/// * t0: start of parametric domain
/// * t1: end of parametric domain
/// * rtol: relative tolerance for stopping criterium. It is defined \
/// to be ||e||_L2 / D, where D is the length of the curve and \
/// ||e||_L2 is the L2-error (see Curve.error)
/// * atol: absolute tolerance for stopping criterium. It is defined \
/// to be the maximal distance between the curve approximation and \
/// the exact curve
///
/// ### returns
/// Curve (NURBS)
fn fit() {
    todo!()
}

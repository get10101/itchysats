use crate::curve_handlers::basis::BSplineBasis;
use crate::curve_handlers::curve::Curve;
use crate::curve_handlers::Error;

use ndarray::prelude::*;
use ndarray_linalg::Solve;

/// Perform general spline interpolation on a provided basis.
///
/// ### parameters
/// * x: Matrix *X\[i,j\]* of interpolation points *x_i* with components *j*
/// * basis: Basis on which to interpolate
/// * t: parametric values at interpolation points; defaults to
/// Greville points if not provided
///
/// ### returns
/// * Interpolated curve
pub fn interpolate(
    x: &Array2<f64>,
    basis: &BSplineBasis,
    t: Option<Array1<f64>>,
) -> Result<Curve, Error> {
    let mut t = t.unwrap_or_else(|| basis.greville());
    let evals = basis.evaluate(&mut t, 0, true)?;

    // solve the interpolation problem:
    // more kludge; solve_into() only handles systems of the form Ax=b,
    // so we need to interate through the columns of B in AX=B instead
    let evals_dense = evals.todense().to_owned();
    let ncols = x.shape()[1];
    let mut temp = (0..ncols)
        .rev()
        .map(|e| {
            let b = x.slice(s![.., e]).to_owned();
            let sol = evals_dense
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

    let controlpoints = Array2::<f64>::from_shape_vec((nrows, ncols), flattened)?.to_owned();
    let out = Curve::new(Some(vec![basis.clone()]), Some(controlpoints), None)?;

    Ok(out)
}

/// Computes an interpolation for a parametric curve up to a specified
/// tolerance. The method will iteratively refine parts where needed
/// resulting in a non-uniform knot vector with as optimized knot
/// locations as possible.
///
/// ### parameters
/// * x: callable function which takes as input a vector of
/// evaluation points t and gives as output a matrix x where
/// x\[i,j\] is component j evaluated at point t\[i\]
/// * t0: start of parametric domain
/// * t1: end of parametric domain
/// * rtol: relative tolerance for stopping criterium. It is defined
/// to be ||e||_L2 / D, where D is the length of the curve and
/// ||e||_L2 is the L2-error (see Curve.error)
/// * atol: absolute tolerance for stopping criterium. It is defined
/// to be the maximal distance between the curve approximation and
/// the exact curve
///
/// ### returns
/// Curve (NURBS)
fn fit() {
    todo!()
    // b = BSplineBasis(4, [t0,t0,t0,t0, t1,t1,t1,t1])
    // t = np.array(b.greville())
    // crv = interpolate(x(t), b, t)
    // (err2, maxerr) = crv.error(x)
    // # polynomial input (which can be exactly represented) only use one knot span
    // if maxerr < 1e-13:
    //     return crv

    // # for all other curves, start with 4 knot spans
    // knot_vector = [t0,t0,t0,t0] + [i/5.0*(t1-t0)+t0 for i in range(1,5)] + [t1,t1,t1,t1]
    // b = BSplineBasis(4, knot_vector)
    // t = np.array(b.greville())
    // crv = interpolate(x(t), b, t)
    // (err2, maxerr) = crv.error(x)
    // # this is technically false since we need the length of the target function *x*
    // # and not our approximation *crv*, but we don't have the derivative of *x*, so
    // # we can't compute it. This seems like a healthy compromise
    // length = crv.length()
    // while np.sqrt(np.sum(err2))/length > rtol and maxerr > atol:
    //     knot_span    = crv.knots(0) # knot vector without multiplicities
    //     target_error = (rtol*length)**2 / len(err2) # equidistribute error among all knot spans
    //     refinements  = []
    //     for i in range(len(err2)):
    //         # figure out how many new knots we require in this knot interval:
    //         # if we converge with *scale* and want an error of *target_error*
    //         # |e|^2 * (1/n)^scale = target_error^2

    //         conv_order = 4                   # cubic interpolateion is order=4
    //         square_conv_order = 2*conv_order # we are computing with square of error
    //         scale = square_conv_order + 4    # don't want to converge too quickly in case of highly non-uniform mesh refinement is required
    //         n = int(np.ceil(np.exp((np.log(err2[i]) - np.log(target_error))/scale)))

    //         # add *n* new interior knots to this knot span
    //         new_knots = np.linspace(knot_span[i], knot_span[i+1], n+1)
    //         knot_vector = knot_vector + list(new_knots[1:-1])

    //     # build new refined knot vector
    //     knot_vector.sort()
    //     b = BSplineBasis(4, knot_vector)
    //     # do interpolation and return result
    //     t = np.array(b.greville())
    //     crv = interpolate(x(t), b, t)
    //     (err2, maxerr) = crv.error(x)
    //     length = crv.length()

    // return crv
}

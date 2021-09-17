use crate::curve_handlers::basis::BSplineBasis;
use crate::curve_handlers::csr_tools::CSR;
use ndarray::{s, Array1};

/// Perform general spline interpolation on a provided basis.
/// :param matrix-like x: Matrix *X[i,j]* of interpolation points *xi* with
///	components *j*
/// :param BSplineBasis basis: Basis on which to interpolate
/// :param array-like t: parametric values at interpolation points; defaults to
///	Greville points if not provided
/// :return: Interpolated curve
/// :rtype: Curve
fn interpolate(x: Array2<f64>, basis: BSplineBasis, pts: Option<Array1<f64>>) -> Curve {
    let t = pts.unwrap_or(basis.greville());
    let (t_pts, csr) = basis.evaluate(&t, 0, true);

    // ah crap! I need to use SuperLU or some other solver here,
    // which is not implemented in rust yet!
    //
    // this is the splipy implementation:
    // cp = splinalg.spsolve(csr, t_pts)
    // cp = cp.reshape(t_pts.shape)
    // return Curve(basis, cp)
    //
    // where the part of splinalg.spsolver(csr, t_pts) that
    // gets used is:
    // if not b_is_sparse:
    //     if isspmatrix_csc(A):
    //         flag = 1  # CSC format
    //     else:
    //         flag = 0  # CSR format
    //
    //     options = dict(ColPerm=permc_spec)
    //     x, info = _superlu.gssv(N, A.nnz, A.data, A.indices, A.indptr,
    //                             b, flag, options=options)
    //     if info != 0:
    //         warn("Matrix is exactly singular", MatrixRankWarning)
    //         x.fill(np.nan)
    //     if b_is_vector:
    //         x = x.ravel()
    //
    // as a standin while I work out an FFI for SuperLU we just make
    // cp an array of ones
    let cp = Array1::<f64>::ones(t_pts.len());

    Curve::new(basis, cp);
}

fn fit() {
    todo!()
}

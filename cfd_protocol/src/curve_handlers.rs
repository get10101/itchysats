use ndarray_linalg::error::LinalgError;
use thiserror::Error;

mod basis;
mod basis_eval;
mod csr_tools;
mod curve;
mod curve_factory;
mod splineobject;
mod utils;

#[derive(Error, Debug)]
pub enum Error {
    #[error("failed to init CSR object--is the specified shape correct?")]
    CSRInitError,
    #[error("interpolation failed; singular system?")]
    InterpolationError,
    #[error("matrix must be square")]
    UnsolvableSystemError,
    #[error("evaluation outside parametric domain")]
    InvalidDomainError,
    #[error("parameters arrays must have same length")]
    InvalidEvaluationError,
    #[error("einsum error--array size mismatch?")]
    EinsumError,
    #[error("no operand string found")]
    EinsumOperandError,
    #[error("concatonation error")]
    NdArray {
        #[from]
        source: ndarray::ShapeError,
    },
    #[error("cannot connect periodic curves")]
    IncompatibleCurvesError,
    #[error("analogue of np.insert not available yet; define spline as rational")]
    FeatureNotImplementedError,
    #[error("Ambiguous specification of `raises` vector")]
    InvalidRaisesValueError,
    #[error("attempting to evaluate spline outside of support")]
    OutOfRangeError,
    #[error("a `periodic` value < -1 makes no sense")]
    IvalidPeriodicValueError,
    #[error("cannot invert matrix")]
    CannotInvertMatrix(#[source] LinalgError),
}

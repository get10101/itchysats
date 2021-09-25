use thiserror::Error;

mod basis;
mod basis_eval;
mod csr_tools;
mod curve;
mod curve_factory;
mod splineobject;

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
    #[error("concatonation error")]
    NdArray {
        #[from]
        source: ndarray::ShapeError,
    },
    #[error("einsum error--array size mismatch?")]
    EinsumError,
}

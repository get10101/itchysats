use crate::curve_handlers::Error;

use ndarray::prelude::*;
use ndarray_linalg::Solve;
use std::ops::Mul;

/// NOTE:
/// This struct is provided here in this form as nalgebra_sparse
/// is rather embryonic and incluldes (basically) no solvers
/// at present. As we only need to be able construct as CSR
/// matrix and perform multiplication with a (dense) vector, it
/// seemed to make more sense to define our own rudementary CSR struct
/// and avoid the bloat of using a crate that will introduce
/// breaking changes regularly, and we need to write our own
/// solver regardless.
#[derive(Clone, Debug, PartialEq)]
pub struct CSR {
    pub data: Array1<f64>,
    pub indices: Array1<usize>,
    pub indptr: Array1<usize>,
    pub shape: (usize, usize),
    pub nnz: usize,
}

impl CSR {
    pub fn new(
        data: Array1<f64>,
        indices: Array1<usize>,
        indptr: Array1<usize>,
        shape: (usize, usize),
    ) -> Result<Self, Error> {
        let major_dim: isize = (indptr.len() as isize) - 1;
        // let minor_dim = indices.iter().max().unwrap_or(&0) + 1;
        let nnz = &data.len();

        if major_dim > 1 && shape.0 as isize == major_dim {
            Result::Ok(CSR {
                data,
                indices,
                indptr,
                shape,
                nnz: *nnz,
            })
        } else {
            Result::Err(Error::CSRInitError)
        }
    }

    /// there is no rust implementation of SuperLU or other
    /// "standard" sparse solvers around, so we will simply
    /// use the LAPACK/BLAS bindings provided by ndarrray-linalg
    pub fn solve(&self, b_vec: &Array1<f64>) -> Result<Array1<f64>, Error> {
        if self.shape.0 != self.shape.1 {
            return Err(Error::UnsolvableSystemError);
        }
        let a = self.todense();
        let b = b_vec.clone().to_owned();
        let x = a.solve_into(b).map_err(|_| Error::UnsolvableSystemError)?;

        Ok(x)
    }

    pub fn todense(&self) -> Array2<f64> {
        let mut out = Array2::<f64>::zeros(self.shape);
        for i in 0..self.shape.0 {
            for j in self.indptr[i]..self.indptr[i + 1] {
                out[[i, self.indices[j]]] += self.data[j];
            }
        }

        out
    }
}

impl Mul<&Array1<f64>> for CSR {
    type Output = Array1<f64>;

    fn mul(self, rhs: &Array1<f64>) -> Array1<f64> {
        let mut out = Array1::<f64>::zeros(self.shape.0);
        for i in 0..self.shape.0 {
            for j in self.indptr[i]..self.indptr[i + 1] {
                out[i] += self.data[j] * rhs[self.indices[j]];
            }
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positive_csr_test() {
        let a = CSR::new(
            Array1::<f64>::from_vec(vec![11., 12., 22., 23., 31., 33.]),
            Array1::<usize>::from_vec(vec![0, 1, 1, 2, 0, 2]),
            Array1::<usize>::from_vec(vec![0, 2, 4, 6]),
            (3, 3),
        )
        .unwrap();

        let b = Array1::<f64>::from_vec(vec![47., 43., -2.]);
        let x = &a.solve(&b).unwrap();
        let exact = Array1::<f64>::from_vec(vec![1., 3., -1.]);

        assert!(x.abs_diff_eq(&exact, 1e-9));
    }

    #[test]
    fn negative_csr_test_00() {
        let a = CSR::new(
            Array1::<f64>::zeros(0),
            Array1::<usize>::zeros(0),
            Array1::<usize>::zeros(11),
            (1, 3),
        )
        .unwrap_err();

        assert!(matches!(a, Error::CSRInitError));
    }

    #[test]
    fn negative_csr_test_01() {
        let a = CSR::new(
            Array1::<f64>::from_vec(vec![11., 12., 22., 23., 31., 33., 42.]),
            Array1::<usize>::from_vec(vec![0, 1, 1, 2, 0, 2, 1]),
            Array1::<usize>::from_vec(vec![0, 2, 4, 6, 7]),
            (4, 3),
        )
        .unwrap();

        let b = Array1::<f64>::ones(3);
        let res = &a.solve(&b).unwrap_err();

        assert!(matches!(res, Error::UnsolvableSystemError));
    }

    #[test]
    fn negative_csr_test_02() {
        let a = CSR::new(
            Array1::<f64>::from_vec(vec![11., 12., 22., 23.]),
            Array1::<usize>::from_vec(vec![0, 1, 1, 2]),
            Array1::<usize>::from_vec(vec![0, 2, 4, 4]),
            (3, 3),
        )
        .unwrap();

        let b = Array1::<f64>::ones(3);
        let res = &a.solve(&b).unwrap_err();

        assert!(matches!(res, Error::UnsolvableSystemError));
    }
}

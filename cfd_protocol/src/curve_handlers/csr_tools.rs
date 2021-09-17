use nalgebra::{DMatrix, DVector};
use ndarray::{Array1, Array2};
use std::ops::Mul;

pub struct Error {}

/// NOTE:
/// This struct is provided here in this form as nalgebra_sparse
/// is rather embryonic and incluldes (basically) no solvers
/// at present. As we only need to be able construct as CSR
/// matrix and perform multiplication with a (dense) vector, it
/// seemed to make more sense to define our own rudementary CSR struct
/// and avoid the bloat of using a crate that will introduce
/// breaking changes regularly, and we need to write our own
/// solver regardless.
pub struct CSR {
    data: Array1<f64>,
    indices: Array1<usize>,
    indptr: Array1<usize>,
    shape: (usize, usize),
    nnz: usize,
}

impl CSR {
    pub fn new(
        data: Array1<f64>,
        indices: Array1<usize>,
        indptr: Array1<usize>,
        shape: (usize, usize),
    ) -> Result<Self, Error> {

        if indptr.len() == shape.0 + 1 {
            Result::Ok(CSR {
                data,
                indices,
                indptr,
                shape,
                nnz: data.len(),
            })
        } else {
            Result::Err(Error {})
        }
    }

    /// there is no rust implementation of SuperLU or other
    /// standard sparse solvers around, so we will simply
    /// wrap nalgebra-lapack::lu::solve
    pub fn dense_solve(&self, b_vec: Array1<f64>) -> Array1<f64> {
        let dense = self.todense();

        // convert to nalgebra objects
        let a = DMatrix::<f64>::from_vec(self.shape.0, self.shape.1, dense.into_raw_vec());
        let mut b = DVector::<f64>::from_vec(b_vec.to_vec());
        let res = a.lu().solve(&mut b).unwrap();

        // convert back to an ndarray object for consistency
        Array1::<f64>::from_vec(res.data.into())
    }

    fn todense(&self) -> Array2<f64> {
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
        let mut out = Array1::<f64>::zeros(self.nnz);
        for i in 0..self.nnz {
            for j in self.indptr[i]..self.indptr[i + 1] {
                out[i] += self.data[i] * rhs[self.indices[j]];
            }
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// happy-path test
    fn positive_csr_test() {
        let data = Array1::<f64>::from_vec(vec![4., 3.]);
        let indices = Array1::<usize>::from_vec(vec![0, 2]);
        let indptr = Array1::<usize>::from_vec(vec![0, 1, 1, 2]);
        let shape = (3, 3);

        let A = CSR::new(data, indices, indptr, shape).unwrap();
        let x = Array1::<f64>::from_vec(vec![1., 2., 3.]);

        let b = A * x;
        let b_expected = Array1::<f64>::from_vec(vec![4., 0., 9.]);


        assert!(A.data == data);
        assert!(A.indices == indices);
        assert!(A.indptr == indptr);
        assert!(A.shape == shape);
        assert!(A.nnz == data.len());
        assert!(b == b_expected);
    }

    #[test]
    /// attempt to create an invalid CSR matrix
    fn negative_csr_test() {
        let data = Array1::<f64>::zeros(0);
        let indices = Array1::<usize>::zeros(0);
        let indptr = Array1::<usize>::zeros(11);
        let shape = (1, 3);

        // this will error out
        let A = CSR::new(data, indices, indptr, shape).unwrap_err();
    }
}
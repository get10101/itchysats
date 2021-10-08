use crate::payout_curve::compat::{To1DArray, ToNAlgebraMatrix};
use crate::payout_curve::Error;
use ndarray::prelude::*;
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
#[allow(clippy::upper_case_acronyms)]
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
            Result::Err(Error::CannotInitCSR)
        }
    }

    // matrix version of `solve()`; useful for solving AX = B. Implementation
    // is horrible.
    pub fn matrix_solve(&self, b_arr: &Array2<f64>) -> Result<Array2<f64>, Error> {
        let a_arr = self.todense();
        let ncols = b_arr.shape()[1];
        let mut temp = (0..ncols)
            .rev()
            .map(|e| {
                let b = b_arr.slice(s![.., e]).to_owned();

                let sol = lu_solve(&a_arr, &b).unwrap();
                Ok(sol.to_vec())
            })
            .collect::<Result<Vec<_>, Error>>()?;

        let nrows = temp[0].len();
        let mut raveled = Vec::with_capacity(nrows * temp.len());

        for _ in 0..nrows {
            for vec in &mut temp {
                raveled.push(vec.pop().unwrap());
            }
        }

        raveled.reverse();

        let result = Array2::<f64>::from_shape_vec((nrows, ncols), raveled)?.to_owned();

        Ok(result)
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

fn lu_solve(a: &Array2<f64>, b: &Array1<f64>) -> Result<Array1<f64>, Error> {
    let a = a.to_nalgebra_matrix().lu();
    let b = b.to_nalgebra_matrix();

    let x = a
        .solve(&b)
        .ok_or(Error::MatrixMustBeSquare)?
        .to_1d_array()?;

    Ok(x)
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
    fn test_lu_solve() {
        let a = Array2::<f64>::from(vec![[11., 12., 0.], [0., 22., 23.], [31., 0., 33.]]);
        let b = Array1::<f64>::from_vec(vec![35., 113., 130.]);
        let x_expected = Array1::<f64>::from_vec(vec![1., 2., 3.]);

        let x = lu_solve(&a, &b).unwrap();

        for (x, expected) in x.into_iter().zip(x_expected) {
            assert!(
                (x - expected).abs() < f64::EPSILON * 10.,
                "{} {}",
                x,
                expected
            )
        }
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

        assert!(matches!(a, Error::CannotInitCSR));
    }
}

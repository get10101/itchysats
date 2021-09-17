use ndarray::Array1;

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
        nnz: usize,
    ) -> Self {
        CSR {
            data,
            indices,
            indptr,
            shape,
            nnz: data.len(),
        }
    }

    pub fn spsolve() {
        todo!()
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

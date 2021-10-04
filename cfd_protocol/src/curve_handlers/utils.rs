use crate::curve_handlers::basis::BSplineBasis;
use crate::curve_handlers::Error;

use ndarray::prelude::*;
use ndarray::{concatenate, s, Order};
use num::{One, Zero};
use std::cmp::Ordering;
use std::f64::consts::PI;
use std::ops::{AddAssign, MulAssign};

pub fn bisect_left(arr: &Array1<f64>, val: &f64, mut hi: usize) -> usize {
    let mut lo: usize = 0;
    while lo < hi {
        let mid = (lo + hi) / 2;
        if arr[mid] < *val {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }

    lo
}

pub fn bisect_right(arr: &Array1<f64>, val: &f64, mut hi: usize) -> usize {
    let mut lo: usize = 0;
    while lo < hi {
        let mid = (lo + hi) / 2;
        if *val < arr[mid] {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }

    lo
}

pub fn get_unique_f64(arr: &ArrayD<f64>) -> Result<Array1<f64>, Error> {
    let mut tmp = arr.iter().copied().collect::<Vec<f64>>();
    tmp.sort_by(|a, b| a.partial_cmp(b).expect("No NaNs"));
    tmp.dedup();

    let out = Array1::<f64>::from_vec(tmp);

    Ok(out)
}

pub fn cmp_f64(a: &f64, b: &f64) -> Ordering {
    if a < b {
        return Ordering::Less;
    } else if a > b {
        return Ordering::Greater;
    }
    Ordering::Equal
}

/// Implementation of method in "Efficient Degree Elevation and Knot Insertion
/// for B-spline Curves using Derivatives" by  Qi-Xing Huang, Shi-Min Hu, and
/// Ralph R. Martin. [DOI:10.1080/16864360.2004.10738318](http://www.cad-journal.net/files/vol_1/Vol1Nos1-4.html).
///
/// Only the case of open knot vector is fully implemented.
///
/// ### parameters
/// * n: (n+1) is the number of initial basis functions
/// * k: spline order
/// * t: knot vector
/// * p: weighted NURBS coefficients
/// * m: number of degree elevations
/// * periodic: Number of continuous derivatives at start and end; \
/// -1 is not periodic, 0 is continuous, etc.
///
/// ### returns
/// * new control points
pub fn raise_order_1d(
    mut n: usize,
    k: usize,
    t: Array1<f64>,
    p: ArrayD<f64>,
    m: usize,
    periodic: isize,
) -> Result<ArrayD<f64>, Error> {
    if periodic < -1 {
        return Result::Err(Error::IvalidPeriodicValueError);
    }
    let u = get_unique_f64(&t.slice(s![k - 1..k + 1]).into_dyn().to_owned())?;
    let s = u.shape().iter().product::<usize>() - 1;
    let d = p.shape()[0];

    // Find multiplicity of the knot vector t
    let b = BSplineBasis::new(Some(k), Some(t.clone()), None)?;
    let z = b
        .knot_spans(false)
        .iter()
        .map(|x| (k as isize) - 1 - b.continuity(*x).unwrap())
        .collect::<Vec<_>>();

    // Step 1: Find Pt_i^j
    let pu = (periodic + 1) as usize;
    let mut pt_ij_temp = Array3::<f64>::zeros((d, n + 1, k));
    {
        let mut slice = pt_ij_temp.slice_mut(s![.., .., 0]);
        slice += &p.view();
    }
    let mut pt_ij = concatenate(
        Axis(1),
        &[pt_ij_temp.view(), pt_ij_temp.slice(s![.., 0..pu, ..])],
    )?;
    n += pu;

    for l in 1..k {
        for i in 0..n + 1 - l {
            if t[i + l] < t[i + k] {
                // oooh this is ugly!
                let mut temp_slice = pt_ij.clone().to_owned();
                let mut temp_update = pt_ij.clone().to_owned();
                {
                    let mut slice = temp_slice.slice_mut(s![.., i, l]);
                    let mut update = temp_update.slice_mut(s![.., i + 1, l - 1]).to_owned();
                    update = update - temp_update.slice_mut(s![.., i, l - 1]).to_owned();
                    update /= t[i + k] - t[i + l];
                    slice.assign(&update);
                }
                pt_ij = temp_slice;
            }
        }
    }

    // Step 2: Create new knot vector Tb
    let nb = n + s * m;
    let mut tb = Array1::<f64>::zeros(nb + m + k + 1);
    {
        let mut slice = tb.slice_mut(s![..k - 1]);
        slice += &t.slice(s![..k - 1]).to_owned().view();
    }
    {
        let mut slice = tb.slice_mut(s![tb.len() - k + 1..]);
        slice += &t.slice(s![t.len() - k + 1..]);
    }

    let mut idx_j = k - 1;
    for i in 0..z.len() {
        {
            let right_boundary = ((idx_j + m - 1) as isize + z[i]) as usize;
            let mut slice = tb.slice_mut(s![idx_j..right_boundary]);
            let temp = Array1::<f64>::from_vec(vec![u[i]; slice.len()]);
            slice.assign(&temp.view());
        }
        let incr = (z[i] + m as isize) as usize;
        idx_j += incr;
    }

    // Step 3: Find boundary values of Qt_i^j
    let k_64 = k as f64;
    let m_64 = m as f64;
    let temp = Array1::<f64>::range(k_64 - 1., 0., -1.);
    let arr = &temp / (m_64 + &temp);

    let mut alpha = cumprod1(&arr.to_vec()[..])?;
    alpha.push(1.);
    alpha.rotate_right(1);

    let alpha_arr = Array1::<f64>::from_vec(alpha);

    let mut beta = cumsum1(&z[1..z.len() - 1])?;
    beta.push(0);
    beta.rotate_right(1);

    let mut qt = Array3::<f64>::zeros((d, nb + 1, k));
    {
        let temp_a = pt_ij.slice(s![.., 0, 0..k]).to_owned();
        let temp_b = alpha_arr
            .slice(s![0..k])
            .to_shape(((1, 1, k), Order::C))?
            .to_owned();
        let update = temp_a * temp_b;

        // the broadcasting rules seem to be buggy or something as this
        // fix step should not be needed, yet it is
        let shape = update.shape();
        let mut slice = qt
            .slice_mut(s![.., 0, 0..k])
            .to_shape((shape, Order::C))?
            .to_owned();
        slice.assign(&update.view());
    }

    // Step 4: Find remaining values of Qt_i^j
    for i in 0..s {
        let left = ((k as isize) - z[i]) as usize;
        let right = z[i] as usize;
        {
            let temp_a = pt_ij.slice(s![.., beta[i], left..k]).to_owned();
            let temp_b = alpha_arr
                .slice(s![left..k])
                .to_shape(((1, 1, right), Order::C))?
                .to_owned();
            let update = temp_a * temp_b;

            // broadcasting fix again
            let shape = update.shape();
            let idx = (beta[i] + ((i * m) as isize)) as usize;
            let mut slice = qt
                .slice_mut(s![.., idx, left..k])
                .to_shape((shape, Order::C))?
                .to_owned();
            slice.assign(&temp.view());
        }
    }

    let out = qt.slice(s![.., .., 0]).into_dyn().to_owned();

    Ok(out)
}

pub fn cumsum1<T>(arr: &[T]) -> Result<Vec<T>, Error>
where
    T: AddAssign + Default + Clone + Copy + Zero,
{
    let cumsum = arr
        .iter()
        .scan(T::zero(), |acc, x| {
            *acc += *x;
            Some(*acc)
        })
        .collect::<Vec<T>>();

    Ok(cumsum.to_vec())
}

pub fn cumprod1<T>(arr: &[T]) -> Result<Vec<T>, Error>
where
    T: MulAssign + Default + Clone + Copy + One,
{
    let cumprod = arr
        .iter()
        .scan(T::one(), |acc, x| {
            *acc *= *x;
            Some(*acc)
        })
        .collect::<Vec<T>>();

    Ok(cumprod.to_vec())
}

/// Gauss-Legendre_Quadrature
///
/// Could not find a rust implementation of this, so have created one from
/// a C implementation found
/// [here](https://rosettacode.org/wiki/Numerical_integration/Gauss-Legendre_Quadrature#C).
///
/// The code is well short of optimal, but it gets things moving. Better
/// versions are provided by, for example,
/// [numpy.polynomial.legendre.leggauss](https://github.com/numpy/numpy/blob/v1.21.0/numpy/polynomial/legendre.py#L1519-L1584)
/// but the implementaitons are more involved so we have opted for quick and
/// dirty for the time being.
#[derive(Debug, Clone)]
pub struct GaussLegendreQuadrature {
    pub sample_points: Array1<f64>,
    pub weights: Array1<f64>,
}

impl GaussLegendreQuadrature {
    pub fn new(order: usize) -> Result<Self, Error> {
        if order < 1 {
            return Result::Err(Error::InvalidDegreeError);
        }

        let data = legendre_wrapper(&order);

        Ok(GaussLegendreQuadrature {
            sample_points: data.0,
            weights: data.1,
        })
    }
}

fn legendre_wrapper(order: &usize) -> (Array1<f64>, Array1<f64>) {
    let arr = legendre_coefficients(order);
    legendre_roots(&arr)
}

fn legendre_coefficients(order: &usize) -> Array2<f64> {
    let mut lcoef_arr = Array2::<f64>::zeros((*order + 1, *order + 1));
    lcoef_arr[[0, 0]] = 1.;
    lcoef_arr[[1, 1]] = 1.;

    for n in 2..*order + 1 {
        let n_64 = n as f64;
        lcoef_arr[[n, 0]] = -(n_64 - 1.) * lcoef_arr[[n - 2, 0]] / n_64;

        for i in 1..n + 1 {
            lcoef_arr[[n, i]] = ((2. * n_64 - 1.) * lcoef_arr[[n - 1, i - 1]]
                - (n_64 - 1.) * lcoef_arr[[n - 2, i]])
                / n_64;
        }
    }

    lcoef_arr
}

fn legendre_eval(coeff_arr: &Array2<f64>, n: &usize, x: &f64) -> f64 {
    let mut s = coeff_arr[[*n, *n]];
    for i in (1..*n + 1).rev() {
        s = s * (*x) + coeff_arr[[*n, i - 1]];
    }

    s
}

fn legendre_diff(coeff_arr: &Array2<f64>, n: &usize, x: &f64) -> f64 {
    let n_64 = *n as f64;
    n_64 * (x * legendre_eval(coeff_arr, n, x) - legendre_eval(coeff_arr, &(n - 1), x))
        / (x * x - 1.)
}

fn legendre_roots(coeff_arr: &Array2<f64>) -> (Array1<f64>, Array1<f64>) {
    let n = coeff_arr.shape()[0] - 1;
    let n_64 = n as f64;

    let mut sample_points_arr = Array1::<f64>::zeros(n + 1);
    let mut weights_arr = Array1::<f64>::zeros(n + 1);

    for i in 1..n + 1 {
        let i_64 = i as f64;
        let mut x = (PI * (i_64 - 0.25) / (n_64 + 0.5)).cos();
        let mut x1 = x;
        x -= legendre_eval(coeff_arr, &n, &x) / legendre_diff(coeff_arr, &n, &x);

        while fdim(&x, &x1) > 2e-16 {
            x1 = x;
            x -= legendre_eval(coeff_arr, &n, &x) / legendre_diff(coeff_arr, &n, &x);
        }

        sample_points_arr[i - 1] = x;
        x1 = legendre_diff(coeff_arr, &n, &x);
        weights_arr[i - 1] = 2. / ((1. - x * x) * x1 * x1);
    }

    // truncate the dummy value off the end + reverse sample points +
    // use symmetry to stable things up a bit.
    let mut samples = sample_points_arr.slice(s![..n; -1]).to_owned();
    samples = symmetric_samples(&samples);

    let mut weights = weights_arr.slice(s![..n]).to_owned();
    weights = symmetric_weights(&weights);

    (samples, weights)
}

fn symmetric_samples(arr: &Array1<f64>) -> Array1<f64> {
    let arr_rev = arr.slice(s![..; -1]).to_owned();
    (arr - &arr_rev) / 2.
}

fn symmetric_weights(arr: &Array1<f64>) -> Array1<f64> {
    let s = &arr.sum_axis(Axis(0));
    let arr_rev = arr.slice(s![..; -1]).to_owned();
    (arr + &arr_rev) / s
}

fn fdim(a: &f64, b: &f64) -> f64 {
    let res;
    if a - b > 0f64 {
        res = a - b;
    } else {
        res = 0f64;
    }
    res
}

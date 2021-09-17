use ndarray::Array1;
use std::cmp::min;

pub struct Basis {
    knots: Array1<f64>,
    order: usize,
    periodic: isize,
    n_all: usize,
    n: usize,
    start: f64,
    end: f64,
    ktol: f64,
}

impl Basis {
    pub fn new(
        order: usize,
        knots: Array1<f64>,
        periodic: Option<isize>,
        knot_tol: Option<f64>,
    ) -> Self {
        let p = periodic.unwrap_or_else(|| -1);
        let n_all = knots.len() - order;
        let n = knots.len() - order - ((p + 1) as usize);
        let start = knots[order - 1];
        let end = knots[n_all];

        Basis {
            order,
            knots,
            periodic: p,
            n_all,
            n,
            start,
            end,
            ktol: knot_tolerance(knot_tol),
        }
    }

    /// Snap evaluation points to knots if they are sufficiently close
    /// as specified by self.ktol
    ///
    /// :param t:        The parametric coordinate(s) in which to evaluate
    pub fn snap(&self, t: &Array1<f64>) -> Array1<f64> {
        let mut out = t.clone().to_owned();

        for j in 0..t.len() {
            let i = bisect_left(&self.knots, &t[j], t.len());
            if i < t.len() && (self.knots[i] - t[j]).abs() < self.ktol {
                out[j] = self.knots[i];
            } else if i > 0 && (self.knots[i - 1] - t[j]).abs() < self.ktol {
                out[j] = self.knots[i - 1];
            }
        }

        out
    }

    fn wrap_periodic(&self, t: &Array1<f64>, right: &bool) -> Array1<f64> {
        // Wrap periodic evaluation into domain
        let mut out = t.clone().to_owned();

        for i in 0..t.len() {
            if t[i] < self.start || t[i] > self.end {
                out[i] = (t[i] - self.start) % (self.end - self.start) + self.start;
            }
            if (t[i] - self.start).abs() < self.ktol && !right {
                out[i] = self.end;
            }
        }

        out
    }

    pub fn evaluate(
        &self,
        t: &Array1<f64>,
        d: usize,
        from_right: Option<bool>,
    ) -> (Array1<f64>, CSR) {
        let m = t.len();
        let mut right = from_right.unwrap_or(true);

        let mut out = t.clone().to_owned();
        if self.periodic >= 0 {
            out = self.wrap_periodic(&t, &right);
        }

        // init these -- there must be a better way to do this?
        let mut store = Array1::<f64>::zeros(self.order);
        let mut mu: usize = 0;
        let mut j: usize = 0;
        let mut k: usize = 0;
        let mut eval_t = out[0];

        let mut data = Array1::<f64>::zeros(m * self.order);
        let mut indices = Array1::<usize>::zeros(m * self.order);
        let indptr =
            Array1::<usize>::from_vec((0..m * self.order + 1).step_by(self.order).collect());

        for i in 0..m {
            right = from_right.unwrap_or(true).clone();
            eval_t = out[i];
            // Special-case the endpoint, so the user doesn't need to
            if (out[i] - self.end).abs() < self.ktol {
                right = false;
            }
            // Skip non-periodic evaluation points outside the domain
            if out[i] < self.start
                || out[i] > self.end
                || ((out[i] - self.start).abs() < self.ktol && !right)
            {
                continue;
            }

            // mu = index of last non-zero basis function
            if right {
                mu = bisect_right(&self.knots, &eval_t, self.n_all + self.order);
            } else {
                mu = bisect_left(&self.knots, &eval_t, self.n_all + self.order);
            }
            mu = min(mu, self.n_all);

            for k in 0..self.order - 1 {
                store[k] = 0.;
            }

            // the last entry is a dummy-zero which is never used
            store[self.order - 1] = 1.;

            for q in 1..self.order - d {
                j = self.order - q - 1;
                k = mu - q - 1;
                store[j] = store[j]
                    + store[j + 1] * (self.knots[k + q + 1] - eval_t)
                        / (self.knots[k + q + 1] - self.knots[k + 1]);

                for j in self.order - q..self.order - 1 {
                    // 'i'-index in global knot vector (ref Hughes book pg.21)
                    let k = mu - self.order + j;
                    store[j] =
                        store[j] * (eval_t - self.knots[k]) / (self.knots[k + q] - self.knots[k]);
                    store[j] = store[j]
                        + store[j + 1] * (self.knots[k + q + 1] - eval_t)
                            / (self.knots[k + q + 1] - self.knots[k + 1]);
                }
                j = self.order - 1;
                k = mu - 1;
                store[j] =
                    store[j] * (eval_t - self.knots[k]) / (self.knots[k + q] - self.knots[k]);
            }

            for q in self.order - d..self.order {
                for j in self.order - q - 1..self.order {
                    // 'i'-index in global knot vector (ref Hughes book pg.21)
                    k = mu - self.order + j;
                    if j != self.order - q - 1 {
                        store[j] = store[j] * (q as f64) / (self.knots[k + q] - self.knots[k]);
                    }
                    if j != self.order - 1 {
                        store[j] = store[j]
                            - store[j + 1] * (q as f64)
                                / (self.knots[k + q + 1] - self.knots[k + 1]);
                    }
                }
            }

            for (j, k) in (i * self.order..(i + 1) * self.order).enumerate() {
                data[k] = store[j];
                indices[k] = (mu - self.order + j) % self.n;
            }
        }
        let csr = CSR::new(data, indices, indptr, (m, self.n));

        (out, csr)
    }
}

fn knot_tolerance(tol: Option<f64>) -> f64 {
    tol.unwrap_or(1e-10)
}

fn bisect_left(arr: &Array1<f64>, val: &f64, mut hi: usize) -> usize {
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

fn bisect_right(arr: &Array1<f64>, val: &f64, mut hi: usize) -> usize {
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

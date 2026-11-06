#![allow(non_snake_case)]

//! Accelerate's CBLAS entry points Clip Studio Paint's plugin bundles import.
//!
//! No vendor math library backs this - matrix multiplication is plain nested
//! loops, correct for every `CBLAS_ORDER`/`CBLAS_TRANSPOSE` combination but
//! not tuned for speed.
//!
//! FIXME (ACCURACY): single-precision `f32` arithmetic here is Rust's IEEE
//! 754, which does not reproduce a real BLAS's fused multiply-add or SIMD
//! reduction order bit-for-bit. Results are correct to the last bit or two,
//! not necessarily identical to Apple's own.

use std::os::raw::c_int;

/// `enum CBLAS_ORDER`.
const CBLAS_COL_MAJOR: c_int = 102;

/// `enum CBLAS_TRANSPOSE`. `CblasConjTrans` is read the same as
/// `CblasTrans`: these routines are real, and a real matrix's conjugate
/// transpose is its transpose.
const CBLAS_NO_TRANS: c_int = 111;

/// The physical offset of element `(row, col)` of an array stored with
/// `lda` elements between consecutive rows (row-major) or columns
/// (col-major) - before any `CBLAS_TRANSPOSE` is applied.
fn offset(row_major: bool, lda: c_int, row: c_int, col: c_int) -> isize {
    (if row_major {
        row * lda + col
    } else {
        row + col * lda
    }) as isize
}

/// Element `(i, j)` of an operand that is logically `rows`-by-`cols` *after*
/// its own `CBLAS_TRANSPOSE`: `transposed` swaps to the physical `(j, i)`
/// the array actually stores, the way `op(A) = A^T` reads `A`.
///
/// # Safety
///
/// `data` holds [`offset`]'s result for every `i < rows`, `j < cols` this
/// call is used with.
unsafe fn read(
    data: *const f32,
    row_major: bool,
    lda: c_int,
    transposed: bool,
    i: c_int,
    j: c_int,
) -> f32 {
    let (row, col) = if transposed { (j, i) } else { (i, j) };
    // SAFETY: the caller's obligation.
    unsafe { *data.offset(offset(row_major, lda, row, col)) }
}

/// `void cblas_sgemm(enum CBLAS_ORDER Order, enum CBLAS_TRANSPOSE TransA,
/// enum CBLAS_TRANSPOSE TransB, int M, int N, int K, float alpha, const
/// float *A, int lda, const float *B, int ldb, float beta, float *C, int
/// ldc)`: `C := alpha * op(A) * op(B) + beta * C`, where `op(A)` is
/// `M`-by-`K` and `op(B)` is `K`-by-`N` - `lda`/`ldb` are the leading
/// dimension of the array as `A`/`B` are actually stored, which is `K`-by-`M`
/// and `N`-by-`K` respectively when transposed.
#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// For valid dimensions and strides, `a` and `b` must point to readable matrices
/// and `c` to a writable matrix of the requested layout and extent. The output
/// must not overlap either input, and all pointers must be aligned for `f32`.
pub unsafe extern "C" fn cblas_sgemm(
    order: c_int,
    trans_a: c_int,
    trans_b: c_int,
    m: c_int,
    n: c_int,
    k: c_int,
    alpha: f32,
    a: *const f32,
    lda: c_int,
    b: *const f32,
    ldb: c_int,
    beta: f32,
    c: *mut f32,
    ldc: c_int,
) {
    if m <= 0 || n <= 0 || k < 0 || a.is_null() || b.is_null() || c.is_null() {
        return;
    }
    let row_major = order != CBLAS_COL_MAJOR;
    let trans_a = trans_a != CBLAS_NO_TRANS;
    let trans_b = trans_b != CBLAS_NO_TRANS;
    for i in 0..m {
        for j in 0..n {
            let mut sum = 0.0f32;
            for l in 0..k {
                // SAFETY: `op(A)` is `m`-by-`k` and `op(B)` is `k`-by-`n`,
                // and the caller's `lda`/`ldb` are their strides.
                let (av, bv) = unsafe {
                    (
                        read(a, row_major, lda, trans_a, i, l),
                        read(b, row_major, ldb, trans_b, l, j),
                    )
                };
                sum += av * bv;
            }
            // SAFETY: `C` is `m`-by-`n` with the caller's stride `ldc`.
            unsafe {
                let at = c.offset(offset(row_major, ldc, i, j));
                *at = alpha * sum + beta * *at;
            }
        }
    }
}

/// `void cblas_sgemv(enum CBLAS_ORDER order, enum CBLAS_TRANSPOSE TransA,
/// int M, int N, float alpha, const float *A, int lda, const float *X, int
/// incX, float beta, float *Y, int incY)`: `y := alpha * op(A) * x + beta *
/// y`. Unlike `sgemm`'s `M`/`K`, `M` and `N` here name the stored matrix `A`
/// itself (`M`-by-`N`, regardless of `TransA`) rather than `op(A)` - that is
/// `N`-by-`M` when `TransA` transposes it.
#[allow(clippy::too_many_arguments)]
/// # Safety
///
/// For valid dimensions and strides, `a` and `x` must cover the readable matrix
/// and vector elements and `y` must cover the writable output elements,
/// including negative increments. All pointers must be aligned for `f32`,
/// and output elements must not overlap the inputs.
pub unsafe extern "C" fn cblas_sgemv(
    order: c_int,
    trans_a: c_int,
    m: c_int,
    n: c_int,
    alpha: f32,
    a: *const f32,
    lda: c_int,
    x: *const f32,
    inc_x: c_int,
    beta: f32,
    y: *mut f32,
    inc_y: c_int,
) {
    if m <= 0 || n <= 0 || a.is_null() || x.is_null() || y.is_null() {
        return;
    }
    let row_major = order != CBLAS_COL_MAJOR;
    let transposed = trans_a != CBLAS_NO_TRANS;
    // `op(A)` reads out `out_len` values from `in_len` inputs: `M` from `N`
    // untransposed, `N` from `M` transposed - `A` itself stays `M`-by-`N`.
    let (out_len, in_len) = if transposed { (n, m) } else { (m, n) };
    for out in 0..out_len {
        let mut sum = 0.0f32;
        for inp in 0..in_len {
            let (row, col) = if transposed { (inp, out) } else { (out, inp) };
            // SAFETY: `row < m` and `col < n`, so this stays inside `A`.
            let av = unsafe { *a.offset(offset(row_major, lda, row, col)) };
            // SAFETY: the caller's stride for an `in_len`-element vector.
            let xv = unsafe { *x.offset((inp * inc_x) as isize) };
            sum += av * xv;
        }
        // SAFETY: the caller's stride for an `out_len`-element vector.
        unsafe {
            let at = y.offset((out * inc_y) as isize);
            *at = alpha * sum + beta * *at;
        }
    }
}

exports::exports! {
    functions {
        "cblas_sgemm" => cblas_sgemm,
        "cblas_sgemv" => cblas_sgemv,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CBLAS_ROW_MAJOR: c_int = 101;
    const CBLAS_TRANS: c_int = 112;

    /// `A` (2x3) times `B` (3x2), the hand-computed answer every variant
    /// below is checked against.
    const EXPECTED: [f32; 4] = [58.0, 64.0, 139.0, 154.0];

    #[test]
    fn sgemm_row_major_no_transpose() {
        let a = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
        let b = [7.0f32, 8.0, 9.0, 10.0, 11.0, 12.0];
        let mut c = [0.0f32; 4];
        // SAFETY: `a`, `b` and `c` cover exactly the strides passed below.
        unsafe {
            cblas_sgemm(
                CBLAS_ROW_MAJOR,
                CBLAS_NO_TRANS,
                CBLAS_NO_TRANS,
                2,
                2,
                3,
                1.0,
                a.as_ptr(),
                3,
                b.as_ptr(),
                2,
                0.0,
                c.as_mut_ptr(),
                2,
            );
        }
        assert_eq!(c, EXPECTED);
    }

    #[test]
    fn sgemm_row_major_transposed_a_reads_the_same_logical_matrix() {
        // `A^T`, stored directly: row i holds A's column i.
        let a_transposed = [1.0f32, 4.0, 2.0, 5.0, 3.0, 6.0];
        let b = [7.0f32, 8.0, 9.0, 10.0, 11.0, 12.0];
        let mut c = [0.0f32; 4];
        // SAFETY: as above.
        unsafe {
            cblas_sgemm(
                CBLAS_ROW_MAJOR,
                CBLAS_TRANS,
                CBLAS_NO_TRANS,
                2,
                2,
                3,
                1.0,
                a_transposed.as_ptr(),
                2,
                b.as_ptr(),
                2,
                0.0,
                c.as_mut_ptr(),
                2,
            );
        }
        assert_eq!(c, EXPECTED);
    }

    #[test]
    fn sgemm_col_major_matches_the_row_major_answer() {
        const CBLAS_COL_MAJOR: c_int = 102;
        // The same logical A and B, laid out column by column.
        let a = [1.0f32, 4.0, 2.0, 5.0, 3.0, 6.0];
        let b = [7.0f32, 9.0, 11.0, 8.0, 10.0, 12.0];
        let mut c = [0.0f32; 4];
        // SAFETY: as above.
        unsafe {
            cblas_sgemm(
                CBLAS_COL_MAJOR,
                CBLAS_NO_TRANS,
                CBLAS_NO_TRANS,
                2,
                2,
                3,
                1.0,
                a.as_ptr(),
                2,
                b.as_ptr(),
                3,
                0.0,
                c.as_mut_ptr(),
                2,
            );
        }
        // Column-major C: [C00, C10, C01, C11].
        assert_eq!(c, [EXPECTED[0], EXPECTED[2], EXPECTED[1], EXPECTED[3]]);
    }

    #[test]
    fn sgemm_beta_scales_the_existing_output() {
        let a = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
        let b = [7.0f32, 8.0, 9.0, 10.0, 11.0, 12.0];
        let mut c = [1.0f32, 1.0, 1.0, 1.0];
        // SAFETY: as above.
        unsafe {
            cblas_sgemm(
                CBLAS_ROW_MAJOR,
                CBLAS_NO_TRANS,
                CBLAS_NO_TRANS,
                2,
                2,
                3,
                2.0,
                a.as_ptr(),
                3,
                b.as_ptr(),
                2,
                3.0,
                c.as_mut_ptr(),
                2,
            );
        }
        // `2 * EXPECTED + 3 * 1`.
        assert_eq!(c, [119.0, 131.0, 281.0, 311.0]);
    }

    #[test]
    fn sgemv_row_major_no_transpose() {
        let a = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
        let x = [1.0f32, 1.0, 1.0];
        let mut y = [0.0f32; 2];
        // SAFETY: `a` is 2x3 row-major, `x` has 3 elements, `y` has 2.
        unsafe {
            cblas_sgemv(
                CBLAS_ROW_MAJOR,
                CBLAS_NO_TRANS,
                2,
                3,
                1.0,
                a.as_ptr(),
                3,
                x.as_ptr(),
                1,
                0.0,
                y.as_mut_ptr(),
                1,
            );
        }
        assert_eq!(y, [6.0, 15.0]);
    }

    #[test]
    fn sgemv_transposed_reads_columns_of_the_same_stored_matrix() {
        let a = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
        let x = [1.0f32, 1.0];
        let mut y = [0.0f32; 3];
        // SAFETY: `a` is still the 2x3 array `M`/`N` describe; transposed
        // output has `N` = 3 elements, input `M` = 2.
        unsafe {
            cblas_sgemv(
                CBLAS_ROW_MAJOR,
                CBLAS_TRANS,
                2,
                3,
                1.0,
                a.as_ptr(),
                3,
                x.as_ptr(),
                1,
                0.0,
                y.as_mut_ptr(),
                1,
            );
        }
        assert_eq!(y, [5.0, 7.0, 9.0]);
    }

    #[test]
    fn sgemv_strided_vectors_skip_the_right_elements() {
        let a = [1.0f32, 1.0, 1.0];
        // Every other element is a value `incX` of 2 must skip.
        let x = [2.0f32, 999.0, 3.0, 999.0, 4.0, 999.0];
        let mut y = [0.0f32];
        // SAFETY: `a` is 1x3, `x` reads three elements two apart.
        unsafe {
            cblas_sgemv(
                CBLAS_ROW_MAJOR,
                CBLAS_NO_TRANS,
                1,
                3,
                1.0,
                a.as_ptr(),
                3,
                x.as_ptr(),
                2,
                0.0,
                y.as_mut_ptr(),
                1,
            );
        }
        assert_eq!(y, [9.0]);
    }
}

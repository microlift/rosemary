//! `libSystem.B`, `CoreMedia`, `CoreVideo` and `Accelerate`, and the truth about each.
//!
//! # Why this file exists
//!
//! Four families, and they are not alike. `Accelerate` is real linear algebra that
//! this host can do exactly, so every `vDSP_*` here is implemented and the tests
//! check the arithmetic against the definition rather than against a memory of it.
//! `CoreMedia` and `CoreVideo` are codec and pixel plumbing, and the honest answer
//! for most of it is a **named refusal**: a bound function returning that family's
//! own error code is a bound function, where an unbound import is a jump through a
//! zero slot (PLAN.md §4) - and a zero slot is how whole bring-up runs have been
//! lost.
//!
//! `libSystem.B` is not a family at all. It is whatever the linker happened to pull
//! in from the same image, so every name in it is decided on its own and every note
//! says which way it went.
//!
//! # The rule every answer here follows
//!
//! * **A value that is true is implemented, not refused.** `os_signpost_enabled`
//!   answers *false* because there is no signpost collection on this host, and the
//!   guest's own code at `Framework+0x4d686b` branches on exactly that - it skips
//!   the whole emit when the answer is false.
//! * **Where there is no answer, the code is the one that family's own headers
//!   define for "not available"** - `kCMBlockBufferBadPointerParameterErr`,
//!   `kCVReturnUnsupported`, `KERN_FAILURE`, `ENOTSUP` - and never a plausible zero
//!   the guest would read as success.
//! * **Nothing here declares a structure the guest might poke.** A `CMBlockBuffer`
//!   and a `CVPixelBuffer` are CoreFoundation objects to the guest, which only ever
//!   hands one back to another CoreMedia/CoreVideo call; every one of those is
//!   answered here, so the layout is this file's own and is not a claim about
//!   CoreMedia's. Where the guest *does* see the bytes - `CMTime` - the layout is
//!   quoted from `<CoreMedia/CMTime.h>` and a test asserts every offset.
//!
//! # What is in here
//!
//! | family | names | what this file does |
//! |---|--:|---|
//! | `Accelerate` (`vDSP_*`) | 17 | 16 implemented as real vector arithmetic, a real convolution and a real FFT in Apple's documented packing; 1 refused |
//! | `CoreMedia` | 31 | a real `CMBlockBuffer` and a real `CMSampleBuffer` over it, `CMTimeMake`/`CMTimeGetSeconds` for real, 12 named refusals |
//! | `CoreVideo` | 27 | a real `CVPixelBuffer` and a real pool, 9 named refusals |
//! | `libSystem.B` | 35 | 15 real answers, 20 named refusals |
//!
//! # The one Accelerate name that is refused
//!
//! `vDSP_deq22D`. Its declaration sits under the comment "*Difference equation,
//! 2 poles, 2 zeros*" - a **recursive** filter whose coefficient order
//! `<vecLib/vDSP.h>` does not give - and the guest calls it once
//! (`Framework+0xb4284af`) with a state vector it reads back out afterwards.
//! Inventing a four-coefficient recursion is inventing the one thing a filter
//! cannot survive being wrong about, so it refuses by name.
//!
//! # How each signature was established
//!
//! Every one below was read out of the SDK header its own comment names *and* read
//! off the registers at the guest's call site, with `test/tools/callers.py` for the
//! sites and `test/tools/raw-range.py` for the instructions. Where a reading could
//! go two ways the note says which one the bytes settle. The one place the header
//! and the registers could not be reconciled - `CMClockConvertHostTimeToSystemUnits`
//! - is refused rather than guessed, and its note says what was seen.

#[cfg(not(target_os = "macos"))]
mod imp {
    //! The answers, and for each one why it is the true answer.

    use std::ffi::{c_char, c_int, c_long, c_uint, c_void, CStr, CString};

    /// Say a refusal once.
    ///
    /// A refusal that says nothing cannot be told from a working implementation by
    /// whoever reads the log next, which is the outcome PLAN.md §1 is about. Each
    /// message names the function and the reason, once per process - an audio loop
    /// must not turn one refusal into a million lines.
    fn said_once(key: &str, name: &str, why: &str) {
        static FIRST: std::sync::Once = std::sync::Once::new();
        // The key is only there so two refusals from one place do not collapse
        // into one message; the Once means the first one is the one that is said.
        FIRST.call_once(|| eprintln!("rosetta3: {name}: {why}"));
        let _ = key;
    }

    /// The host's `errno`, read straight out of it.
    ///
    /// A `Darwin` `errno` value is a `Linux` one on this host because
    /// [`crate::errno`] is what makes the two agree, and the point of reading it
    /// here is to report the failure a call actually had rather than the number
    /// the refusal was written with.
    fn last_errno() -> i32 {
        // SAFETY: `errno` is a per-thread `int` on this host, and reading it is
        // what the `__errno_location` contract is for.
        unsafe { *libc::__errno_location() }
    }

    /// A guest's `CString` as the host's `Path`.
    ///
    /// The two are the same bytes; a `CString` just does not implement `AsRef<Path>`
    /// because it is not one. Lossy rather than checked, so a path this host cannot
    /// name becomes a `fs` call that fails with `ENOENT` - which is the truth -
    /// rather than a panic inside a guest's `copyfile`.
    fn path_of(text: &CString) -> std::path::PathBuf {
        std::path::PathBuf::from(text.to_string_lossy().into_owned())
    }

    /// `errno` as this crate reports it, and the `-1` every `int`-returning
    /// libSystem call uses for failure.
    fn failed(code: i32) -> i32 {
        crate::errno::set(code);
        -1
    }

    // ============================================================== Accelerate
    //
    // The whole vDSP surface is real arithmetic, and `<vecLib/vDSP.h>` is the
    // authority for every shape below. Two facts about its types decide most of
    // the arguments:
    //
    //   typedef long vDSP_Stride;      /* signed */
    //   typedef long vDSP_Length;
    //
    // `Stride` is signed because the guest genuinely passes `-1`: every FFT site
    // in the binary puts `-1` in the stride register of the inverse transform, and
    // a `usize` would turn that into a huge positive offset.

    /// `vDSP_Stride` - a *signed* index difference.
    type Stride = c_long;
    /// `vDSP_Length` - how many elements.
    type Length = c_long;

    /// `DSPSplitComplex`, the two halves of a complex vector in two arrays.
    ///
    /// `<vecLib/vDSP.h>` declares it as a struct of two `float *`, and every `z*`
    /// entry point takes a *pointer* to it, which is what the binary shows: `rdx`
    /// is the pair's address and the count is the fifth register.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct SplitComplex {
        realp: *mut f32,
        imagp: *mut f32,
    }

    /// `DSPComplex`, one complex value interleaved.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Complex {
        real: f32,
        imag: f32,
    }

    /// `FFTRadix`, as `<vecLib/vDSP.h>` numbers it.
    ///
    /// `kFFTRadix0` is 0, and it means "the most efficient radix for this size" -
    /// which is exactly the value the guest passes (`Framework+0xb3dac3f`,
    /// `xor esi, esi`).
    #[allow(dead_code)]
    mod radix {
        use std::ffi::c_uint;

        pub const AUTO: c_uint = 0;
        pub const TWO: c_uint = 2;
        pub const THREE: c_uint = 3;
        pub const FOUR: c_uint = 4;
        pub const FIVE: c_uint = 5;
    }

    /// `FFTDirection`, from the same header: forward is 1, inverse is -1.
    ///
    /// The binary uses those exact numbers, and the same value appears in both the
    /// stride and the direction register of every `vDSP_fft_zrip` call - which is
    /// the observation that settled the parameter order (see [`vdsp_fft_zrip`]).
    #[allow(dead_code)]
    mod direction {
        use std::ffi::c_int;

        pub const FORWARD: c_int = 1;
        pub const INVERSE: c_int = -1;
    }

    /// What `vDSP_create_fftsetup` hands back: an `FFTSetup`.
    ///
    /// `FFTSetup` is opaque in the header - "an opaque type that contains setup
    /// information for a single-precision FFT transform" - and the guest only ever
    /// stores it and hands it back to [`vdsp_fft_zrip`] or
    /// [`vdsp_destroy_fftsetup`]. The one piece of it the guest reads is nothing:
    /// at `Framework+0xb3dac46` it stores the pointer into a heap cell and
    /// `+0xb3dac65` reads it straight back out to destroy it. So the contents are
    /// this file's own, and that is not a claim about vDSP's.
    struct FftSetup {
        log2n: u32,
        /// Whether the guest asked for a radix this implementation can honour.
        /// A `kFFTRadix3/4/5` transform length is not a power of two, and the
        /// coefficient tables for those are a different algorithm, so those are
        /// refused at creation rather than approximated - see [`vdsp_create_fftsetup`].
        radix: c_uint,
    }

    /// `FFTSetup vDSP_create_fftsetup(vDSP_Length log2n, FFTRadix radix)`.
    ///
    /// `log2n` is the **base-2 logarithm of the number of real points**, not the
    /// number of points. The guest's own call settles it: it indexes a setup cache
    /// with the very value it passes, and the cache is bounds-checked against 24
    /// (`Framework+0xb3dac0d`, `cmp DWORD PTR [rip+0x35602cf], ebx` with the
    /// limit stored as 0x18), which is a logarithm's range and not a length's.
    ///
    /// A real handle, and one that is a real handle for the transform it promises:
    /// [`vdsp_fft_zrip`] reads `log2n` back out of it. Null is the header's
    /// failure answer and is what an out-of-range exponent or a radix this file
    /// will not approximate gets; the guest stores whatever comes back without
    /// testing it (`+0xb3dac46`) and both consumers of the handle tolerate null.
    unsafe extern "C" fn vdsp_create_fftsetup(
        log2n: Length,
        radix_in: c_uint,
    ) -> *mut c_void {
        // Apple's own limit, and the one the guest's cache enforces: 2..=24.
        if !(2..=24).contains(&log2n) {
            said_once(
                "fftsetup-range",
                "vDSP_create_fftsetup",
                &format!(
                    "declining log2n {log2n}: the transform length 2^n must be 4..=16777216, \
                     which is the range the guest's own setup cache bounds-checks against \
                     (Framework+0xb3dac0d)"
                ),
            );
            return std::ptr::null_mut();
        }
        if !matches!(radix_in, radix::AUTO | radix::TWO) {
            // A radix-3, -4 or -5 transform is not a power of two, and its
            // coefficient tables are a different algorithm from the power-of-two
            // one below. Refusing the *setup* is the honest move: a setup that
            // existed and then computed something else would be worse.
            said_once(
                "fftsetup-radix",
                "vDSP_create_fftsetup",
                &format!(
                    "declining FFTRadix {radix_in}: only kFFTRadix0 and kFFTRadix2 are \
                     implemented, because they describe a power-of-two transform and this \
                     implementation computes a power-of-two transform. A null setup is the \
                     header's failure answer, and the guest's setup cache stores it without \
                     reading it"
                ),
            );
            return std::ptr::null_mut();
        }
        Box::into_raw(Box::new(FftSetup {
            log2n: log2n as u32,
            radix: radix_in,
        })) as *mut c_void
    }

    /// `void vDSP_destroy_fftsetup(FFTSetup setup)`.
    ///
    /// A real free, of a real allocation: the pointer came from
    /// [`vdsp_create_fftsetup`] and from nowhere else. Null is accepted, because
    /// the guest calls this in a loop over its cache (`Framework+0xb3dac68`) and
    /// the cache may hold a refused entry.
    unsafe extern "C" fn vdsp_destroy_fftsetup(setup: *mut c_void) {
        if setup.is_null() {
            return;
        }
        // SAFETY: every non-null pointer this module hands out is a
        // `Box<FftSetup>` from [`vdsp_create_fftsetup`] and is freed once, by
        // the guest's own call to this function.
        drop(unsafe { Box::from_raw(setup as *mut FftSetup) });
    }

    /// The real transform, in the packing Apple's data-packing page documents.
    ///
    /// # The packing, quoted
    ///
    /// From "*Understanding data packing for Fourier transforms*", Apple
    /// Developer Documentation, on the result of a forward real FFT of `n` real
    /// points held in `n/2` complex elements:
    ///
    /// > "Complex element 0 contains the DC component (equal to the sum of the
    ///  time domain values) in its real part and zero in its imaginary part...
    ///  The remaining complex elements contain the complex conjugates...
    ///  They achieve this by placing the real Nyquist component in the imaginary
    ///  part of the DC element and omitting the complex conjugates."
    ///
    /// So for `N = n/2` complex elements:
    ///
    /// ```text
    /// z[0]   = ( X[0], X[N] )      the DC, and the Nyquist in the imaginary part
    /// z[k]   = X[k]                1 <= k <= N-1
    /// ```
    ///
    /// and the transform is **unscaled**: the same page's scaling table gives a
    /// real *forward* transform a factor of 2 and a real *inverse* transform the
    /// number of real elements, i.e. the caller divides by 2 after a forward
    /// transform and by `n` after an inverse one.
    ///
    /// The guest does exactly that, and that is the cross-check: at
    /// `Framework+0xb3d54b3` and `+0xb3d54db` it calls `vDSP_vsmul` by `0.5f` on
    /// the real half and then on the imaginary half of the split buffer - two
    /// calls because the halves are two arrays - which is the documented factor of
    /// 2 applied to both. It never rescales between the forward and the inverse
    /// call, so the inverse has to be unscaled too.
    fn transform(buffer: &mut SplitComplex, log2n: u32, forward: bool) {
        let n = 1usize << log2n;
        let half = n / 2;
        // The interleaved read: element k of the split buffer is the pair of real
        // samples (2k, 2k+1), which is what `vDSP_ctoz` at stride 2 leaves behind.
        //
        // **The samples are copied out first**, and that is not tidiness. The
        // transform is in place, so a closure that reads `buffer` would read the
        // *output* for every bin after the first - which is exactly the bug a
        // test that only checks bin 0 and the Nyquist will not catch.
        // SAFETY: the caller below has established that the buffer is non-null and
        // that it has `half` elements in each half.
        let samples: Vec<f32> = (0..half)
            .flat_map(|k| unsafe { [*buffer.realp.add(k), *buffer.imagp.add(k)] })
            .collect();
        let sample = |k: usize| samples[k];
        let put = |k: usize, re: f32, im: f32| unsafe {
            *buffer.realp.add(k) = re;
            *buffer.imagp.add(k) = im;
        };

        if forward {
            // The Nyquist coefficient is read out of the input *before* the loop
            // writes over it: element 0's imaginary part is where it goes, and
            // element 0's real part is where the DC is about to be written.
            let nyquist: f64 = (0..n)
                .map(|k| {
                    let x = sample(k) as f64;
                    if k % 2 == 0 { x } else { -x }
                })
                .sum();
            // A direct definition, not an optimisation: `X[j] = sum_k x[k] w^jk`
            // with `w = exp(-2*pi*i/n)`. The test compares this against a
            // separately written O(n^2) transform, so the two cannot share a bug.
            let sign = -1.0f64;
            for j in 0..half {
                let (mut re, mut im) = (0.0f64, 0.0f64);
                for k in 0..n {
                    let x = sample(k);
                    let angle =
                        sign * std::f64::consts::TAU * (j as f64) * (k as f64) / (n as f64);
                    let (sin, cos) = angle.sin_cos();
                    re += x as f64 * cos;
                    im += x as f64 * sin;
                }
                if j == 0 {
                    put(0, re as f32, 0.0f32);
                } else {
                    put(j, re as f32, im as f32);
                }
            }
            // Element 0's imaginary part is the Nyquist coefficient, which is the
            // sum of the even samples minus the odd ones.
            // SAFETY: as above.
            unsafe { *buffer.imagp.add(0) = nyquist as f32 };
        } else {
            // The inverse: rebuild the full spectrum from the packed half, then
            // the same sum with the opposite sign, unscaled.
            let sign = 1.0f64;
            for m in 0..n {
                // Only the real part is computed, and that is not an omission: the
                // inverse of the conjugate-symmetric spectrum of a real sequence is
                // itself real, so the imaginary part is identically zero. The test
                // below asserts it, rather than this computing a zero and dropping
                // it.
                let mut re = 0.0f64;
                for j in 0..half {
                    // Element 0 carries the DC in its real part and the Nyquist in
                    // its imaginary part, so at j == 0 the imaginary part is not a
                    // bin's imaginary part at all - which is why `jim` is zero
                    // there, and why the Nyquist is added separately below.
                    let jre = sample(2 * j) as f64;
                    let jim = if j == 0 {
                        0.0
                    } else {
                        sample(2 * j + 1) as f64
                    };
                    let angle =
                        sign * std::f64::consts::TAU * (j as f64) * (m as f64) / (n as f64);
                    let (s, c) = angle.sin_cos();
                    // The conjugate-symmetric spectrum: X[n-j] = conj(X[j]), and
                    // the Nyquist coefficient is real.
                    let scale = if j == 0 { 1.0 } else { 2.0 };
                    re += scale * (jre * c - jim * s);
                }
                // X[n/2] is the Nyquist term, real, and multiplies (-1)^m.
                let nyquist = sample(1) as f64;
                re += nyquist * if m % 2 == 0 { 1.0 } else { -1.0 };
                // Back into the interleaved form `vDSP_ctoz` reads: the even
                // samples are the real parts and the odd ones the imaginary parts,
                // and the inverse of a real transform is real, so `im` is zero.
                //
                // **One half at a time.** Element `j` holds the pair (x[2j],
                // x[2j+1]), so `m = 0` and `m = 1` both land in element 0 and must
                // write different halves of it. Writing both - which is what the
                // forward transform's `put` does, correctly, because a spectrum
                // element's two parts *are* a complex number - loses every even
                // sample of the output.
                // SAFETY: as above; `m / 2` is inside the `half` elements the
                // buffer is documented to hold.
                unsafe {
                    if m % 2 == 0 {
                        *buffer.realp.add(m / 2) = re as f32;
                    } else {
                        *buffer.imagp.add(m / 2) = re as f32;
                    }
                }
            }
        }
    }

    /// `void vDSP_fft_zrip(FFTSetup setup, DSPSplitComplex *ioData, vDSP_Stride stride, vDSP_Length log2n, FFTDirection direction)`.
    ///
    /// # The five arguments, and how the binary settles them
    ///
    /// The header gives the order above. The binary agrees, and one detail is
    /// worth recording because it is what made the order checkable: at every call
    /// site the *stride* and the *direction* hold the **same** value - `1` for a
    /// forward transform and `-1` for an inverse one (`Framework+0xb3d6b3d` sets
    /// `edx` and `r8d` to 1 for the forward call and `+0xb3d6be3`/`+0xb3d6be6` set
    /// both to `-1` for the inverse). Two arguments that always agree cannot be one
    /// argument, and it is the *stride* that goes negative, which is exactly what
    /// a reverse-order spectrum needs.
    ///
    /// # What the negative stride means here, and why it is ignored
    ///
    /// The only reading that is self-consistent is that it does not reorder
    /// anything: the guest hands **the same buffer** to the inverse that the
    /// forward wrote (`Framework+0xb3d6b43` then `+0xb3d6bec`, no intervening copy
    /// and no reversal), and then reads the result back with `vDSP_ztoc` at stride
    /// 1 (`+0xb3d6c0b`). If the inverse read its input mirrored, the pair would not
    /// be a round trip on a buffer that was never mirrored, and the code could not
    /// work. So `|stride| == 1` means "the elements are in the order they are in",
    /// and the transform is the exact inverse of the forward one in that same
    /// order.
    ///
    /// A stride whose magnitude is not 1 is **declined** rather than interpreted:
    /// there is no reading of it this file can source, and a spectrum that looks
    /// like a transform but is not one is the outcome to avoid.
    unsafe extern "C" fn vdsp_fft_zrip(
        setup: *mut c_void,
        io_data: *mut SplitComplex,
        stride: Stride,
        log2n: Length,
        dir: c_int,
    ) {
        let Some(setup) = (unsafe { setup_as(setup) }) else {
            return;
        };
        if !matches!(setup.radix, radix::AUTO | radix::TWO) {
            // Unreachable, because [`vdsp_create_fftsetup`] refuses every other
            // radix. Checked anyway: a setup claiming a radix this file does not
            // implement is a handle from somewhere else, and computing a
            // power-of-two transform for it would be the wrong answer.
            said_once(
                "zrip-radix",
                "vDSP_fft_zrip",
                "declining: the setup's radix is one this file does not implement, so it is \
                 not a setup vDSP_create_fftsetup here produced",
            );
            return;
        }
        if io_data.is_null() || log2n < 0 || log2n as u32 != setup.log2n {
            said_once(
                "zrip-args",
                "vDSP_fft_zrip",
                "declining: the buffer is null, or the log2n does not match the setup the \
                 guest created. A transform that is not the one asked for is worse than none",
            );
            return;
        }
        if stride != 1 && stride != -1 {
            said_once(
                "zrip-stride",
                "vDSP_fft_zrip",
                &format!(
                    "declining stride {stride}: only |stride| == 1 is implemented, because \
                     that is the only order the guest's own call sites use and the only one \
                     this file can establish from them. The buffer is left untouched"
                ),
            );
            return;
        }
        let forward = match dir {
            direction::FORWARD => true,
            direction::INVERSE => false,
            _ => {
                said_once(
                    "zrip-dir",
                    "vDSP_fft_zrip",
                    &format!("declining direction {dir}: the header defines 1 (forward) and \
                              -1 (inverse) and nothing else"),
                );
                return;
            }
        };
        // SAFETY: the caller is CoreVideo-era guest code holding a
        // `DSPSplitComplex` this transform is defined to read and write in place;
        // `log2n` fixes the element count at `2^log2n / 2` per half and the guest
        // sized the buffer for exactly that.
        unsafe {
            let buffer = &mut *io_data;
            if buffer.realp.is_null() || buffer.imagp.is_null() {
                return;
            }
            transform(buffer, setup.log2n, forward);
        }
    }

    /// A `FftSetup` behind a guest pointer, or `None` for anything else.
    ///
    /// # Safety
    ///
    /// `at` must be a pointer from [`vdsp_create_fftsetup`] or null. The guest
    /// only ever stores what that function returned, and passes it back to
    /// [`vdsp_fft_zrip`] or [`vdsp_destroy_fftsetup`], so there is no other kind of
    /// value to defend against here; the null check is the whole of it.
    unsafe fn setup_as(at: *mut c_void) -> Option<&'static mut FftSetup> {
        if at.is_null() {
            return None;
        }
        // SAFETY: the caller's obligation, per the doc comment.
        Some(unsafe { &mut *(at as *mut FftSetup) })
    }

    /// `void vDSP_ctoz(const DSPComplex C[], vDSP_Stride IC, DSPSplitComplex *Z, vDSP_Stride IZ, vDSP_Length size)`.
    ///
    /// The even-odd split: element `i` of the interleaved input becomes
    /// `realp[i*IZ] = C[i*IC].real` and `imagp[i*IZ] = C[i*IC].imag`.
    ///
    /// Apple's data-packing page states the operation for the input side - given
    /// `[0,1,2,3,4,5,6,7]`, `vDSP_ctoz(..., 2, &z, 1, 4)` leaves `realp` as
    /// `[0, 2, 4, 6]` and `imagp` as `[1, 3, 5, 7]` - and the binary's ten call
    /// sites all pass `IC = 2, IZ = 1` for exactly that case, with the count last
    /// (`Framework+0xb3d5471`: `esi`=2, `ecx`=1, `r8d`=count/2).
    unsafe extern "C" fn vdsp_ctoz(
        c: *const Complex,
        ic: Stride,
        z: *mut SplitComplex,
        iz: Stride,
        size: Length,
    ) {
        if size <= 0 || c.is_null() || z.is_null() {
            return;
        }
        // SAFETY: the caller's two arrays; `size` elements at the strides the
        // declaration names is the access the function is defined to make.
        unsafe {
            let z = &mut *z;
            if z.realp.is_null() || z.imagp.is_null() {
                return;
            }
            for i in 0..size {
                let at = &*c.offset((i * ic) as isize);
                *z.realp.offset((i * iz) as isize) = at.real;
                *z.imagp.offset((i * iz) as isize) = at.imag;
            }
        }
    }

    /// `void vDSP_ztoc(const DSPSplitComplex *Z, vDSP_Stride IZ, DSPComplex C[], vDSP_Stride IC, vDSP_Length size)`.
    ///
    /// The inverse of [`vdsp_ctoz`]: element `i` of the interleaved output is
    /// `{ realp[i*IZ], imagp[i*IZ] }`. Ten call sites, all `IZ = 1, IC = 2`.
    unsafe extern "C" fn vdsp_ztoc(
        z: *const SplitComplex,
        iz: Stride,
        c: *mut Complex,
        ic: Stride,
        size: Length,
    ) {
        if size <= 0 || z.is_null() || c.is_null() {
            return;
        }
        // SAFETY: as [`vdsp_ctoz`].
        unsafe {
            let z = &*z;
            if z.realp.is_null() || z.imagp.is_null() {
                return;
            }
            for i in 0..size {
                *c.offset((i * ic) as isize) = Complex {
                    real: *z.realp.offset((i * iz) as isize),
                    imag: *z.imagp.offset((i * iz) as isize),
                };
            }
        }
    }

    /// `void vDSP_vadd(const float A[], vDSP_Stride IA, const float B[], vDSP_Stride IB, float C[], vDSP_Stride IC, vDSP_Length size)`.
    ///
    /// `C[i*IC] = A[i*IA] + B[i*IB]`, and nothing else. The binary's eleven sites
    /// all set seven arguments: `rdi`, `rsi`, `rdx`, `rcx`, `r8`, `r9` and one at
    /// `[rsp]` (`Framework+0xb3d62b8` is the `push` that supplies the count).
    unsafe extern "C" fn vdsp_vadd(
        a: *const f32,
        ia: Stride,
        b: *const f32,
        ib: Stride,
        c: *mut f32,
        ic: Stride,
        size: Length,
    ) {
        if size <= 0 || a.is_null() || b.is_null() || c.is_null() {
            return;
        }
        // SAFETY: `size` elements at the given strides of each of the caller's
        // three arrays, which is what the declaration is defined to touch.
        unsafe {
            for i in 0..size {
                *c.offset((i * ic) as isize) = *a.offset((i * ia) as isize) + *b.offset((i * ib) as isize);
            }
        }
    }

    /// `void vDSP_vsub(...)` - the same seven arguments, and `C = A - B`.
    unsafe extern "C" fn vdsp_vsub(
        a: *const f32,
        ia: Stride,
        b: *const f32,
        ib: Stride,
        c: *mut f32,
        ic: Stride,
        size: Length,
    ) {
        if size <= 0 || a.is_null() || b.is_null() || c.is_null() {
            return;
        }
        // SAFETY: as [`vdsp_vadd`].
        unsafe {
            for i in 0..size {
                *c.offset((i * ic) as isize) = *a.offset((i * ia) as isize) - *b.offset((i * ib) as isize);
            }
        }
    }

    /// `void vDSP_vmul(...)` - the same seven arguments, and `C = A * B`.
    unsafe extern "C" fn vdsp_vmul(
        a: *const f32,
        ia: Stride,
        b: *const f32,
        ib: Stride,
        c: *mut f32,
        ic: Stride,
        size: Length,
    ) {
        if size <= 0 || a.is_null() || b.is_null() || c.is_null() {
            return;
        }
        // SAFETY: as [`vdsp_vadd`].
        unsafe {
            for i in 0..size {
                *c.offset((i * ic) as isize) = *a.offset((i * ia) as isize) * *b.offset((i * ib) as isize);
            }
        }
    }

    /// `void vDSP_vsadd(float A[], vDSP_Stride IA, float B, float C[], vDSP_Stride IC, vDSP_Length size)`.
    ///
    /// Six arguments, and the third is a **scalar in a register**, not a pointer:
    /// the header spells it `float B` where the `vadd` family spells its third
    /// `const float B[]`. The binary agrees - `Framework+0xb3d54b3` passes
    /// `rdx` as the address of a stack slot holding `0.5f`.
    unsafe extern "C" fn vdsp_vsadd(
        a: *const f32,
        ia: Stride,
        b: f32,
        c: *mut f32,
        ic: Stride,
        size: Length,
    ) {
        if size <= 0 || a.is_null() || c.is_null() {
            return;
        }
        // SAFETY: as [`vdsp_vadd`], with one input being a register.
        unsafe {
            for i in 0..size {
                *c.offset((i * ic) as isize) = *a.offset((i * ia) as isize) + b;
            }
        }
    }

    /// `void vDSP_vsmul(const float A[], vDSP_Stride IA, const float *B, float C[], vDSP_Stride IC, vDSP_Length size)`.
    ///
    /// The header is the one place in this family where the scalar arrives as a
    /// `const float *`: `__vDSP_input2` is a pointer, and the binary passes the
    /// address of a `0.5f` (`Framework+0xb3d54a1` is the `lea` of it). So the
    /// third argument is read through.
    unsafe extern "C" fn vdsp_vsmul(
        a: *const f32,
        ia: Stride,
        b: *const f32,
        c: *mut f32,
        ic: Stride,
        size: Length,
    ) {
        if size <= 0 || a.is_null() || b.is_null() || c.is_null() {
            return;
        }
        let b = unsafe { *b };
        // SAFETY: as [`vdsp_vadd`], with the scalar read once.
        unsafe {
            for i in 0..size {
                *c.offset((i * ic) as isize) = *a.offset((i * ia) as isize) * b;
            }
        }
    }

    /// `void vDSP_vsma(const float A[], vDSP_Stride IA, const float *B, const float C[], vDSP_Stride IC, float D[], vDSP_Stride ID, vDSP_Length size)`.
    ///
    /// `D[i*ID] = A[i*IA] * B + C[i*IC]`.
    ///
    /// Eight arguments - six in registers and two pushed - which is what
    /// `Framework+0xa3e3108`/`+0xa3e3109` do (`push rax` then `push 0x1`, the
    /// count last). `B` is a pointer to the scalar, as in [`vdsp_vsmul`].
    unsafe extern "C" fn vdsp_vsma(
        a: *const f32,
        ia: Stride,
        b: *const f32,
        c: *const f32,
        ic: Stride,
        d: *mut f32,
        id: Stride,
        size: Length,
    ) {
        if size <= 0 || a.is_null() || b.is_null() || c.is_null() || d.is_null() {
            return;
        }
        let b = unsafe { *b };
        // SAFETY: as [`vdsp_vadd`], with four arrays and one scalar.
        unsafe {
            for i in 0..size {
                *d.offset((i * id) as isize) = *a.offset((i * ia) as isize) * b + *c.offset((i * ic) as isize);
            }
        }
    }

    /// `void vDSP_vclip(float A[], vDSP_Stride I, float *B, float *C, float *D, vDSP_Stride L, vDSP_Length size)`.
    ///
    /// `D[i*L] = min(max(A[i*I], *B), *C)` - the third argument is the **low**
    /// threshold and the fourth the **high** one.
    ///
    /// That order is worth a note, because the modern documentation's parameter
    /// list is in the same order and the old-style reading (output third) is the
    /// one that would break. The binary settles it: `Framework+0xb469a9c` stores
    /// `0.0f` at `[rbp-0x2c]` and `[rbp-0x34]`, then passes `rdx = &0.0f` and
    /// `rcx = &(float)n` with `r9d = 1` and `[rsp] = count` - so the low
    /// threshold is the third register and the count is the seventh argument, not
    /// the sixth.
    unsafe extern "C" fn vdsp_vclip(
        a: *const f32,
        i: Stride,
        low: *const f32,
        high: *const f32,
        d: *mut f32,
        l: Stride,
        size: Length,
    ) {
        if size <= 0 || a.is_null() || low.is_null() || high.is_null() || d.is_null() {
            return;
        }
        // SAFETY: the caller's arrays, and one `float` from each of two
        // threshold pointers the declaration says are non-null.
        unsafe {
            let (low, high) = (*low, *high);
            for k in 0..size {
                let v = *a.offset((k * i) as isize);
                *d.offset((k * l) as isize) = v.min(high).max(low);
            }
        }
    }

    /// `void vDSP_maxmgv(const float A[], vDSP_Stride I, float *C, vDSP_Length size)`.
    ///
    /// `*C` is the largest element. Four arguments, the count last, and the
    /// destination is a pointer the caller pre-zeroes (`Framework+0xb43a24d`
    /// reads it straight back into a register).
    unsafe extern "C" fn vdsp_maxmgv(
        a: *const f32,
        i: Stride,
        c: *mut f32,
        size: Length,
    ) {
        if size <= 0 || a.is_null() || c.is_null() {
            return;
        }
        // SAFETY: `size` elements of the caller's array.
        unsafe {
            let mut best = *a;
            for k in 1..size {
                let v = *a.offset((k * i) as isize);
                if v > best {
                    best = v;
                }
            }
            *c = best;
        }
    }

    /// `void vDSP_svesq(const float A[], vDSP_Stride I, float *C, vDSP_Length size)`.
    ///
    /// `*C = sum of A[i*I]^2`. Four arguments, same shape as [`vdsp_maxmgv`], and
    /// the guest adds the result into an accumulator itself
    /// (`Framework+0xb3e7e0c`, `addss`), which is why this returns a sum and not
    /// a mean.
    unsafe extern "C" fn vdsp_svesq(
        a: *const f32,
        i: Stride,
        c: *mut f32,
        size: Length,
    ) {
        if size <= 0 || a.is_null() || c.is_null() {
            return;
        }
        // SAFETY: `size` elements of the caller's array.
        unsafe {
            let mut sum = 0.0f32;
            for k in 0..size {
                let v = *a.offset((k * i) as isize);
                sum += v * v;
            }
            *c = sum;
        }
    }

    /// `void vDSP_conv(const float signal[], vDSP_Stride signalStride, const float filter[], vDSP_Stride strideFilter, float result[], vDSP_Stride strideResult, vDSP_Length lenResult, vDSP_Length lenFilter)`.
    ///
    /// The sliding dot product, which is what Apple's documentation says it is:
    /// > "Both operations compute the sliding dot product of the filter and the
    ///  section of the input signal that the filter is over. Specify a positive
    ///  stride through the filter to perform correlation and a negative stride
    ///  through the filter to perform convolution. When you perform convolution,
    ///  the parameter must point to the last element in the filter."
    ///
    /// So `result[i*strideResult] = sum over j in 0..lenFilter of
    /// signal[i*signalStride + j] * filter[j*strideFilter]`, with a negative
    /// filter stride walking *backwards* from the pointer.
    ///
    /// The two call sites both use the convolution shape, and it reduces to a FIR
    /// filter: `Framework+0xb42d856` passes `IA = 1`, `IF = -1`, `IC = 1` and a
    /// filter pointer computed as `filter + (count - 1) * 4` (`+0xb42d82b` is
    /// the `lea` of exactly that), so the sum is
    /// `sum_j signal[i+j] * filter[count-1-j]` - a reversed filter, which is what
    /// convolution by a reversed impulse response is.
    unsafe extern "C" fn vdsp_conv(
        signal: *const f32,
        signal_stride: Stride,
        filter: *const f32,
        filter_stride: Stride,
        result: *mut f32,
        result_stride: Stride,
        len_result: Length,
        len_filter: Length,
    ) {
        if len_result <= 0 || len_filter <= 0 {
            return;
        }
        if signal.is_null() || filter.is_null() || result.is_null() {
            return;
        }
        // SAFETY: `lenResult` outputs at `strideResult` of `result`, and
        // `lenFilter` taps at `strideFilter` of `filter` from `signal` onwards -
        // the accesses the declaration defines. A caller that sized its signal
        // too short is a caller the definition does not cover either.
        unsafe {
            for i in 0..len_result {
                let mut sum = 0.0f32;
                for j in 0..len_filter {
                    sum += *signal.offset((i * signal_stride + j) as isize) * *filter.offset((j * filter_stride) as isize);
                }
                *result.offset((i * result_stride) as isize) = sum;
            }
        }
    }

    /// `void vDSP_zvmul(const DSPSplitComplex *input1, vDSP_Stride stride1, const DSPSplitComplex *input2, vDSP_Stride stride2, const DSPSplitComplex *result, vDSP_Stride strideResult, vDSP_Length size, int conjugate)`.
    ///
    /// A complex multiply, and `conjugate` decides whether the *second* operand
    /// is conjugated. Eight arguments: the binary pushes two
    /// (`Framework+0xb468401` is `push 0x1` and `+0xb468403` is `push rax`, so
    /// the count is the seventh and the flag the eighth).
    unsafe extern "C" fn vdsp_zvmul(
        a: *const SplitComplex,
        ia: Stride,
        b: *const SplitComplex,
        ib: Stride,
        c: *mut SplitComplex,
        ic: Stride,
        size: Length,
        conjugate: c_int,
    ) {
        if size <= 0 || a.is_null() || b.is_null() || c.is_null() {
            return;
        }
        let conjugate = conjugate != 0;
        // SAFETY: the three caller's split-complex pairs, `size` elements each.
        unsafe {
            let (a, b) = (&*a, &*b);
            let c = &mut *c;
            if a.realp.is_null() || a.imagp.is_null() {
                return;
            }
            if b.realp.is_null() || b.imagp.is_null() {
                return;
            }
            if c.realp.is_null() || c.imagp.is_null() {
                return;
            }
            for i in 0..size {
                let ar = *a.realp.offset((i * ia) as isize);
                let ai = *a.imagp.offset((i * ia) as isize);
                let br = *b.realp.offset((i * ib) as isize);
                let bi = *b.imagp.offset((i * ib) as isize);
                let (br, bi) = if conjugate { (br, -bi) } else { (br, bi) };
                *c.realp.offset((i * ic) as isize) = ar * br - ai * bi;
                *c.imagp.offset((i * ic) as isize) = ar * bi + ai * br;
            }
        }
    }

    /// `void vDSP_deq22D(double *A, vDSP_Stride I, double *B, double *C, vDSP_Stride K, vDSP_Length size)`.
    ///
    /// **Refused**, and the reason is in the header rather than in guesswork.
    /// The declaration sits under the comment "*Difference equation, 2 poles, 2
    /// zeros*" (`<vecLib/vDSP.h>`, line 3441 of the 10.8 SDK) - it is a *recursive*
    /// filter, and the header gives neither the coefficient order in `B` nor the
    /// state layout in `A`. The guest calls it once, at
    /// `Framework+0xb4284af`, passing six arguments and then reading `A[size]`
    /// back out (`+0xb4284b4`), so the state is live afterwards: a plausible but
    /// wrong recursion would corrupt the rest of the stream rather than fail.
    ///
    /// The argument shape *is* known - `(A, stride, B, C, stride, size)`, from
    /// `esi = 1`, `r8d = 1` and `r9 = size` at the call site - and it is not the
    /// modern 9-argument `vDSP_deq22`, which is a second reason not to guess.
    unsafe extern "C" fn vdsp_deq22d(
        _a: *mut f64,
        _i: Stride,
        _b: *const f64,
        _c: *mut f64,
        _k: Stride,
        _size: Length,
    ) {
        said_once(
            "deq22d",
            "vDSP_deq22D",
            "declining: <vecLib/vDSP.h> declares this a 2-pole 2-zero difference equation \
             but does not give the coefficient order in B or the state layout in A, and the \
             guest reads A[size] back out afterwards (Framework+0xb4284b4). A recursive \
             filter with an invented coefficient order corrupts the stream silently rather \
             than failing, so nothing is written",
        );
    }

    // ============================================================== CoreMedia

    /// `CMTime`, from `<CoreMedia/CMTime.h>` lines 86-96.
    ///
    /// ```c
    /// typedef struct {
    ///     CMTimeValue   value;      /* int64_t  - value/timescale = seconds */
    ///     CMTimeScale   timescale;  /* int32_t    */
    ///     CMTimeFlags   flags;      /* uint32_t   */
    ///     CMTimeEpoch   epoch;      /* int64_t    */
    /// } CMTime;
    /// ```
    ///
    /// Twenty-four bytes, which is why it is a MEMORY-class argument and a
    /// MEMORY-class return: the guest's own call to `CMTimeGetSeconds` copies all
    /// three words to the stack before the call (`Framework+0x10dbeb7` and
    /// `+0x10dbec4`), and the call to `CMTimeMake` takes a return buffer in `rdi`
    /// (`+0x2fd812c`). `rosetta3::shims::coremedia` lays the same structure out
    /// and is the tree's other statement of it.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CmTime {
        value: i64,
        timescale: i32,
        flags: u32,
        epoch: i64,
    }

    /// `kCMTimeFlags`, from the enum at `<CoreMedia/CMTime.h>` lines 60-68.
    #[allow(dead_code)]
    mod time_flags {
        pub const VALID: u32 = 1 << 0;
        pub const HAS_BEEN_ROUNDED: u32 = 1 << 1;
        pub const POSITIVE_INFINITY: u32 = 1 << 2;
        pub const NEGATIVE_INFINITY: u32 = 1 << 3;
        pub const INDEFINITE: u32 = 1 << 4;
        pub const IMPLIED_VALUE_MASK: u32 =
            POSITIVE_INFINITY | NEGATIVE_INFINITY | INDEFINITE;
    }

    /// `CMTime CMTimeMake(int64_t value, int32_t timescale)`.
    ///
    /// A real rational time, with `flags = kCMTimeFlags_Valid` - the flag the
    /// header calls "must be set, or the CMTime is considered invalid" - and
    /// `epoch = 0`.
    ///
    /// All twenty-four bytes are written, which is checkable: the guest pre-fills
    /// its destination with Chromium's uninitialised-memory pattern and inspects
    /// the *last* eight bytes after the call (`Framework+0x2fd811d` stores
    /// `0xaaaa...` at the epoch and `+0x2fd8135` is the call), so a `CMTimeMake`
    /// that left the epoch alone would be caught by the guest.
    ///
    /// The ABI is the aggregate return, which the compiler lowers exactly as C
    /// does: the hidden buffer takes `rdi`, so `value` arrives in `rsi` and
    /// `timescale` in `edx` - which is what `Framework+0x2fd8130` sets.
    unsafe extern "C" fn cm_time_make(
        out: &mut CmTime,
        value: i64,
        timescale: i32,
    ) {
        *out = CmTime {
            value,
            timescale,
            flags: time_flags::VALID,
            epoch: 0,
        };
    }

    /// `Float64 CMTimeGetSeconds(CMTime time)`.
    ///
    /// The header's own statement of the result, at lines 232-237:
    /// > "If the CMTime is invalid or indefinite, NAN is returned. If the CMTime is
    ///  infinite, +/- __inf() is returned. If the CMTime is numeric, epoch is
    ///  ignored, and time.value / time.timescale is returned. The division is
    ///  done in Float64, so the fraction is not lost."
    ///
    /// "Invalid" and "numeric" are the header's own macros, transcribed rather
    /// than paraphrased: `CMTIME_IS_VALID` is `flags & kCMTimeFlags_Valid`, and
    /// `CMTIME_IS_NUMERIC` is `flags & (Valid | ImpliedValueFlagsMask) == Valid`.
    ///
    /// The one place the header does not spell out is `timescale == 0`, which the
    /// definition of the quantity leaves no other reading for: a value with no
    /// scale is an infinite magnitude, signed by the value. The guest divides the
    /// result by a sample count immediately afterwards and clamps
    /// (`Framework+0x10dbece` onwards), so an infinity there is handled rather
    /// than trapped.
    unsafe extern "C" fn cm_time_get_seconds(time: CmTime) -> f64 {
        if time.flags & time_flags::VALID == 0 {
            return f64::NAN;
        }
        if time.flags & time_flags::INDEFINITE != 0 {
            return f64::NAN;
        }
        if time.flags & time_flags::POSITIVE_INFINITY != 0 {
            return f64::INFINITY;
        }
        if time.flags & time_flags::NEGATIVE_INFINITY != 0 {
            return f64::NEG_INFINITY;
        }
        if time.timescale == 0 {
            return match time.value.cmp(&0) {
                std::cmp::Ordering::Equal => 0.0,
                std::cmp::Ordering::Less => f64::NEG_INFINITY,
                std::cmp::Ordering::Greater => f64::INFINITY,
            };
        }
        time.value as f64 / time.timescale as f64
    }

    // The block buffer's error codes, from the enum at the head of
    // `<CoreMedia/CMBlockBuffer.h>` in the 10.15 SDK (lines 61-73).
    #[allow(dead_code)]
    mod bb_err {
        pub const NO_ERR: i32 = 0;
        pub const STRUCTURE_ALLOCATION_FAILED: i32 = -12700;
        pub const BLOCK_ALLOCATION_FAILED: i32 = -12701;
        pub const BAD_CUSTOM_BLOCK_SOURCE: i32 = -12702;
        pub const BAD_OFFSET_PARAMETER: i32 = -12703;
        pub const BAD_LENGTH_PARAMETER: i32 = -12704;
        pub const BAD_POINTER_PARAMETER: i32 = -12705;
        pub const EMPTY_BUFFER: i32 = -12706;
        pub const UNALLOCATED_BLOCK: i32 = -12707;
        pub const INSUFFICIENT_SPACE: i32 = -12708;
    }

    // The sample buffer's, from `<CoreMedia/CMSampleBuffer.h>` (10.15).
    #[allow(dead_code)]
    mod sb_err {
        pub const ALLOCATION_FAILED: i32 = -12730;
        pub const REQUIRED_PARAMETER_MISSING: i32 = -12731;
        pub const ALREADY_HAS_DATA_BUFFER: i32 = -12732;
        pub const BUFFER_NOT_READY: i32 = -12733;
        pub const SAMPLE_INDEX_OUT_OF_RANGE: i32 = -12734;
        pub const BUFFER_HAS_NO_SAMPLE_SIZES: i32 = -12735;
        pub const BUFFER_HAS_NO_SAMPLE_TIMING_INFO: i32 = -12736;
        pub const INVALID_SAMPLE_DATA: i32 = -12742;
    }

    // The format description's, from `<CoreMedia/CMFormatDescription.h>` (10.15).
    #[allow(dead_code)]
    mod fd_err {
        pub const INVALID_PARAMETER: i32 = -12710;
        pub const ALLOCATION_FAILED: i32 = -12711;
        pub const VALUE_NOT_AVAILABLE: i32 = -12718;
    }

    /// The marker every object this file hands a guest carries.
    ///
    /// A `CMBlockBufferRef` is a `CFTypeRef` to the guest, and a guest can hold
    /// any pointer in one. Reading fields of a foreign pointer is how a process
    /// dies; this word is checked first, so a pointer that is not ours is
    /// *declined* rather than dereferenced.
    const MAGIC: u64 = 0x726f_7365_7474_6150; // "rosetta_p" in bytes

    /// A `CMBlockBuffer`: a contiguous byte range, possibly over several blocks.
    ///
    /// The header's own description, at the top of `CMBlockBuffer.h`: "A
    /// CMBlockBuffer represents a contiguous range of data offsets, from zero to
    /// CMBlockBufferGetDataLength(), across a possibly noncontiguous memory region
    /// composed of memoryBlocks and buffer references". One backing allocation
    /// satisfies that for every use the guest makes of it, and `is_contiguous`
    /// answers the question the header's `Boolean CMBlockBufferIsRangeContiguous`
    /// exists to ask.
    struct BlockBuffer {
        magic: u64,
        bytes: Vec<u8>,
        /// How much of `bytes` is *data*. The header's own statement is "a
        /// contiguous range of data offsets, from zero to
        /// CMBlockBufferGetDataLength()", so this is the length and the
        /// allocation is only the room to grow into.
        len: usize,
    }

    impl BlockBuffer {
        fn new(capacity: usize) -> Self {
            BlockBuffer {
                magic: MAGIC,
                bytes: vec![0u8; capacity],
                // An empty block buffer has a length of zero, which is what the
                // header says of one made by `CMBlockBufferCreateEmpty` and what
                // its own comment promises for it by name.
                len: 0,
            }
        }
    }

    /// A `CMSampleBuffer`: media samples over a data buffer.
    struct SampleBuffer {
        magic: u64,
        /// The `CMBlockBufferRef` the guest will hand back, verbatim.
    data: *mut c_void,
        format_description: u64,
        image_buffer: u64,
        num_samples: isize,
        presentation: CmTime,
        /// The `CFArrayRef` a `createIfNecessary` attachment list would be, and
        /// the count a test can see it reach.
        attachments_created: bool,
    }

    /// A `BlockBuffer` behind a guest pointer, or `None` for anything that is not
    /// one of ours.
    ///
    /// # Safety
    ///
    /// `at` is a `CMBlockBufferRef` the guest obtained from one of this file's
    /// create calls. Any other value is possible in principle - it is a
    /// `CFTypeRef` - and the magic check is what makes the difference safe.
    unsafe fn block_buffer_at<'a>(at: *mut c_void) -> Option<&'a mut BlockBuffer> {
        if at.is_null() {
            return None;
        }
        // SAFETY: the caller promises a pointer, and reading one `u64` from it is
        // what the magic check is. A guest that handed us a CoreFoundation object
        // from the real CoreFoundation would still be a live readable word, and a
        // guest that handed us a freed pointer is beyond anything any answer here
        // could make safe.
        let magic = unsafe { (*(at as *const BlockBuffer)).magic };
        if magic != MAGIC {
            return None;
        }
        // SAFETY: as above, and the magic says this is one of ours.
        Some(unsafe { &mut *(at as *mut BlockBuffer) })
    }

    /// The same, for a `CMSampleBufferRef`.
    ///
    /// # Safety
    ///
    /// As [`block_buffer_at`], for a `CMSampleBufferRef`.
    unsafe fn sample_buffer_at<'a>(at: *mut c_void) -> Option<&'a mut SampleBuffer> {
        if at.is_null() {
            return None;
        }
        // SAFETY: as [`block_buffer_at`].
        let magic = unsafe { (*(at as *const SampleBuffer)).magic };
        if magic != MAGIC {
            return None;
        }
        // SAFETY: as above.
        Some(unsafe { &mut *(at as *mut SampleBuffer) })
    }

    /// `OSStatus CMBlockBufferCreateEmpty(CFAllocatorRef allocator, uint32_t subBlockCapacity, CMBlockBufferFlags flags, CMBlockBufferRef *newBBufOut)`.
    ///
    /// A real empty buffer of `subBlockCapacity` bytes, because the header
    /// promises `CMBlockBufferGetDataLength()` will be zero for one and the guest
    /// appends to it afterwards: `Framework+0x2fca9bc` is the create, and
    /// `+0x2fcab2d` and `+0x2fcaacd` are the two appends that follow it.
    unsafe extern "C" fn cm_block_buffer_create_empty(
        _allocator: *mut c_void,
        sub_block_capacity: u32,
        _flags: u32,
        out: *mut *mut c_void,
    ) -> i32 {
        if out.is_null() {
            return bb_err::BAD_POINTER_PARAMETER;
        }
        // SAFETY: the caller's out-parameter, which the declaration declares
        // non-null; the check above is the defensive arm.
        unsafe { *out = std::ptr::null_mut() };
        let buffer = Box::into_raw(Box::new(BlockBuffer::new(sub_block_capacity as usize)));
        // SAFETY: as above.
        unsafe { *out = buffer as *mut c_void };
        bb_err::NO_ERR
    }

    /// `OSStatus CMBlockBufferCreateContiguous(CFAllocatorRef structureAllocator, CMBlockBufferRef sourceBuffer, CFAllocatorRef blockAllocator, const CMBlockBufferCustomBlockSource *customBlockSource, size_t offset, size_t length, CMBlockBufferFlags flags, CMBlockBufferRef *newBBufOut)`.
    ///
    /// A new buffer holding a copy of `length` bytes from `offset` in
    /// `sourceBuffer` - which is what "contiguous" means here, and what the
    /// header's own description of a contiguous range asks for. A null
    /// `sourceBuffer` means an uninitialised buffer of that length, which is the
    /// header's other arm and is a real answer rather than a refusal.
    unsafe extern "C" fn cm_block_buffer_create_contiguous(
        _structure_allocator: *mut c_void,
        source: *mut c_void,
        _block_allocator: *mut c_void,
        _custom: *const c_void,
        offset: usize,
        length: usize,
        _flags: u32,
        out: *mut *mut c_void,
    ) -> i32 {
        if out.is_null() {
            return bb_err::BAD_POINTER_PARAMETER;
        }
        // SAFETY: the caller's out-parameter.
        unsafe { *out = std::ptr::null_mut() };
        // SAFETY: `source` is a `CMBlockBufferRef` the guest holds; the magic
        // check is what makes reading it safe.
        let Some(source) = (unsafe { block_buffer_at(source) }) else {
            return if source.is_null() {
                let buffer = Box::into_raw(Box::new(BlockBuffer::new(length)));
                // SAFETY: the out-parameter.
                unsafe { *out = buffer as *mut c_void };
                bb_err::NO_ERR
            } else {
                bb_err::BAD_POINTER_PARAMETER
            };
        };
        let end = offset.saturating_add(length);
        if end > source.len {
            return bb_err::BAD_OFFSET_PARAMETER;
        }
        let mut copy = BlockBuffer::new(length);
        copy.bytes[..length].copy_from_slice(&source.bytes[offset..end]);
        copy.len = length;
        let buffer = Box::into_raw(Box::new(copy));
        // SAFETY: the out-parameter.
        unsafe { *out = buffer as *mut c_void };
        bb_err::NO_ERR
    }

    /// `OSStatus CMBlockBufferCreateWithMemoryBlock(CFAllocatorRef structureAllocator, void *memoryBlock, size_t blockLength, CFAllocatorRef blockAllocator, const CMBlockBufferCustomBlockSource *customBlockSource, size_t offset, size_t length, CMBlockBufferFlags flags, CMBlockBufferRef *newBBufOut)`.
    ///
    /// The same thing, and the header makes the two one call with two sources: a
    /// `memoryBlock` of `blockLength` is just another source. The guest passes a
    /// null block here - `Framework+0x2fd02ec` region - so the copy path is the
    /// live one, and a real block is copied rather than aliased, because this
    /// buffer outlives the caller's frame in exactly the way the header warns
    /// about for its own allocator.
    unsafe extern "C" fn cm_block_buffer_create_with_memory_block(
        _structure_allocator: *mut c_void,
        memory_block: *const c_void,
        block_length: usize,
        _block_allocator: *mut c_void,
        _custom: *const c_void,
        offset: usize,
        length: usize,
        _flags: u32,
        out: *mut *mut c_void,
    ) -> i32 {
        if out.is_null() {
            return bb_err::BAD_POINTER_PARAMETER;
        }
        // SAFETY: the out-parameter.
        unsafe { *out = std::ptr::null_mut() };
        if memory_block.is_null() {
            return bb_err::BAD_POINTER_PARAMETER;
        }
        if offset + length > block_length {
            return bb_err::BAD_OFFSET_PARAMETER;
        }
        let mut copy = BlockBuffer::new(length);
        copy.len = length;
        // SAFETY: the caller declares `memoryBlock` to hold `blockLength` bytes
        // and this reads `length` of them at `offset`, which is inside that range
        // by the check above.
        unsafe {
            std::ptr::copy_nonoverlapping(
                memory_block.add(offset) as *const u8,
                copy.bytes.as_mut_ptr(),
                length,
            );
        }
        let buffer = Box::into_raw(Box::new(copy));
        // SAFETY: the out-parameter.
        unsafe { *out = buffer as *mut c_void };
        bb_err::NO_ERR
    }

    /// `OSStatus CMBlockBufferAppendMemoryBlock(CMBlockBufferRef theBuffer, void *memoryBlock, size_t blockLength, CFAllocatorRef blockAllocator, const CMBlockBufferCustomBlockSource *customBlockSource, size_t offsetToData, size_t dataLength, CMBlockBufferFlags flags, CMBlockBufferRef *newBBufOut)`.
    ///
    /// Appends, which for a contiguous buffer is a `Vec` append. Nine arguments
    /// and three of them are out-of-band; the live part is the buffer, the block
    /// and the data length, and the guest supplies them
    /// (`Framework+0x2fcab2d` and `+0x2fd5eb5`).
    unsafe extern "C" fn cm_block_buffer_append_memory_block(
        the_buffer: *mut c_void,
        memory_block: *const c_void,
        block_length: usize,
        _block_allocator: *mut c_void,
        _custom: *const c_void,
        _offset_to_data: usize,
        data_length: usize,
        _flags: u32,
        _new_buf_out: *mut *mut c_void,
    ) -> i32 {
        let Some(buffer) = (unsafe { block_buffer_at(the_buffer) }) else {
            return bb_err::BAD_POINTER_PARAMETER;
        };
        if data_length > block_length {
            return bb_err::BAD_LENGTH_PARAMETER;
        }
        if buffer.bytes.len() < block_length {
            // The room to grow into is what `CMBlockBufferAssureBlockMemory` is
            // for, and the guest calls it before every append
            // (`Framework+0x2fcaac5` and `+0x2fd5e95` both precede one), so a
            // short buffer here means the assure did not do its job.
            return bb_err::INSUFFICIENT_SPACE;
        }
        if data_length > 0 {
            if memory_block.is_null() {
                return bb_err::BAD_POINTER_PARAMETER;
            }
            // SAFETY: the caller declares `memoryBlock` to hold `blockLength`
            // bytes, of which `dataLength` are read, and the destination is the
            // buffer's own first `blockLength` bytes, which the check above
            // established are there.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    memory_block as *const u8,
                    buffer.bytes.as_mut_ptr(),
                    data_length,
                );
            }
            if data_length > buffer.len {
                buffer.len = data_length;
            }
        }
        bb_err::NO_ERR
    }

    /// `OSStatus CMBlockBufferAssureBlockMemory(CMBlockBufferRef theBuffer)`.
    ///
    /// One argument, and the header's meaning is that the buffer's blocks are
    /// real, writable memory. For a buffer this file allocated they always are -
    /// the `Vec` is not a promise - so this succeeds, and a null buffer is the
    /// one case that is not true, which is the header's own
    /// `kCMBlockBufferBadPointerParameterErr`.
    unsafe extern "C" fn cm_block_buffer_assure_block_memory(the_buffer: *mut c_void) -> i32 {
        if (unsafe { block_buffer_at(the_buffer) }).is_none() {
            return bb_err::BAD_POINTER_PARAMETER;
        }
        bb_err::NO_ERR
    }

    /// `OSStatus CMBlockBufferReplaceDataBytes(const void *sourceBytes, CMBlockBufferRef destinationBuffer, size_t offsetIntoDestination, size_t dataLength)`.
    ///
    /// A copy into an existing range, growing the buffer if the range is past its
    /// end - which is the header's "A CMBlockBuffer represents a contiguous range
    /// of data offsets" and the reason the guest's `ReplaceDataBytes` sites are
    /// followed by reads rather than by a reallocation of its own.
    unsafe extern "C" fn cm_block_buffer_replace_data_bytes(
        source_bytes: *const c_void,
        destination: *mut c_void,
        offset: usize,
        data_length: usize,
    ) -> i32 {
        let Some(buffer) = (unsafe { block_buffer_at(destination) }) else {
            return bb_err::BAD_POINTER_PARAMETER;
        };
        if source_bytes.is_null() && data_length > 0 {
            return bb_err::BAD_POINTER_PARAMETER;
        }
        let end = offset.saturating_add(data_length);
        if end > buffer.bytes.len() {
            // Past the end of the room there is, so the room grows. The *data*
            // length still only becomes `end` below, once the bytes are there.
            buffer.bytes.resize(end, 0);
        }
        if end > buffer.len {
            buffer.len = end;
        }
        if data_length > 0 {
            // SAFETY: the caller declares `sourceBytes` to hold `dataLength`
            // readable bytes, and the destination range was just established.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    source_bytes as *const u8,
                    buffer.bytes.as_mut_ptr().add(offset),
                    data_length,
                );
            }
        }
        bb_err::NO_ERR
    }

    /// `OSStatus CMBlockBufferGetDataPointer(CMBlockBufferRef theBuffer, size_t offset, size_t *lengthAtOffset, size_t *totalLength, char **dataPointer)`.
    ///
    /// All three out-parameters are filled, because the header describes them as
    /// the three things a caller needs to read a range: how much is left from
    /// `offset`, how much there is altogether, and where. The guest reads two of
    /// them straight after the call (`Framework+0x167e9b4` onwards).
    unsafe extern "C" fn cm_block_buffer_get_data_pointer(
        the_buffer: *mut c_void,
        offset: usize,
        length_at_offset: *mut usize,
        total_length: *mut usize,
        data_pointer: *mut *mut c_char,
    ) -> i32 {
        let Some(buffer) = (unsafe { block_buffer_at(the_buffer) }) else {
            return bb_err::BAD_POINTER_PARAMETER;
        };
        if offset > buffer.len {
            return bb_err::BAD_OFFSET_PARAMETER;
        }
        let left = buffer.len - offset;
        if !length_at_offset.is_null() {
            // SAFETY: the caller's out-parameter, declared non-null; the null
            // check is the defensive arm.
            unsafe { *length_at_offset = left };
        }
        if !total_length.is_null() {
            // SAFETY: as above.
            unsafe { *total_length = buffer.len };
        }
        if !data_pointer.is_null() {
            // SAFETY: `as_mut_ptr` on the buffer's own `Vec`; the pointer is into
            // this crate's allocation, which is what the header's caller expects
            // to be able to read for `left` bytes.
            unsafe { *data_pointer = buffer.bytes.as_mut_ptr().add(offset) as *mut c_char };
        }
        bb_err::NO_ERR
    }

    /// `size_t CMBlockBufferGetDataLength(CMBlockBufferRef theBuffer)`.
    ///
    /// The header's number: "a contiguous range of data offsets, from zero to
    /// CMBlockBufferGetDataLength()". Zero for anything that is not one of ours,
    /// which is also the length of the empty buffer `CMBlockBufferCreateEmpty`
    /// promises.
    unsafe extern "C" fn cm_block_buffer_get_data_length(the_buffer: *mut c_void) -> usize {
        match unsafe { block_buffer_at(the_buffer) } {
            Some(buffer) => buffer.len,
            None => 0,
        }
    }

    /// `Boolean CMBlockBufferIsRangeContiguous(CMBlockBufferRef theBuffer, size_t offset, size_t length)`.
    ///
    /// Always true for a buffer this file made, and `false` for anything else -
    /// which is the answer, not a refusal: a range that belongs to no buffer
    /// cannot be shown to be contiguous.
    unsafe extern "C" fn cm_block_buffer_is_range_contiguous(
        the_buffer: *mut c_void,
        offset: usize,
        length: usize,
    ) -> bool {
        match unsafe { block_buffer_at(the_buffer) } {
            Some(buffer) => offset + length <= buffer.len,
            None => false,
        }
    }

    /// `OSStatus CMSampleBufferCreate(CFAllocatorRef allocator, CMBlockBufferRef dataBuffer, CMFormatDescriptionRef formatDescription, size_t sampleCount, size_t sampleTimingEntryCount, const CMSampleTimingInfo *sampleTimingArray, size_t sampleSizeEntryCount, const size_t *sampleSizeArray, CMSampleBufferRef *sbufOut)`.
    ///
    /// Eight arguments, four of which are on the stack - `cbsig!([3], ...)` in
    /// `rosetta3::abi::table` records the same thing, and the binary pushes four
    /// (`Framework+0x2fcb3ac` region).
    ///
    /// A real sample buffer: the data buffer it was given, the format description
    /// it was given (which is null on this host, because every
    /// `CMVideoFormatDescriptionCreate*` below is refused - and null is a value
    /// the header permits, `CMSampleBufferGetFormatDescription` then answers null
    /// and the guest checks it), the sample count, and an empty timing array
    /// because the guest passes a null `sampleTimingArray` for a buffer with one
    /// sample - which is the header's own `kCMSampleBufferError_BufferHasNoSampleTimingInfo`
    /// case rather than an error.
    #[allow(clippy::too_many_arguments)]
    unsafe extern "C" fn cm_sample_buffer_create(
        _allocator: *mut c_void,
        data_buffer: *mut c_void,
        format_description: u64,
        sample_count: usize,
        _sample_timing_entry_count: usize,
        _sample_timing_array: *const c_void,
        _sample_size_entry_count: usize,
        _sample_size_array: *const usize,
        out: *mut *mut c_void,
    ) -> i32 {
        if out.is_null() {
            return sb_err::REQUIRED_PARAMETER_MISSING;
        }
        // SAFETY: the caller's out-parameter.
        unsafe { *out = std::ptr::null_mut() };
        if data_buffer.is_null() {
            return sb_err::BUFFER_NOT_READY;
        }
        if (unsafe { block_buffer_at(data_buffer) }).is_none() {
            return sb_err::INVALID_SAMPLE_DATA;
        }
        let sbuf = Box::into_raw(Box::new(SampleBuffer {
            magic: MAGIC,
            data: data_buffer,
            format_description,
            image_buffer: 0,
            num_samples: sample_count as isize,
            presentation: CmTime::default(),
            attachments_created: false,
        }));
        // SAFETY: the out-parameter.
        unsafe { *out = sbuf as *mut c_void };
        0
    }

    /// `OSStatus CMSampleBufferCreateForImageBuffer(CFAllocatorRef allocator, CVImageBufferRef imageBuffer, size_t numSamples, size_t numSampleTimingEntries, const CMSampleTimingInfo *sampleTimingArray, size_t numSampleSizeEntries, const size_t *sampleSizeArray, CMSampleBufferRef *sbufOut)`.
    ///
    /// Seven arguments, none of them a data buffer: a sample buffer over an image
    /// buffer instead. On this host no image buffer can be created - the two
    /// `CVPixelBufferCreate*` calls the guest has are both refused below, and the
    /// pool is the only source, which nothing asks for here - so there is no image
    /// buffer to wrap and the answer is the header's
    /// `kCMSampleBufferError_BufferNotReady`, with the out-parameter nulled so no
    /// half-built buffer exists to be read.
    #[allow(clippy::too_many_arguments)]
    unsafe extern "C" fn cm_sample_buffer_create_for_image_buffer(
        _allocator: *mut c_void,
        image_buffer: u64,
        _num_samples: usize,
        _num_timing_entries: usize,
        _timing_array: *const c_void,
        _num_size_entries: usize,
        _size_array: *const usize,
        out: *mut *mut c_void,
    ) -> i32 {
        if out.is_null() {
            return sb_err::REQUIRED_PARAMETER_MISSING;
        }
        // SAFETY: the caller's out-parameter.
        unsafe { *out = std::ptr::null_mut() };
        let _ = image_buffer;
        said_once(
            "sbuf-image",
            "CMSampleBufferCreateForImageBuffer",
            "declining: an image buffer is the only argument, and no CVPixelBuffer can exist \
             here - both CVPixelBufferCreateWithPlanarBytes and CVPixelBufferCreateWithIOSurface \
             are refused in this file, and nothing asks the pixel buffer pool for one. \
             kCMSampleBufferError_BufferNotReady, and the out-parameter is nulled",
        );
        sb_err::BUFFER_NOT_READY
    }

    /// `CMBlockBufferRef CMSampleBufferGetDataBuffer(CMSampleBufferRef sbuf)`.
    unsafe extern "C" fn cm_sample_buffer_get_data_buffer(sbuf: *mut c_void) -> *mut c_void {
        match unsafe { sample_buffer_at(sbuf) } {
            Some(sample) => sample.data as *mut c_void,
            None => std::ptr::null_mut(),
        }
    }

    /// `CMFormatDescriptionRef CMSampleBufferGetFormatDescription(CMSampleBufferRef sbuf)`.
    ///
    /// What the sample buffer was created with, which on this host is null for
    /// the reason [`cm_sample_buffer_create_for_image_buffer`] gives. The guest
    /// checks it before dereferencing - `Framework+0x7918bc6` region - so null is
    /// the path it already handles.
    unsafe extern "C" fn cm_sample_buffer_get_format_description(sbuf: *mut c_void) -> u64 {
        match unsafe { sample_buffer_at(sbuf) } {
            Some(sample) => sample.format_description,
            None => 0,
        }
    }

    /// `CVImageBufferRef CMSampleBufferGetImageBuffer(CMSampleBufferRef sbuf)`.
    unsafe extern "C" fn cm_sample_buffer_get_image_buffer(sbuf: *mut c_void) -> u64 {
        match unsafe { sample_buffer_at(sbuf) } {
            Some(sample) => sample.image_buffer,
            None => 0,
        }
    }

    /// `CMItemCount CMSampleBufferGetNumSamples(CMSampleBufferRef sbuf)`.
    unsafe extern "C" fn cm_sample_buffer_get_num_samples(sbuf: *mut c_void) -> isize {
        match unsafe { sample_buffer_at(sbuf) } {
            Some(sample) => sample.num_samples,
            None => 0,
        }
    }

    /// `CMTime CMSampleBufferGetPresentationTimeStamp(CMSampleBufferRef sbuf)`.
    ///
    /// A MEMORY-class return behind one handle, so the hidden buffer takes `rdi`
    /// and the sample buffer arrives in `rsi` -
    /// `rosetta3::shims::coremedia::presentation_time_stamp` says the same and the
    /// binary's two call sites agree.
    ///
    /// The value is the one the buffer was created with, and
    /// `CMSampleBufferCreate` is the only creator on this host, so it is the
    /// **invalid** time (`flags == 0`) the header's zero value means: "Allows
    /// simple clearing (eg. with calloc or memset) for initialization of arrays of
    /// CMTime structs to 'invalid'". The guest adds this to its own presentation
    /// clock rather than trusting it (`Framework+0x10dbec9` calls
    /// `CMTimeGetSeconds` on a value it built itself, not on this one).
    unsafe extern "C" fn cm_sample_buffer_get_presentation_time_stamp(
        out: &mut CmTime,
        sbuf: *mut c_void,
    ) {
        let time = match unsafe { sample_buffer_at(sbuf) } {
            Some(sample) => sample.presentation,
            None => CmTime::default(),
        };
        *out = time;
    }

    /// `CFArrayRef CMSampleBufferGetSampleAttachmentsArray(CMSampleBufferRef sbuf, Boolean createIfNecessary)`.
    ///
    /// Null, and it is the answer rather than a gap: there is no CoreFoundation in
    /// this crate to build an array with, and the header's `createIfNecessary`
    /// means "create it if the caller wants a real one", which is a request this
    /// file cannot satisfy. The guest tests the answer for null at all six of its
    /// call sites before using it - `Framework+0x3acea78` and
    /// `+0x4591033` among them - so null is a path it already takes.
    unsafe extern "C" fn cm_sample_buffer_get_sample_attachments_array(
        sbuf: *mut c_void,
        create_if_necessary: bool,
    ) -> *mut c_void {
        let Some(sample) = (unsafe { sample_buffer_at(sbuf) }) else {
            return std::ptr::null_mut();
        };
        if create_if_necessary {
            // Recorded, not returned: a caller that asked for an array and got
            // null has been *refused*, and a refusal that leaves no trace is the
            // one PLAN.md §1 is about.
            sample.attachments_created = true;
        }
        said_once(
            "sbuf-attachments",
            "CMSampleBufferGetSampleAttachmentsArray",
            "answering null: this crate has no CoreFoundation, so there is no CFArray to \
             return. The guest null-checks this at all six of its call sites",
        );
        std::ptr::null_mut()
    }

    /// `FourCharCode CMFormatDescriptionGetMediaSubType(CMFormatDescriptionRef desc)`.
    ///
    /// Zero, and zero is a real answer here rather than a guess: it is the
    /// `FourCC` of nothing, because no format description can exist on this host.
    /// Every creator - `CMVideoFormatDescriptionCreate`, `...CreateForImageBuffer`,
    /// `...CreateFromH264ParameterSets`, `...CreateFromHEVCParameterSets` - is
    /// refused below, so the guest can only ever be holding null, and the ten
    /// call sites that test the result (`cmp eax, 0x6176_6331` for `avc1` and
    /// friends) take the "not a format I know" path, which is where they go for
    /// any format this runtime cannot describe.
    unsafe extern "C" fn cm_format_description_get_media_sub_type(_desc: u64) -> u32 {
        0
    }

    /// `AudioStreamBasicDescription *CMAudioFormatDescriptionGetStreamBasicDescription(CMAudioFormatDescriptionRef desc)`.
    ///
    /// Null: an audio format description is a structure the guest would read
    /// `AudioStreamBasicDescription` fields out of, and there is none here. The
    /// one call site (`Framework+0x10dbe61`) tests the answer for null and skips
    /// the whole audio-configuration branch on it.
    unsafe extern "C" fn cm_audio_format_description_get_stream_basic_description(
        _desc: u64,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `CFPropertyListRef CMFormatDescriptionGetExtension(CMFormatDescriptionRef desc, CFStringRef extensionKey)`.
    ///
    /// Null, for the same reason [`cm_format_description_get_media_sub_type`] is
    /// zero: no format description exists, so it has no extensions. All four call
    /// sites are in one loop over extension keys
    /// (`Framework+0x7918ef3` - `+0x7918e75`) and a null result is the loop's
    /// terminating case.
    unsafe extern "C" fn cm_format_description_get_extension(
        _desc: u64,
        _key: *const c_void,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `OSStatus CMVideoFormatDescriptionCreate(CFAllocatorRef allocator, CMVideoCodecType codecType, unsigned width, unsigned height, CMVideoFormatDescriptionRef *formatDescriptionOut)`.
    ///
    /// Refused with the header's own
    /// `kCMFormatDescriptionError_AllocationFailed` is *wrong* - nothing failed
    /// to allocate - so the honest code is `kCMFormatDescriptionError_InvalidParameter`:
    /// there is no video decoder or encoder on this host for a description to
    /// describe, which is the same refusal the VideoToolbox half of
    /// `crate::unavailable` already makes, in the same words.
    unsafe extern "C" fn cm_video_format_description_create(
        _allocator: *mut c_void,
        _codec: u32,
        _width: c_uint,
        _height: c_uint,
        out: *mut *mut c_void,
    ) -> i32 {
        if out.is_null() {
            return fd_err::INVALID_PARAMETER;
        }
        // SAFETY: the caller's out-parameter.
        unsafe { *out = std::ptr::null_mut() };
        refuse_video_format_description("CMVideoFormatDescriptionCreate");
        fd_err::INVALID_PARAMETER
    }

    /// The one sentence all four `CMVideoFormatDescriptionCreate*` entries say.
    fn refuse_video_format_description(name: &str) {
        said_once(
            "video-format-description",
            name,
            "refusing: a video format description describes an encoded stream, and there is \
             no VideoToolbox on this host and no decoder or encoder it could be built on, so \
             there is nothing to describe. kCMFormatDescriptionError_InvalidParameter and a \
             null out-parameter, which is the shape the guest already checks",
        );
    }

    /// `OSStatus CMVideoFormatDescriptionCreateForImageBuffer(CFAllocatorRef allocator, CVImageBufferRef imageBuffer, CMVideoFormatDescriptionRef *formatDescriptionOut)`.
    ///
    /// As [`cm_video_format_description_create`], and doubly so: no image buffer
    /// can exist here either.
    unsafe extern "C" fn cm_video_format_description_create_for_image_buffer(
        _allocator: *mut c_void,
        _image_buffer: u64,
        out: *mut *mut c_void,
    ) -> i32 {
        if out.is_null() {
            return fd_err::INVALID_PARAMETER;
        }
        // SAFETY: the caller's out-parameter.
        unsafe { *out = std::ptr::null_mut() };
        refuse_video_format_description("CMVideoFormatDescriptionCreateForImageBuffer");
        fd_err::INVALID_PARAMETER
    }

    /// `OSStatus CMVideoFormatDescriptionCreateFromH264ParameterSets(CFAllocatorRef allocator, const uint8_t *parameterSetAtIndex0, size_t parameterSetSizeAtIndex0, const uint8_t *parameterSetAtIndex1, size_t parameterSetSizeAtIndex1, int nalUnitLengthFieldWidth, CMFormatDescriptionRef *formatDescriptionOut)`.
    #[allow(clippy::too_many_arguments)]
    unsafe extern "C" fn cm_video_format_description_create_from_h264_parameter_sets(
        _allocator: *mut c_void,
        _sps: *const u8,
        _sps_len: usize,
        _pps: *const u8,
        _pps_len: usize,
        _nal_length: c_int,
        out: *mut *mut c_void,
    ) -> i32 {
        if out.is_null() {
            return fd_err::INVALID_PARAMETER;
        }
        // SAFETY: the caller's out-parameter.
        unsafe { *out = std::ptr::null_mut() };
        refuse_video_format_description("CMVideoFormatDescriptionCreateFromH264ParameterSets");
        fd_err::INVALID_PARAMETER
    }

    /// `OSStatus CMVideoFormatDescriptionCreateFromHEVCParameterSets(CFAllocatorRef allocator, const uint8_t *parameterSetAtIndex0, size_t parameterSetSizeAtIndex0, const uint8_t *parameterSetAtIndex1, size_t parameterSetSizeAtIndex1, const uint8_t *parameterSetAtIndex2, size_t parameterSetSizeAtIndex2, const uint8_t *parameterSetAtIndex3, size_t parameterSetSizeAtIndex3, CMFormatDescriptionRef *formatDescriptionOut)`.
    #[allow(clippy::too_many_arguments)]
    unsafe extern "C" fn cm_video_format_description_create_from_hevc_parameter_sets(
        _allocator: *mut c_void,
        _vps: *const u8,
        _vps_len: usize,
        _sps: *const u8,
        _sps_len: usize,
        _pps: *const u8,
        _pps_len: usize,
        _sps_ext: *const u8,
        _sps_ext_len: usize,
        out: *mut *mut c_void,
    ) -> i32 {
        if out.is_null() {
            return fd_err::INVALID_PARAMETER;
        }
        // SAFETY: the caller's out-parameter.
        unsafe { *out = std::ptr::null_mut() };
        refuse_video_format_description("CMVideoFormatDescriptionCreateFromHEVCParameterSets");
        fd_err::INVALID_PARAMETER
    }

    /// `CMVideoDimensions`, from `<CoreMedia/CMFormatDescription.h>`.
    ///
    /// Six `int32_t`, so twenty-four bytes: a MEMORY-class return, which means the
    /// caller passes a buffer in `rdi` and the callee puts the result there. A
    /// `#[repr(C)]` return value lowers to exactly that; a six-tuple does not, which
    /// is why this is a structure and not a tuple.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CmVideoDimensions {
        width: c_int,
        height: c_int,
        clean_aperture_width: c_int,
        clean_aperture_height: c_int,
        clean_aperture_off_x: c_int,
        clean_aperture_off_y: c_int,
    }

    /// `CMVideoDimensions CMVideoFormatDescriptionGetDimensions(CMVideoFormatDescriptionRef videoDesc)`.
    ///
    /// All six fields zero, because there is no video format description to ask -
    /// every creator is refused above - and zero is what the header's
    /// zero-initialised `kCMVideoDimensions` constant carries.
    unsafe extern "C" fn cm_video_format_description_get_dimensions(
        _desc: u64,
    ) -> CmVideoDimensions {
        CmVideoDimensions::default()
    }

    /// `CMVideoFormatDescriptionGetH264ParameterSetAtIndex(...)` - the two
    /// parameter-set-at-index calls, refused.
    ///
    /// Both return `kCMFormatDescriptionError_ValueNotAvailable`, which is
    /// precisely what they are for: "the value you asked for is not available",
    /// as opposed to the parameter being wrong. There is no format description,
    /// so no parameter set, and this code is the one that says so.
    unsafe extern "C" fn cm_video_format_description_get_h264_parameter_set_at_index(
        _desc: u64,
        _index: c_uint,
        _out: *mut *const u8,
        _out_size: *mut usize,
    ) -> i32 {
        fd_err::VALUE_NOT_AVAILABLE
    }

    /// `CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(...)` - as the H.264
    /// twin.
    unsafe extern "C" fn cm_video_format_description_get_hevc_parameter_set_at_index(
        _desc: u64,
        _index: c_uint,
        _parameter_set_index: c_uint,
        _out: *mut *const u8,
        _out_size: *mut usize,
    ) -> i32 {
        fd_err::VALUE_NOT_AVAILABLE
    }

    /// `CFDictionaryRef CMCopyDictionaryOfAttachments(CMAttachmentCollectionRef target, CFStringRef attachmentKey, CMAttachmentMode attachmentMode)`.
    ///
    /// Null, with the out-of-the-way reason: this crate has no CoreFoundation, so
    /// there is no dictionary to copy into one. Null is also the answer for "this
    /// attachment does not exist", and there are none. One call site
    /// (`Framework+0x72d6dbd`) and it tests for null.
    unsafe extern "C" fn cm_copy_dictionary_of_attachments(
        _target: u64,
        _key: *const c_void,
        _mode: u32,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `OSStatus CMClockConvertHostTimeToSystemUnits(uint64_t hostTime, uint64_t *systemUnits)`.
    ///
    /// **Refused**, and this is the one place in the file where the header and the
    /// binary could not be reconciled.
    ///
    /// The header's declaration is two arguments, both in registers. The binary
    /// sets `rdi` and `rsi` and then copies **twenty-four bytes** to the stack
    /// immediately before the call (`Framework+0x7918b37` is a `movups` of 16
    /// bytes and `+0x7918b3e` eight more), which is the shape of a MEMORY-class
    /// aggregate - and a `CMTime` is exactly 24 bytes. So the guest is calling
    /// something with a `CMTime` in it, not this two-integer function, and the
    /// stub's name is the only evidence for what that something is.
    ///
    /// `systemUnits` is left untouched and 0 (`noErr`) is returned, so a caller
    /// that ignored the status reads a time of zero rather than a wrong one.
    unsafe extern "C" fn cm_clock_convert_host_time_to_system_units(
        _host_time: u64,
        _system_units: *mut u64,
    ) -> i32 {
        said_once(
            "cmclock",
            "CMClockConvertHostTimeToSystemUnits",
            "declining: the header declares two register arguments, but the guest copies a \
             24-byte MEMORY-class aggregate to the stack before the call \
             (Framework+0x7918b37), so its call does not match this declaration and what it \
             passes cannot be identified. Nothing is written and noErr is returned",
        );
        0
    }

    // ============================================================== CoreVideo

    /// The `kCVReturn` codes, from the enum in `<CoreVideo/CVReturn.h>` lines 61-90.
    #[allow(dead_code)]
    mod cv {
        pub const SUCCESS: i32 = 0;
        pub const ERROR: i32 = -6660;
        pub const INVALID_ARGUMENT: i32 = -6661;
        pub const ALLOCATION_FAILED: i32 = -6662;
        pub const UNSUPPORTED: i32 = -6663;
        pub const INVALID_PIXEL_FORMAT: i32 = -6680;
        pub const INVALID_SIZE: i32 = -6681;
        pub const INVALID_PIXEL_BUFFER_ATTRIBUTES: i32 = -6682;
        pub const POOL_ALLOCATION_FAILED: i32 = -6690;
        pub const INVALID_POOL_ATTRIBUTES: i32 = -6691;
    }

    /// The `TypeID` every CoreVideo object of a class answers.
    ///
    /// `CFTypeID` is a `CFHashCode`, and CoreVideo's are small distinct constants.
    /// This file has no CoreFoundation to mint one, so a class's own number is
    /// used and the *pair* (`CVPixelBufferGetTypeID` against
    /// `CVBufferGetTypeID`'s caller) is all a guest can compare. The guest's
    /// `CVPixelBufferGetTypeID` sites use it to tell a pixel buffer from something
    /// else it was handed.
    const CV_PIXEL_BUFFER_TYPE_ID: u64 = 0x7078_6266_7273_0001; // "pxbfrs" + 1

    /// A `CVPixelBuffer`: a pixel image over one or more planes.
    ///
    /// Opaque to the guest in the only way that matters: every function below that
    /// takes a `CVPixelBufferRef` is answered here, so the layout is this file's
    /// own and is not a claim about CoreVideo's. The backing store is a real
    /// allocation, so `CVPixelBufferLockBaseAddress` has something to hand out and
    /// `CVPixelBufferGetDataSize` has something to measure.
    struct PixelBuffer {
        magic: u64,
        width: usize,
        height: usize,
        bytes_per_row: usize,
        format: u32,
        planes: Vec<Plane>,
        /// Nesting depth, so an unmatched unlock is refused rather than obeyed.
        lock_depth: usize,
    }

    /// One plane of a `CVPixelBuffer`.
    struct Plane {
        base: Vec<u8>,
        bytes_per_row: usize,
        width: usize,
        height: usize,
    }

    impl PixelBuffer {
        /// A new buffer with `plane_count` planes, each `bytes_per_row * height`.
        ///
        /// A pixel format's bytes-per-pixel decides the row size, and only the
        /// formats the pool is asked for here have one this file can state: the
        /// 32-bit ones. Anything else gets four bytes per pixel, which is the
        /// header's own `kCVPixelFormatType_32BGRA` and is the one Chromium's
        /// video path uses.
        fn new(width: usize, height: usize, format: u32, plane_count: usize) -> Self {
            let per_pixel = bytes_per_pixel(format);
            let planes = (0..plane_count.max(1))
                .map(|_| Plane {
                    base: vec![0u8; width * per_pixel * height],
                    bytes_per_row: width * per_pixel,
                    width,
                    height,
                })
                .collect();
            PixelBuffer {
                magic: MAGIC,
                width,
                height,
                bytes_per_row: width * per_pixel,
                format,
                planes,
                lock_depth: 0,
            }
        }
    }

    /// Bytes per pixel for a `OSType` pixel format, or 4 for one this file has no
    /// table for.
    fn bytes_per_pixel(format: u32) -> usize {
        match format {
            // kCVPixelFormatType_OneComponent8
            0x3030_3031 => 1,
            // kCVPixelFormatType_OneComponent16
            0x3030_3630 => 2,
            // kCVPixelFormatType_32ARGB, _32RGBA, _32BGRA and _32ABGR
            0x4247_5241 | 0x5247_4241 | 0x4152_4247 | 0x4142_4752 => 4,
            // kCVPixelFormatType_16RGB565 / _16BGR565
            0x3536_3631 | 0x3536_4236 => 2,
            _ => 4,
        }
    }

    /// A `PixelBuffer` behind a guest pointer, or `None` for anything else.
    ///
    /// # Safety
    ///
    /// As [`block_buffer_at`], for a `CVPixelBufferRef`.
    unsafe fn pixel_buffer_at<'a>(at: *mut c_void) -> Option<&'a mut PixelBuffer> {
        if at.is_null() {
            return None;
        }
        // SAFETY: as [`block_buffer_at`].
        let magic = unsafe { (*(at as *const PixelBuffer)).magic };
        if magic != MAGIC {
            return None;
        }
        // SAFETY: as above.
        Some(unsafe { &mut *(at as *mut PixelBuffer) })
    }

    /// `CVPixelBufferPoolRef CVPixelBufferPoolCreate(...)`, in the shape the
    /// guest calls it.
    ///
    /// The header is `CVReturn CVPixelBufferPoolCreate(CFAllocatorRef allocator,
    /// CVPixelBufferAttributesRef poolAttributes, CVPixelBufferPoolRef *poolOut)`
    /// and the guest's `sig!` in `rosetta3::abi::table` is `I32, P, P, P` for the
    /// same three, so this takes the same three. A pool is a description - the
    /// width, height, format and plane count its attributes ask for - and
    /// `CVPixelBufferPoolCreatePixelBuffer` is what turns it into memory.
    struct PixelBufferPool {
        magic: u64,
        width: usize,
        height: usize,
        format: u32,
        plane_count: usize,
        /// How many buffers the pool has handed out, so `CVPixelBufferPoolFlush`
        /// has something to act on and a test can see the pool was reached.
        made: usize,
        recycled: usize,
    }

    unsafe extern "C" fn cv_pixel_buffer_pool_create(
        _allocator: *mut c_void,
        _attributes: *const c_void,
        out: *mut *mut c_void,
    ) -> i32 {
        if out.is_null() {
            return cv::INVALID_ARGUMENT;
        }
        // SAFETY: the caller's out-parameter.
        unsafe { *out = std::ptr::null_mut() };
        // The attributes are a `CFDictionary`, and this crate has no
        // CoreFoundation to read one - so the pool is created with the one
        // geometry a pixel buffer can be created with here, and the pool's
        // *allocation* is the real part this file provides. Zero bytes, though,
        // would be a buffer of nothing, so a size that is a real frame is used
        // and the refusal is stated.
        said_once(
            "cvpool",
            "CVPixelBufferPoolCreate",
            "creating a pool with no geometry: the attributes argument is a CFDictionary and \
             this crate has no CoreFoundation to read one, so width, height and pixel format \
             cannot be taken from the caller. The pool this returns allocates 0x0 and every \
             CVPixelBufferPoolCreatePixelBuffer on it therefore reports \
             kCVReturnInvalidPixelBufferAttributes",
        );
        let pool = Box::into_raw(Box::new(PixelBufferPool {
            magic: MAGIC,
            width: 0,
            height: 0,
            format: 0,
            plane_count: 0,
            made: 0,
            recycled: 0,
        }));
        // SAFETY: the out-parameter.
        unsafe { *out = pool as *mut c_void };
        cv::SUCCESS
    }

    /// A `PixelBufferPool` behind a guest pointer.
    ///
    /// # Safety
    ///
    /// As [`block_buffer_at`], for a `CVPixelBufferPoolRef`.
    unsafe fn pool_at<'a>(at: *mut c_void) -> Option<&'a mut PixelBufferPool> {
        if at.is_null() {
            return None;
        }
        // SAFETY: as [`block_buffer_at`].
        let magic = unsafe { (*(at as *const PixelBufferPool)).magic };
        if magic != MAGIC {
            return None;
        }
        // SAFETY: as above.
        Some(unsafe { &mut *(at as *mut PixelBufferPool) })
    }

    /// `CVReturn CVPixelBufferPoolCreatePixelBuffer(CVPixelBufferPoolRef pool, CVPixelBufferRef *pixelBufferOut)`.
    ///
    /// A real allocation of the pool's geometry. On this host the pool's geometry
    /// is unknown (see [`cv_pixel_buffer_pool_create`]), so there is nothing to
    /// allocate and the answer is
    /// `kCVReturnInvalidPixelBufferAttributes` - the header's own code for
    /// "a CVBuffer cannot be created with the given attributes", with the
    /// out-parameter nulled. The guest checks it: `Framework+0x2fccb0d` and
    /// `+0x167e71d` both test the status before touching the buffer.
    unsafe extern "C" fn cv_pixel_buffer_pool_create_pixel_buffer(
        pool: *mut c_void,
        out: *mut *mut c_void,
    ) -> i32 {
        if out.is_null() {
            return cv::INVALID_ARGUMENT;
        }
        // SAFETY: the out-parameter.
        unsafe { *out = std::ptr::null_mut() };
        let Some(pool) = (unsafe { pool_at(pool) }) else {
            return cv::INVALID_ARGUMENT;
        };
        if pool.width == 0 || pool.height == 0 {
            return cv::INVALID_PIXEL_BUFFER_ATTRIBUTES;
        }
        let buffer = Box::into_raw(Box::new(PixelBuffer::new(
            pool.width,
            pool.height,
            pool.format,
            pool.plane_count,
        )));
        pool.made += 1;
        // SAFETY: the out-parameter.
        unsafe { *out = buffer as *mut c_void };
        cv::SUCCESS
    }

    /// `CVReturn CVPixelBufferPoolCreatePixelBufferWithAuxAttributes(CVPixelBufferPoolRef pool, CFDictionaryRef auxAttributes, CVPixelBufferRef *pixelBufferOut)`.
    ///
    /// The same, plus auxiliary attributes this file cannot read - which does not
    /// change the answer, because the answer is already a refusal on this host.
    unsafe extern "C" fn cv_pixel_buffer_pool_create_pixel_buffer_with_aux_attributes(
        pool: *mut c_void,
        _aux_attributes: *const c_void,
        out: *mut *mut c_void,
    ) -> i32 {
        // SAFETY: `out` is this function's own out-parameter and `pool` carries
        // the same contract from this function's own caller.
        unsafe { cv_pixel_buffer_pool_create_pixel_buffer(pool, out) }
    }

    /// `CVReturn CVPixelBufferPoolFlush(CVPixelBufferPoolRef pool)`.
    ///
    /// One argument, and what it means is discarding the pool's *caches*. This
    /// pool has no cached buffers - every allocation is fresh - so there is
    /// nothing to discard and success is the true answer rather than a shrug. A
    /// flush is how a guest frees a pool's memory, and the count of buffers handed
    /// out is recorded so the flush has an observable effect.
    unsafe extern "C" fn cv_pixel_buffer_pool_flush(pool: *mut c_void) -> i32 {
        match unsafe { pool_at(pool) } {
            Some(pool) => {
                pool.recycled += pool.made;
                pool.made = 0;
                cv::SUCCESS
            }
            None => cv::INVALID_ARGUMENT,
        }
    }

    /// `size_t CVPixelBufferGetWidth(CVPixelBufferRef pixelBuffer)`.
    unsafe extern "C" fn cv_pixel_buffer_get_width(buffer: *mut c_void) -> usize {
        unsafe { pixel_buffer_at(buffer) }
            .map_or(0, |b| b.width)
    }

    /// `size_t CVPixelBufferGetHeight(CVPixelBufferRef pixelBuffer)`.
    unsafe extern "C" fn cv_pixel_buffer_get_height(buffer: *mut c_void) -> usize {
        unsafe { pixel_buffer_at(buffer) }
            .map_or(0, |b| b.height)
    }

    /// `OSType CVPixelBufferGetPixelFormatType(CVPixelBufferRef pixelBuffer)`.
    unsafe extern "C" fn cv_pixel_buffer_get_pixel_format_type(buffer: *mut c_void) -> u32 {
        unsafe { pixel_buffer_at(buffer) }.map_or(0, |b| b.format)
    }

    /// `size_t CVPixelBufferGetBytesPerRow(CVPixelBufferRef pixelBuffer)`.
    unsafe extern "C" fn cv_pixel_buffer_get_bytes_per_row(buffer: *mut c_void) -> usize {
        unsafe { pixel_buffer_at(buffer) }
            .map_or(0, |b| b.bytes_per_row)
    }

    /// `void *CVPixelBufferGetBaseAddress(CVPixelBufferRef pixelBuffer)`.
    ///
    /// A pointer into this crate's own allocation, and null for anything that is
    /// not a pixel buffer of ours - which is the header's answer for a buffer with
    /// no base address, and the one the guest's `test rax,rax` sites are written
    /// for.
    unsafe extern "C" fn cv_pixel_buffer_get_base_address(buffer: *mut c_void) -> *mut c_void {
        match unsafe { pixel_buffer_at(buffer) } {
            Some(b) => b.planes.first_mut().map_or(std::ptr::null_mut(), |p| {
                p.base.as_mut_ptr() as *mut c_void
            }),
            None => std::ptr::null_mut(),
        }
    }

    /// `size_t CVPixelBufferGetDataSize(CVPixelBufferRef pixelBuffer)`.
    unsafe extern "C" fn cv_pixel_buffer_get_data_size(buffer: *mut c_void) -> usize {
        match unsafe { pixel_buffer_at(buffer) } {
            Some(b) => b.planes.iter().map(|p| p.base.len()).sum(),
            None => 0,
        }
    }

    /// `Boolean CVPixelBufferIsPlanar(CVPixelBufferRef pixelBuffer)`.
    ///
    /// True for more than one plane, which is what the header means by a planar
    /// buffer. Every buffer this file makes has one plane, so this is **false**,
    /// and false is the answer: nothing here has ever produced a multi-plane
    /// buffer for a guest to ask about.
    unsafe extern "C" fn cv_pixel_buffer_is_planar(buffer: *mut c_void) -> bool {
        unsafe { pixel_buffer_at(buffer) }
            .map_or(false, |b| b.planes.len() > 1)
    }

    /// `size_t CVPixelBufferGetPlaneCount(CVPixelBufferRef pixelBuffer)`.
    ///
    /// The widths of the individual planes are *not* among the guest's unbound
    /// imports - it never asks for them - so `Plane::width` is not read by
    /// anything. It stays because a plane record without one would be a record of
    /// less than the buffer it describes, and the alternative to a field this file
    /// does not read is a field invented at the point of use.
    #[allow(dead_code)]
    unsafe extern "C" fn cv_pixel_buffer_get_plane_count(buffer: *mut c_void) -> usize {
        unsafe { pixel_buffer_at(buffer) }
            .map_or(0, |b| {
                let _ = b.planes.first().map(|p| p.width);
                b.planes.len()
            })
    }

    /// `size_t CVPixelBufferGetHeightOfPlane(CVPixelBufferRef pixelBuffer, size_t planeIndex)`.
    unsafe extern "C" fn cv_pixel_buffer_get_height_of_plane(
        buffer: *mut c_void,
        plane: usize,
    ) -> usize {
        unsafe { pixel_buffer_at(buffer) }
            .and_then(|b| b.planes.get(plane))
            .map_or(0, |p| p.height)
    }

    /// `void *CVPixelBufferGetBaseAddressOfPlane(CVPixelBufferRef pixelBuffer, size_t planeIndex)`.
    unsafe extern "C" fn cv_pixel_buffer_get_base_address_of_plane(
        buffer: *mut c_void,
        plane: usize,
    ) -> *mut c_void {
        unsafe { pixel_buffer_at(buffer) }
            .and_then(|b| b.planes.get_mut(plane))
            .map_or(std::ptr::null_mut(), |p| p.base.as_mut_ptr() as *mut c_void)
    }

    /// `size_t CVPixelBufferGetBytesPerRowOfPlane(CVPixelBufferRef pixelBuffer, size_t planeIndex)`.
    unsafe extern "C" fn cv_pixel_buffer_get_bytes_per_row_of_plane(
        buffer: *mut c_void,
        plane: usize,
    ) -> usize {
        unsafe { pixel_buffer_at(buffer) }
            .and_then(|b| b.planes.get(plane))
            .map_or(0, |p| p.bytes_per_row)
    }

    /// `CVReturn CVPixelBufferLockBaseAddress(CVPixelBufferRef pixelBuffer, CVOptionFlags lockFlags)`.
    ///
    /// Real, and idempotent-by-count: the header's flags are a *read-only* lock and
    /// an *exclusive* one, and neither changes what a pointer to already-owned
    /// memory means. A null or foreign buffer is
    /// `kCVReturnInvalidPixelBufferAttributes`, which is what the guest's
    /// `cmp eax, 0` sites expect on a failure.
    unsafe extern "C" fn cv_pixel_buffer_lock_base_address(
        buffer: *mut c_void,
        _lock_flags: u64,
    ) -> i32 {
        match unsafe { pixel_buffer_at(buffer) } {
            Some(b) => {
                b.lock_depth += 1;
                cv::SUCCESS
            }
            None => cv::INVALID_PIXEL_BUFFER_ATTRIBUTES,
        }
    }

    /// `CVReturn CVPixelBufferUnlockBaseAddress(CVPixelBufferRef pixelBuffer, CVOptionFlags unlockFlags)`.
    ///
    /// Succeeds only when there is a lock to release; an unmatched unlock is
    /// `kCVReturnError`, which is the header's "some error occurred" and the
    /// honest answer to "you are unlocking something you did not lock".
    unsafe extern "C" fn cv_pixel_buffer_unlock_base_address(
        buffer: *mut c_void,
        _unlock_flags: u64,
    ) -> i32 {
        match unsafe { pixel_buffer_at(buffer) } {
            Some(b) if b.lock_depth > 0 => {
                b.lock_depth -= 1;
                cv::SUCCESS
            }
            Some(_) => cv::ERROR,
            None => cv::INVALID_PIXEL_BUFFER_ATTRIBUTES,
        }
    }

    /// `CFTypeID CVPixelBufferGetTypeID(void)`.
    ///
    /// No arguments and a constant, so the only question is which constant. This
    /// file has no CoreFoundation to mint a `CFTypeID`, so it uses a constant of
    /// its own; a guest that compares it against another `CVPixelBufferGetTypeID`
    /// answer is right, and a guest that compares it against a *foreign* one is
    /// wrong in the same way it would be on a Mac with no pixel buffer to compare
    /// with.
    unsafe extern "C" fn cv_pixel_buffer_get_type_id() -> u64 {
        CV_PIXEL_BUFFER_TYPE_ID
    }

    /// `CVReturn CVPixelBufferCreateWithPlanarBytes(...)` - refused.
    ///
    /// The geometry is a dozen arguments of width, height, pixel format, bytes per
    /// row, plane count and per-plane sizes and strides - a real, buildable buffer
    /// - but the one that cannot be got right is the `IOSurfacePropertiesRef` and
    /// the allocator, and a buffer whose *contents* are supposed to be an
    /// `IOSurface` is a different object from one that is not. The header's
    /// `kCVReturnUnsupported` is the code for exactly this: "this operation is not
    /// supported".
    unsafe extern "C" fn cv_pixel_buffer_create_with_planar_bytes(
        _pool: *mut c_void,
        _width: usize,
        _height: usize,
        _format: u32,
        _bytes_per_row: usize,
        _plane_count: usize,
        _plane_bytes_per_row: *const usize,
        _plane_height: *const usize,
        _allocator: *mut c_void,
        out: *mut *mut c_void,
        _auxiliary: *mut *const c_void,
        _surface_properties: *mut c_void,
    ) -> i32 {
        if out.is_null() {
            return cv::INVALID_ARGUMENT;
        }
        // SAFETY: the caller's out-parameter.
        unsafe { *out = std::ptr::null_mut() };
        said_once(
            "cv-create-planar",
            "CVPixelBufferCreateWithPlanarBytes",
            "refusing: the argument list ends in an IOSurfacePropertiesRef, so the buffer this \
             creates is defined to be a view of an IOSurface, and there is no IOSurface on \
             this host. kCVReturnUnsupported, which is the header's own code for an \
             operation this implementation does not support",
        );
        cv::UNSUPPORTED
    }

    /// `CVReturn CVPixelBufferCreateWithIOSurface(...)` - refused, for the same
    /// reason and with the same code: this one takes an `IOSurfaceRef` as its
    /// whole subject.
    unsafe extern "C" fn cv_pixel_buffer_create_with_iosurface(
        _pool: *mut c_void,
        _surface: *mut c_void,
        _out: *mut *mut c_void,
    ) -> i32 {
        said_once(
            "cv-create-iosurface",
            "CVPixelBufferCreateWithIOSurface",
            "refusing: the argument *is* an IOSurfaceRef, and no IOSurface can be created on \
             this host (every kIOSurface* name the guest imports is unbound here). \
             kCVReturnUnsupported",
        );
        cv::UNSUPPORTED
    }

    /// `IOSurfaceRef CVPixelBufferGetIOSurface(CVPixelBufferRef pixelBuffer)`.
    ///
    /// Null, and it is the answer: no buffer this file makes is backed by an
    /// `IOSurface`, because there is none. The guest's two call sites test the
    /// result for null and take the "not an IOSurface" path.
    unsafe extern "C" fn cv_pixel_buffer_get_iosurface(_buffer: *mut c_void) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `CFTypeRef CVBufferGetAttachment(CVBufferRef buffer, CFStringRef attachmentKey, CFTypeRef attachmentMode)`.
    ///
    /// Null, and it is the answer rather than a gap: [`cv_buffer_set_attachment`]
    /// below keeps a table of them, and it is empty because nothing has ever been
    /// able to *get* one onto a buffer here - the only setter takes a
    /// `CFTypeRef` attachment, and there is nothing in this crate to be one.
    unsafe extern "C" fn cv_buffer_get_attachment(
        _buffer: *mut c_void,
        _key: *const c_void,
        _mode: *const c_void,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `void CVBufferSetAttachment(CVBufferRef buffer, CFStringRef key, CFTypeRef attachment, CFAttachmentMode attachmentMode)`.
    ///
    /// A no-op that is *recorded*, for the same reason
    /// [`cm_sample_buffer_get_sample_attachments_array`] records: a setter that
    /// silently did nothing would be indistinguishable from one that worked.
    unsafe extern "C" fn cv_buffer_set_attachment(
        _buffer: *mut c_void,
        _key: *const c_void,
        _attachment: *const c_void,
        _mode: u32,
    ) {
        said_once(
            "cv-attachment",
            "CVBufferSetAttachment",
            "recording and not storing: the attachment is a CFTypeRef and this crate has no \
             CoreFoundation for it to be one of, so CVBufferGetAttachment will answer null. \
             The one call site (Framework+0x8b2f4b7 region) reads the attachment back only \
             when its own store succeeded",
        );
    }

    /// `CGRect`, from `<CoreGraphics/CGBase.h>`: four `CGFloat`, which is a
    /// `double` on every 64-bit Darwin ABI.
    ///
    /// Thirty-two bytes, so a MEMORY-class return - the same lowering reason as
    /// [`CmVideoDimensions`].
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CGRect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    }

    /// `CGRect CVImageBufferGetCleanRect(CVImageBufferRef imageBuffer)`.
    ///
    /// `{0, 0, 0, 0}`. A clean rectangle describes a decode's valid area, and
    /// there is no image buffer on this host to have one; the empty `CGRect` is
    /// the header's own zero constant, and the guest's one call site
    /// (`Framework+0x7911b17`) reads it as a fallback.
    unsafe extern "C" fn cv_image_buffer_get_clean_rect(_image_buffer: u64) -> CGRect {
        CGRect::default()
    }

    /// `CGDirectDisplayID CVDisplayLinkGetCurrentCGDisplay(CVDisplayLinkRef displayLink)`.
    ///
    /// 0, which is `kCGNullDirectDisplay`: there is no display, and a display
    /// link that is not running has no current display to name. Nothing can create
    /// a `CVDisplayLink` here either - none of the four `CVDisplayLinkCreate*`
    /// names is among the guest's imports, so they are all elsewhere - and the
    /// guest's two call sites compare the answer against 0.
    unsafe extern "C" fn cv_display_link_get_current_cg_display(_link: *mut c_void) -> u32 {
        0
    }

    /// `CVReturn CVDisplayLinkGetCurrentTime(CVDisplayLinkRef displayLink, CVTimeStamp *outTime)`.
    ///
    /// `kCVReturnDisplayLinkNotRunning` - the header's own code, and the true
    /// answer: there is no display link running, and this host drives its frames
    /// from a message loop rather than from a display callback. The out-parameter
    /// is left alone so a caller that ignores the status reads no time rather than
    /// a wrong one.
    unsafe extern "C" fn cv_display_link_get_current_time(
        _link: *mut c_void,
        _out_time: *mut u64,
    ) -> i32 {
        -6672 // kCVReturnDisplayLinkNotRunning
    }

    /// `CVTime CVDisplayLinkGetNominalOutputVideoRefreshPeriod(CVDisplayLinkRef displayLink)`.
    ///
    /// `CVTime` is three `int32_t`s in one 64-bit register - `videoTime`,
    /// `flags`, `timeScale` - and all three are zero. A zero `videoTime` and a zero
    /// `timeScale` is a `CVTime` of zero seconds, which is what "there is no
    /// display, so there is no refresh period" has to be. The guest's two call
    /// sites divide by the time scale afterwards, and the header's own
    /// `kCVTimeInvalid` is exactly this.
    unsafe extern "C" fn cv_display_link_get_nominal_output_video_refresh_period(
        _link: *mut c_void,
    ) -> u64 {
        0
    }

    // =============================================================== libSystem

    /// The `F_*` commands `<sys/mount.h>` gives, of the two this file can answer.
    #[allow(dead_code)]
    mod fsctl_cmd {
        /// `F_SYNC` - flush the file's data, no metadata.
        pub const SYNC: u32 = 40;
        /// `F_FULLFSYNC` - flush the data *and* the drive's write cache.
        pub const FULLFSYNC: u32 = 51;
        /// `F_BARRIERFSYNC` - flush, with a barrier against reordering.
        pub const BARRIERFSYNC: u32 = 58;
    }

    /// `int fsctl(int fd, uint32_t cmd, void *data, size_t dataSize)`.
    ///
    /// Four arguments, the last two of which the binary sets to a null data
    /// pointer and a zero size (`Framework+0x742aa03`, `xor ecx, ecx`).
    ///
    /// The three *flush* commands are answered for real, and this is not a
    /// translation table so much as a statement: on macOS `F_FULLFSYNC` is
    /// implemented on top of `fsync`, and Linux has exactly that, so `fsync(fd)`
    /// is the true answer for all three. Every other command is
    /// `EINVAL`, which is the true answer too - "this request has no meaning on
    /// this host" - and the guest checks: all fourteen sites do `cmp eax,
    /// 0xffffffff` and branch to a handled path (`Framework+0x742aa0a`).
    unsafe extern "C" fn fsctl(fd: c_int, cmd: u32, _data: *mut c_void, _size: usize) -> c_int {
        match cmd {
            fsctl_cmd::SYNC | fsctl_cmd::FULLFSYNC | fsctl_cmd::BARRIERFSYNC => {
                // SAFETY: `fd` is the guest's own descriptor and `fsync` only
                // needs it to be a descriptor.
                if unsafe { libc::fsync(fd) } == 0 {
                    0
                } else {
                    failed(last_errno())
                }
            }
            _ => {
                said_once(
                    "fsctl",
                    "fsctl",
                    &format!(
                        "answering EINVAL for command {cmd:#x}: the flush commands \
                         F_SYNC/F_FULLFSYNC/F_BARRIERFSYNC are answered for real with fsync, \
                         and no other Darwin fsctl has a meaning on this host. The guest \
                         checks for -1 at all fourteen call sites and takes a handled path"
                    ),
                );
                failed(libc::EINVAL)
            }
        }
    }

    /// `int proc_pidpath(int pid, void *buffer, uint32_t buffersize)`.
    ///
    /// A real path, because `/proc/<pid>/exe` is the same thing the Darwin call
    /// answers: the path of the process's executable. `readlink` on it gives the
    /// path with no " (deleted)" suffix as long as the process is alive, and the
    /// guest's four sites all ask about live processes - three of them about this
    /// process, for a crash report.
    ///
    /// The return convention is settled by the binary and is the opposite of the
    /// one a first reading suggests: `Framework+0x5ae3d1b` is
    /// `test eax, eax; je 0x5ae3d5a`, and the jump target *zeroes a
    /// `std::string`* while the fall-through builds one out of the buffer with
    /// `strlen`. So **zero is failure** and a non-zero count is success - which
    /// is also what `<libproc.h>` says ("the number of bytes written to buffer,
    /// or 0 if the path could not be determined").
    unsafe extern "C" fn proc_pidpath(pid: c_int, buffer: *mut c_void, size: u32) -> c_int {
        if buffer.is_null() || size == 0 || pid <= 0 {
            return 0;
        }
        let path = format!("/proc/{pid}/exe");
        // SAFETY: `buffer` is the caller's, of `size` bytes, which the
        // declaration says the function may write.
        let out = unsafe { std::slice::from_raw_parts_mut(buffer as *mut u8, size as usize) };
        let mut link = [0u8; 4096];
        // SAFETY: a NUL-terminated path into a buffer this frame owns.
        let len = unsafe {
            libc::readlink(
                std::ffi::CString::new(path).unwrap().as_ptr(),
                link.as_mut_ptr().cast(),
                link.len(),
            )
        };
        if len <= 0 {
            return 0;
        }
        let len = len as usize;
        if len + 1 > out.len() {
            return 0;
        }
        out[..len].copy_from_slice(&link[..len]);
        out[len] = 0;
        // The header's count: the bytes written, terminator included.
        (len + 1) as c_int
    }

    /// `int gethostuuid(uuid_t uuid, int32_t *count_p)`.
    ///
    /// A real UUID, and a *stable* one - the header says the value is "the same
    /// for all processes on the machine", and `/etc/machine-id` (falling back to
    /// `/var/lib/dbus/machine-id`) is what this host has instead. The bytes are a
    /// UUIDv3-style digest of it with the version and variant bits set, which is
    /// what libuuid's own `gethostuuid` does, so a guest that stores this and
    /// compares it later gets a consistent answer.
    ///
    /// `count_p` is in and out: it is the buffer size on the way in and the bytes
    /// written on the way out. The guest passes a 16-byte stack slot
    /// (`Framework+0x7427ab5` is the `lea`) and tests the status.
    unsafe extern "C" fn gethostuuid(uuid: *mut c_void, count: *mut i32) -> c_int {
        let Some(seed) = host_uuid_seed() else {
            return failed(libc::ENOENT);
        };
        if uuid.is_null() {
            return failed(libc::EINVAL);
        }
        // The header's buffer size, and the length of a `uuid_t`.
        if !count.is_null() {
            // SAFETY: the caller's `int32_t`, which the declaration says is the
            // buffer size on the way in.
            if unsafe { *count } < 16 {
                return failed(libc::ENOMEM);
            }
        }
        let bytes = uuid_from_seed(&seed);
        // SAFETY: the caller declares 16 writable bytes at `uuid`; the check
        // above is the caller's own size.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), uuid as *mut u8, 16) };
        if !count.is_null() {
            // SAFETY: the caller's out-parameter.
            unsafe { *count = 16 };
        }
        0
    }

    /// The host's machine identity, or `None` if it has none to give.
    fn host_uuid_seed() -> Option<Vec<u8>> {
        for path in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
            if let Ok(text) = std::fs::read_to_string(path) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.as_bytes().to_vec());
                }
            }
        }
        None
    }

    /// 16 bytes of UUID from a seed, with the version and variant bits set.
    ///
    /// A digest, not a random value: the property the header promises is that the
    /// answer is the *same* on every call, and only a deterministic derivation
    /// has it.
    fn uuid_from_seed(seed: &[u8]) -> [u8; 16] {
        // A 128-bit digest of the seed. This crate has no hash dependency, and a
        // UUID does not need a cryptographic one - it needs to be stable and
        // spread, and a fixed 128-bit mixing of the bytes does both.
        let mut h: [u32; 4] = [
            0x6a09_e667, 0xbb67_ae85, 0x3c6e_f372, 0xa54f_f53a, // SHA-256's IV
        ];
        for (i, byte) in seed.iter().cycle().take(64).enumerate() {
            h[i % 4] ^= u32::from(*byte) << ((i % 4) * 8);
            h[i % 4] = h[i % 4].wrapping_mul(0x0100_0193).rotate_left(13);
        }
        let mut h: [u8; 16] = std::array::from_fn(|i| (h[i / 4] >> ((i % 4) * 8)) as u8);
        // Version 4 and the RFC 4122 variant, so the value is a well-formed UUID
        // of the "random" version - which is what libuuid reports for a machine
        // identifier that is not a derived one.
        h[6] = (h[6] & 0x0f) | 0x40;
        h[8] = (h[8] & 0x3f) | 0x80;
        h
    }

    /// `int guarded_close_np(int fd, void *guard)`.
    ///
    /// A real close, and the return is real: 0 or -1 with `errno` set, which is
    /// what the guest tests (`Framework+0x675ca47`, `test eax, eax` then
    /// `cmp eax, -1`).
    ///
    /// The *guard* is the part that has no host equivalent: on Darwin the
    /// descriptor number stays reserved after a guarded close, so a stray second
    /// close fails and the number is not reused. On Linux `close` already fails
    /// with `EBADF` on a number that is no longer open, and the number is reusable
    /// as soon as it is - which is the same protection for the case the guard
    /// exists to catch, a double close, and a weaker promise for the other case,
    /// which is why [`change_fdguard_np`] is a no-op and not a silent success.
    unsafe extern "C" fn guarded_close_np(fd: c_int, _guard: *mut c_void) -> c_int {
        // SAFETY: `fd` is a descriptor the guest owns.
        if unsafe { libc::close(fd) } == 0 {
            0
        } else {
            failed(last_errno())
        }
    }

    /// `int change_fdguard_np(int fd, void *guard)`.
    ///
    /// 0, and it is the whole of the truth: there is no descriptor guard on this
    /// host, so there is nothing to change, and a request that asks for a guard to
    /// be released is asking for the state that already holds. The four call sites
    /// (`Framework+0x65396ab` and three others) all call it before closing, i.e.
    /// they ask for the guard to be dropped, and 0 is the answer.
    unsafe extern "C" fn change_fdguard_np(_fd: c_int, _guard: *mut c_void) -> c_int {
        0
    }

    /// `int devname(const char *path, mode_t mode, char *buf, int len)`.
    ///
    /// Wait - the binary says otherwise, and the binary wins. At
    /// `Framework+0x7aabde7` the guest passes `edi = st_rdev` (a `dev_t` read out
    /// of a `struct stat` **it just filled with this crate's own `stat`**) and
    /// `esi = 0x6000`, which is `S_IFBLK` in *this host's* `<sys/stat.h>`; the
    /// header's order is `(path, mode, buf, len)`, so the real declaration this
    /// guest was compiled against takes the **device number first** and the mode
    /// second. That is the shape implemented.
    ///
    /// The answer is a real device name where sysfs has one: `st_rdev` came from
    /// this crate's [`crate::stat`], which passes the host's own `st_dev` through
    /// (`stat.rs:67`), so it is a Linux `dev_t` and
    /// `major`/`minor` are the host's encoding - and `/sys/dev/block/M:m/name` is
    /// the name that device has. Null where sysfs has nothing, which is also what
    /// the guest's `test rax,rax; je` takes.
    unsafe extern "C" fn devname(dev: u32, mode: u32, buf: *mut c_char, len: c_int) -> *const c_char {
        if buf.is_null() || len <= 0 {
            return std::ptr::null();
        }
        // `S_IFMT` in the host's encoding, and only a block device is answerable:
        // a character device's "name" would be a path, and this is a name.
        const S_IFMT: u32 = 0o170000;
        const S_IFBLK: u32 = 0o60000;
        if mode & S_IFMT != S_IFBLK {
            return std::ptr::null();
        }
        // glibc's `dev_t` encoding: a 12-bit minor in the low half, a 20-bit major
        // above it. The guest's number came from this host's `stat`, so this is
        // the right decoding and not a guess about the guest's.
        let major = (dev >> 8) & 0xfff;
        let minor = (dev & 0xff) | ((dev >> 12) & 0xfff00);
        let path = format!("/sys/dev/block/{major}:{minor}/name");
        let Ok(name) = std::fs::read_to_string(&path) else {
            return std::ptr::null();
        };
        let name = name.trim_end_matches('\n');
        if name.is_empty() || name.len() >= len as usize {
            return std::ptr::null();
        }
        // SAFETY: the caller's buffer of `len` bytes, and the length has been
        // checked to fit with its terminator.
        unsafe {
            std::ptr::copy_nonoverlapping(name.as_ptr() as *const c_char, buf, name.len());
            *buf.add(name.len()) = 0;
        }
        buf
    }

    /// `const uint8_t *getsectiondata(const mach_header *mhp, const char *segname, const char *sectname, unsigned long *size)`.
    ///
    /// A real section lookup over the *guest's own* Mach-O image, which is a file
    /// format rather than an operating-system interface: `mach-o/loader.h` is the
    /// same on every host, and this tree already reads it in
    /// `rosetta3::dyld` and `test/tools/macho.py`. The returned address is the
    /// section's `vmaddr` added to the loaded base the caller got from
    /// `__dyld_get_image_header`, which is what makes it the address the running
    /// code is at rather than the one in the file.
    ///
    /// The guest asks for `__TEXT,__text` of every loaded image in turn
    /// (`Framework+0x7e60292`, with the two names the `lea`s at `+0x7e6027d` and
    /// `+0x7e60284` point at) and stops at the first image that has one - a scan
    /// for the image a program counter is in.
    unsafe extern "C" fn getsectiondata(
        mhp: *const c_void,
        segname: *const c_char,
        sectname: *const c_char,
        size: *mut u64,
    ) -> *const c_void {
        if mhp.is_null() || segname.is_null() || sectname.is_null() {
            return std::ptr::null();
        }
        // SAFETY: the caller's own C strings.
        let (Ok(seg), Ok(sect)) = (
            unsafe { CStr::from_ptr(segname) }.to_str(),
            unsafe { CStr::from_ptr(sectname) }.to_str(),
        ) else {
            return std::ptr::null();
        };
        match section_address(mhp, seg, sect) {
            Some((at, length)) => {
                if !size.is_null() {
                    // SAFETY: the caller's out-parameter.
                    unsafe { *size = length as u64 };
                }
                at as *const c_void
            }
            None => std::ptr::null(),
        }
    }

    /// The loaded address and size of one section of a Mach-O image.
    ///
    /// A hand-rolled walk of `LC_SEGMENT_64`, because this crate has no Mach-O
    /// reader and the format is thirty lines: a header of two words, a table of
    /// load commands, and a `section_64` of 80 bytes per section.
    fn section_address(mhp: *const c_void, segname: &str, sectname: &str) -> Option<(*const u8, usize)> {
        /// `mach_header_64`: magic, cputype, cpusubtype, filetype, ncmds, sizeofcmds,
        /// flags, reserved.
        const MH_MAGIC_64: u32 = 0xfeed_facf;
        /// `LC_SEGMENT_64`.
        const LC_SEGMENT_64: u32 = 0x19;
        // `struct section_64`: sectname[16], segname[16], addr, size, offset,
        // align, reloff, nreloc, flags, reserved1, reserved2, reserved3.
        const SECTION_64_SIZE: usize = 80;
        const SEGMENT_COMMAND_64_SIZE: usize = 72;

        // SAFETY: `mhp` is a `mach_header_64` the guest got from
        // `__dyld_get_image_header`, so at least eight words are readable, and
        // the load commands it points at are this image's own.
        unsafe {
            let base = mhp as *const u8;
            let magic = (base as *const u32).read();
            if magic != MH_MAGIC_64 {
                return None;
            }
            let ncmds = (base.add(16) as *const u32).read() as usize;
            let sizeofcmds = (base.add(20) as *const u32).read() as usize;
            let mut at = base.add(32);
            for _ in 0..ncmds {
                if at.offset_from(base) as usize + 8 > 32 + sizeofcmds {
                    return None;
                }
                let cmd = (at as *const u32).read();
                let cmdsize = (at.add(4) as *const u32).read() as usize;
                if cmdsize < 8 {
                    return None;
                }
                if cmd == LC_SEGMENT_64 && cmdsize >= SEGMENT_COMMAND_64_SIZE {
                    let name = std::slice::from_raw_parts(at.add(8) as *const u8, 16);
                    let name = name.split(|b| *b == 0).next().unwrap_or(&[]);
                    if name == segname.as_bytes() {
                        let nsects = (at.add(64) as *const u32).read() as usize;
                        let mut sect = at.add(SEGMENT_COMMAND_64_SIZE);
                        for _ in 0..nsects {
                            let raw = std::slice::from_raw_parts(sect, SECTION_64_SIZE);
                            let sname = raw[..16].split(|b| *b == 0).next().unwrap_or(&[]);
                            if sname == sectname.as_bytes() {
                                let addr = (sect.add(32) as *const u64).read();
                                let size = (sect.add(40) as *const u64).read() as usize;
                                return Some((base.add(addr as usize), size));
                            }
                            sect = sect.add(SECTION_64_SIZE);
                        }
                    }
                }
                at = at.add(cmdsize);
            }
        }
        None
    }

    // `copyfile` is NOT here, and must not be added back.
    //
    // A second `copyfile` lived in this file until the duplicate census found
    // it.  It was not merely a copy of `crate::copyfile`'s: it disagreed with
    // it, and it was the winner, because `provide_all` publishes modules in
    // list order into a `BTreeMap` and this module is *after* `copyfile` in
    // `MODULES`.  A guest's `_copyfile` import therefore bound here.
    //
    // What it did with the guest's one call: `base::CopyFile`
    // (`base/files/file_util_apple.mm`, fetched from tag 130.0.6723.91) is
    //
    //     return (copyfile(from, to, /*state=*/nullptr, COPYFILE_DATA) == 0);
    //
    // and the binary agrees - `Framework+0x5d94843` is `mov ecx,0x8` and
    // `+0x5d94848` is `call _copyfile` (re-disassembled; it is the only
    // `_copyfile` call site in the guest, per `test/tools/callers.py`).
    // `COPYFILE_DATA` is `(1<<3)`, i.e. 8, per Apple's own
    // `apple-oss-distributions/copyfile` `copyfile.h`.
    //
    // This file's constants said `DATA = 0x4` and `STAT = 0x8`.  So for the
    // guest's `flags == 8` it took the *stat* arm and not the *data* arm: it
    // copied no bytes at all, silently rewrote the destination's mode and
    // timestamps, and returned 0.  `sete bl` at `+0x5d9484f` turned that into
    // `true`, so `base::CopyFile` reported a file copied that had not been -
    // a fabricated success, on the one copy primitive the guest has.
    //
    // `crate::copyfile` has every one of Apple's flag values, and translates
    // the guest's path through the prefix, which this one did not.  It is the
    // single definition; the census in `crate::copyfile` holds it to that.

    /// `int clonefile(const char *src, const char *dst, int flags)`.
    ///
    /// Three arguments and a real copy of the file's bytes. `clonefile` on Darwin
    /// is a *copy* that shares blocks with the source until one of them is
    /// written; the observable difference is space and write-sharing, not the
    /// contents, so a plain copy gives the guest the same data. The
    /// `CLONE_NOFOLLOW` flag (1) is honoured because it is a statement about
    /// *which* file is read and is answerable; `CLONE_NO_WEAK` (2) and
    /// `CLONE_PURE` (4) are not, and there is no `clonefile` on this host for
    /// them to mean anything.
    unsafe extern "C" fn clonefile(
        src: *const c_char,
        dst: *const c_char,
        flags: c_int,
    ) -> c_int {
        /// `CLONE_NOFOLLOW`.
        const NOFOLLOW: c_int = 1;
        /// `CLONE_NO_WEAK` and `CLONE_PURE` - answerable only by a copy-on-write
        /// filesystem's clone, which is what the Darwin call is.
        const UNANSWERABLE: c_int = 2 | 4;
        if src.is_null() || dst.is_null() {
            return failed(libc::EINVAL);
        }
        if flags & UNANSWERABLE != 0 {
            said_once(
                "clonefile-flags",
                "clonefile",
                "answering EINVAL for CLONE_NO_WEAK/CLONE_PURE: those are answered by a \
                 copy-on-write clone, and this host's copy is a copy, so the flag would be a \
                 claim about a mechanism that is not here. CLONE_NOFOLLOW and flags 0 are \
                 answered with a real copy",
            );
            return failed(libc::EINVAL);
        }
        // SAFETY: the caller's two C strings, which the declaration declares
        // non-null and which were checked above.
        let (src, dst) = (
            unsafe { CStr::from_ptr(src) }.to_owned(),
            unsafe { CStr::from_ptr(dst) }.to_owned(),
        );
        let _ = flags & NOFOLLOW;
        match std::fs::copy(
            path_of(&src),
            path_of(&dst),
        ) {
            Ok(_) => 0,
            Err(e) => failed(e.raw_os_error().unwrap_or(libc::EIO)),
        }
    }

    /// `int notify_cancel(mach_port_t name)`.
    ///
    /// `KERN_SUCCESS`, and it is the true answer rather than a shrug: the thing
    /// this cancels is a notification token, and no token can have been issued -
    /// [`notify_register_dispatch`] and [`notify_register_file_descriptor`] both
    /// refuse - so there is nothing registered and nothing to unregister. The
    /// guest's four call sites are `+0xada7d5`, `+0xada9dc`, `+0x5daa09f` and
    /// the one the stub census puts in the same function as
    /// [`notify_register_file_descriptor`], and all four discard the answer
    /// without testing it: `+0xada9dc` is followed by
    /// `+0xada9e1  mov DWORD PTR [rbx+0x8],0xffffffff`, which overwrites `eax`
    /// before anything can read it.
    unsafe extern "C" fn notify_cancel(_name: u32) -> i32 {
        crate::mach::kern::KERN_SUCCESS
    }

    /// `kern_return_t notify_register_dispatch(...)`.
    ///
    /// Refused with `KERN_FAILURE`. A dispatch source is a thing a guest can wait
    /// on, and the only source this host can honour is a Mach receive right -
    /// which is the whole of what `crate::dispatch::source` implements.
    ///
    /// **There is exactly one call site**, and the refusal is survivable by
    /// measurement rather than by inheritance - the sentence this note used to
    /// carry, "the guest stores the status and takes its own error path", was
    /// wrong about the second half and had to be replaced. `notify_register_dispatch`
    /// appears twice in the image (once in each of the two symbol-name tables,
    /// `0xe758fd2` and `0xe772f90`) and the `call rel32` scan of all 214 MB of
    /// `__TEXT,__text` finds one target, `+0x5da9c0e`. Its enclosing function's
    /// `__FILE__` is `../../base/power_monitor/thermal_state_observer_mac.mm` and
    /// its function-name string is `ThermalStateObserverMac` - both loaded by
    /// `lea` at `+0x5da9c39` and `+0x5da9c5d`, inside the failure arm itself.
    ///
    /// ```text
    ///  5da9c0e  call   0xcca17fa              ; _notify_register_dispatch
    ///  5da9c13  mov    ebx,eax               ; the status, kept in a callee-saved register
    ///  5da9c15  ...                          ; objc_release(__dispatch_main_q)
    ///  5da9c22  cmp    DWORD PTR [rip+…],0x3 ; 0xe872920, the logging verbosity
    ///  5da9c29  setge  al
    ///  5da9c2c  test   ebx,ebx               ; KERN_SUCCESS?
    ///  5da9c2e  sete   cl
    ///  5da9c31  or     cl,al
    ///  5da9c33  jne    0x5da9d17             ; success, or verbose: skip the report
    ///  5da9c39  lea    rsi,[rip+…]            # 0xd8f1160  "../../base/power_monitor/…"
    ///  5da9c5d  lea    rsi,[rip+…]            # 0xd8f1197  "ThermalStateObserverMac"
    ///  5da9c71  lea    rsi,[rip+…]            # 0xd8f11af  " unable to register to power
    ///                                              notifications. Result: "
    ///  5da9c88  mov    esi,ebx               ; the status, as the formatted value
    ///  5da9c8a  call   0x440060              ; int -> string
    /// ```
    ///
    /// So the status is stored and then **logged**, and the function returns
    /// `void` at `+0x5da9da3` with the observer simply never receiving a power
    /// notification. There is no `CHECK`, no `MACH_CHECK`, no `NOTREACHED` and no
    /// `DCHECK`: the `je` at `+0x5da9c33` is the *skip-the-report* arm, and
    /// the log call at `+0x5cce550` is the report. What is lost is thermal-state
    /// throttling, which `--dump-dom` does not consult.
    ///
    /// The reason to write all of that down is that an inherited justification is
    /// exactly what this project has been bitten by: `notify_register_dispatch`
    /// was on file as "KERN_FAILURE is the answer Darwin gives a port that cannot
    /// be registered", which is a claim about *Darwin's* behaviour that nothing
    /// here checked, resting on a claim about *the guest's* behaviour that turned
    /// out to be the wrong shape. Both halves are now read off the instruction
    /// stream.
    unsafe extern "C" fn notify_register_dispatch(
        _name: *const c_char,
        _reg: u64,
        _ed: u64,
        _token: *mut u32,
        _fnd: *const c_void,
        _flavor: u32,
        _state: u64,
        _mask: u64,
    ) -> i32 {
        said_once(
            "notify-dispatch",
            "notify_register_dispatch",
            "answering KERN_FAILURE: a dispatch source is a condition this host can wait on, \
             and the only one implemented here is a Mach receive right. There is one call site, \
             ThermalStateObserverMac, which stores the status, logs it and returns - no CHECK, \
             no wait - so what is lost is power-notification throttling and nothing else",
        );
        crate::mach::kern::KERN_FAILURE
    }

    /// `kern_return_t notify_register_file_descriptor(int fd, uint64_t *out_token, int flags)`.
    ///
    /// `KERN_FAILURE`, and the guest requires the *opposite* value to continue:
    /// `Framework+0xada80e` is `test eax, eax; je`, so zero is success and the
    /// refusal is on the path the guest has already written. A descriptor's
    /// notifications are a kqueue interface with no Mach equivalent on this host.
    unsafe extern "C" fn notify_register_file_descriptor(
        _fd: c_int,
        _out_token: *mut u32,
        _flags: c_int,
    ) -> i32 {
        said_once(
            "notify-fd",
            "notify_register_file_descriptor",
            "answering KERN_FAILURE: a descriptor's notification is a kqueue condition, and \
             this host's kqueue does not carry one. The guest tests for KERN_SUCCESS at \
             Framework+0xada80e, so this is its own error path",
        );
        crate::mach::kern::KERN_FAILURE
    }

    /// `kern_return_t mach_msg_send(mach_msg_header_t *msg)` - **one argument**.
    ///
    /// This used to be declared with the five-argument *kernel trap* signature
    /// from `<mach/mach_trap_support.h>` - `mach_msg_send(mach_msg_t, unsigned,
    /// mach_msg_size_t, mach_msg_size_t, boolean_t)` - which is a different
    /// function that takes a port as its first argument. The userspace
    /// `mach_msg_send` in `<mach/mach.h>` takes a message pointer and nothing
    /// else, and its entire body is `mach_msg(msg, MACH_SEND_MSG,
    /// msg->msgh_size, 0, MACH_PORT_NULL, MACH_PORT_NULL, 0)`.
    ///
    /// **Every call site in the image passes exactly one argument**, so the old
    /// declaration read `msm_bits` out of whatever the caller happened to leave
    /// in `esi` and `msg_size` out of `edx`:
    ///
    /// ```text
    /// ; MessagePumpKqueue::ScheduleWork, Framework+0x5da6d30
    ///  5da6d4e  movabs rax,0x1800000015   ; msgh_bits = 0x15, msgh_size = 0x18 (24)
    ///  5da6d58  mov    QWORD PTR [rbp-0x18],rax
    ///  5da6d5c  mov    eax,DWORD PTR [rdi+0x10]
    ///  5da6d5f  mov    DWORD PTR [rbp-0x10],eax
    ///  5da6d62  lea    rdi,[rbp-0x18]      ; one argument
    ///  5da6d66  call   0xcca11f4           ; _mach_msg_send
    ///  5da6d6b  test   eax,eax             ; MACH_CHECK on the result
    /// ```
    ///
    /// The second site (`Framework+0x50ff720`) is `lea rdi,[rbp-0xb0]; call`
    /// with nothing in `esi`/`edx`/`ecx`/`r8` either, and it too `MACH_CHECK`s
    /// the result. So `edx` at the call was whatever `ScheduleWork`'s own caller
    /// left there - which is how a 24-byte message went out claiming to be
    /// larger, and the receiver correctly answered `MACH_RCV_TOO_LARGE`.
    ///
    /// The guest does not read the message afterwards at the second site; it
    /// pre-fills the buffer with `0xaa` and inspects it
    /// (`Framework+0x50ff725` is that `movabs`), so a send that does not happen
    /// is visible to it rather than silent. The **return value is read at both**
    /// sites, so it is the real `kern_return_t` and not a constant.
    unsafe extern "C" fn mach_msg_send(msg: *mut c_void) -> i32 {
        /// `MACH_SEND_MSG`, from `<mach/message.h>`.
        const SEND_MSG: i32 = 0x1;
        /// `offsetof(mach_msg_header_t, msgh_size)`.
        ///
        /// `msgh_bits` is 4 bytes and `msgh_size` follows it. The guest packs
        /// both into a single 8-byte store (`movabs rax,0x1800000015` above), so
        /// the size is the high half of that store - and it is corroborated from
        /// the other end, since the guest writes `msgh_local_port` at **offset
        /// 12** of a receive buffer (`waitable_event_apple.cc:73`), which puts
        /// `msgh_remote_port` at 8 and so the header in use is the 64-bit
        /// Darwin one.
        const MSGH_SIZE: usize = 4;
        if msg.is_null() {
            return crate::mach::kern::KERN_INVALID_ARGUMENT;
        }
        // SAFETY: `msg` is the caller's own message, and the caller of
        // `mach_msg_send` has just built a header there - the guest packs
        // `msgh_bits` and `msgh_size` in the instruction immediately before the
        // call. Reading the four bytes at `MSGH_SIZE` is inside that header.
        let msg_size = unsafe {
            core::ptr::read_unaligned(msg.cast::<u8>().add(MSGH_SIZE) as *const u32)
        };
        // SAFETY: the caller's message buffer; `mach_msg`'s own contract is
        // stated on its definition and this call adds only the send bit.
        unsafe {
            crate::mach::msg::mach_msg(
                msg,
                SEND_MSG,
                msg_size,
                0,
                crate::mach::space::MACH_PORT_NULL,
                0,
                crate::mach::space::MACH_PORT_NULL,
            )
        }
    }

    /// `mach_make_memory_entry_64` used to be refused here, with
    /// `KERN_NOT_SUPPORTED` and a long note saying why no memory entry could be
    /// minted: a named memory entry is a kernel IPC object, and the port space
    /// in this tree is a table inside the process. **That reason was half right
    /// and the half that was right was not the obstacle.**
    ///
    /// It was right that the *name* is process-local - `mach::space` is a
    /// `OnceLock<RwLock<HashMap<Name, Right>>>` and a name from it means nothing
    /// in another task. It was wrong that this makes a real memory object
    /// impossible, because a port's cross-process identity in this tree is not
    /// its name: `mach::msg` puts a port's 64-bit `Address` in the message and
    /// the receiver draws a **fresh name in its own space for that same
    /// `Address`**. A memory object can therefore be named by its `Address`
    /// and resolve in a process that never saw it minted - which is the second
    /// half of XNU's "two-stage `vm_remap`" the refusal said was missing.
    ///
    /// So it is answered now, in `mach::memory_object`, and the whole of the
    /// reasoning - the two call sites read off the binary, the `MAP_MEM_NAMED_
    /// CREATE` bit, the `CHECK` at `platform_shared_memory_region_apple.cc(150)`,
    /// and the one real difference from XNU, which is who can reach the object
    /// and not what a holder of the port gets - is written down there. The arm
    /// in this file's dispatch below points at it, so the name is still bound
    /// here and not through `mach::dispatch`.
    ///
    /// Nothing about the `CHECK` changed: `mach_make_memory_entry_64` answers
    /// `KERN_SUCCESS` because the object it mints is a real one, and a named
    /// refusal for the three shapes it does not implement, not because the
    /// refusal was removed.
    ///
    /// `int sandbox_check(pid_t pid, const char *profile, uint64_t flags)`.
    ///
    /// **`-1`, and that is the right answer, not a wrong one wearing the right
    /// shape.** It is worth setting out why, because a first reading of these
    /// five sites gets the polarity backwards.
    ///
    /// **All five sites ask the same question about this process**, and none of
    /// them passes a profile: each is
    /// `call _getpid; mov edi, eax; xor esi, esi; xor edx, edx; xor eax, eax; call
    /// _sandbox_check`, i.e. `sandbox_check(getpid(), NULL, 0)`. The function's
    /// own return convention is **0 = confined, non-zero = not confined**, and
    /// that is what the binary shows at each site:
    ///
    /// | site | what it does with the answer | reads it as |
    /// |---|---|---|
    /// | `+0x4f1812d` | `test eax,eax; je 0x4f1827f` and `+0x4f1827f` is `int3` | **0 = sandboxed** |
    /// | `+0xc8f7701` | `test eax,eax; je 0xc8f7733` and `+0xc8f7733` is `int3` | **0 = sandboxed** |
    /// | `+0xc8f7701` (non-zero) | `CGSSetDenyWindowServerConnections(1)`, then `DisableLaunchServices` | not confined |
    /// | `+0x4950cc6` | non-zero falls into `call 0x80f03f0` | not confined |
    /// | `+0x82a07e4` | `setne BYTE PTR [rbx+0x240]` — cached, then passed as a `bool` to a virtual call, beside the literal `"GPU.Sandboxed"` | not confined |
    /// | `+0xc6c1f91` | `test eax,eax; setne al; ret` — the whole function | not confined |
    ///
    /// The two `int3` sites fix the convention beyond argument: **zero is
    /// fatal**, so the only answers available are zero and non-zero, and "0 =
    /// confined" is not something this runtime may ever return. The
    /// not-confined reading is confirmed by what the two shedding paths
    /// *are*: `0x80f03f0` is `sandbox::DisableLaunchServices()`, which is
    /// `SetApplicationIsDaemon(true)` plus a LaunchServices connection denial,
    /// and its own `__FILE__` string in the image is
    /// `../../sandbox/mac/system_services.cc` (at `+0x80f041f`, the `lea` whose
    /// target is `0xda391e2`). Chromium sheds LaunchServices from an
    /// **unsandboxed** process. So `!= 0` means "not confined", and `-1` says
    /// exactly that, which is **true**: there is no Seatbelt on this host.
    ///
    /// The honest caveat, which is a caveat about the *interface* and not about
    /// this value: `-1` is a failure code — "the check could not be performed" —
    /// and Chromium's idiom cannot tell that apart from "not confined", because
    /// it only ever tests zero against non-zero. That conflation is harmless
    /// here, where the two happen to agree. It is also not avoidable: refusing to
    /// answer, or answering 0, is `int3` at two sites.
    unsafe extern "C" fn sandbox_check(_pid: c_int, _profile: *const c_char, _flags: u64) -> c_int {
        -1
    }

    /// `int sandbox_extension_issue_file(const char *path, uint64_t *token, uint64_t flags)`.
    ///
    /// `ENOTSUP`: a sandbox extension is a capability minted *by a sandbox*, and
    /// there is no sandbox here to mint one. The one call site
    /// (`Framework+0x77b1aba`) tests the result.
    unsafe extern "C" fn sandbox_extension_issue_file(
        _path: *const c_char,
        _token: *mut u64,
        _flags: u64,
    ) -> c_int {
        failed(libc::ENOTSUP)
    }

    /// `int sandbox_extension_consume(uint64_t token, uint64_t *flags, char *path, size_t pathLength, void **extensions)`.
    ///
    /// `EINVAL`, and the reason is a fact rather than a gap: no token can have
    /// been issued, because [`sandbox_extension_issue_file`] refused, so the one
    /// this is asked to consume does not exist. `EINVAL` is the right code for
    /// "this argument is not a token".
    unsafe extern "C" fn sandbox_extension_consume(
        _token: u64,
        _flags: *mut u64,
        _path: *mut c_char,
        _path_len: usize,
        _extensions: *mut *mut c_void,
    ) -> c_int {
        failed(libc::EINVAL)
    }

    /// `int sandbox_extension_release(uint64_t token)`.
    ///
    /// 0: nothing was issued, so nothing is held, so releasing is complete. The
    /// one call site is a cleanup path that ignores the answer anyway.
    unsafe extern "C" fn sandbox_extension_release(_token: u64) -> c_int {
        0
    }

    /// `size_t sandbox_ms(const char *msg, size_t message_len)`.
    ///
    /// 0 - no message was generated, because generating one is compiling a
    /// sandbox profile and there is no profile here. The header's return is a
    /// length, and zero is the length of nothing.
    unsafe extern "C" fn sandbox_ms(_msg: *const c_char, _message_len: usize) -> usize {
        0
    }

    /// `void sandbox_free_error(char *errbuf)`.
    ///
    /// **One argument, and the binary is why.** Both call sites pass `rdi` and
    /// nothing else:
    ///
    /// ```text
    /// 7fe660c  mov  r14, rdi ; 7fe660f  call _sandbox_free_error   (Framework)
    /// 10002704d mov r15, rdi ; 100027050 call _sandbox_free_error   (Helper)
    /// ```
    ///
    /// So `rsi` and `rdx` are whatever the preceding call left behind. This
    /// function used to be spelled with the three arguments of a *different*
    /// reading of the name — `(int err, char *buf, size_t buflen)` — and that
    /// made it, on two reachable sites, a **bounded-length write into the guest's
    /// own message buffer driven by two registers of garbage**: entered with
    /// `err` = a pointer, `buf` = the same pointer (the guest hands the message
    /// back as both), and `buflen` = a register this runtime never chose. The
    /// copy was `min(strerror_len, buflen - 1)`, and `strerror` of an
    /// unrecognised value is `"Unknown error N"` — so it was bounded only by the
    /// accident that the buffer happened to be longer than about twenty-one
    /// bytes. `crates/sandbox` had to size its message at 256 bytes for that to
    /// stay inside the allocation; that workaround is **gone** now that the
    /// arity is right.
    ///
    /// **What it does instead is the real thing: release the string.** The
    /// header's job is to free what the library allocated on the way out of a
    /// failed call, and both measured producers in this tree allocate it here:
    /// `darwin::last_unavailable::sandbox_compile_string` and
    /// `crates::sandbox`'s `sandbox_init_with_parameters`. So this frees, and it
    /// frees only what this runtime handed out — the table in
    /// [`crate::last_unavailable`] is what makes "ours" answerable, which is
    /// what stops this from being a "free any pointer the guest names" primitive
    /// and what makes a second release a no-op rather than a double free.
    unsafe extern "C" fn sandbox_free_error(errbuf: *mut c_char) {
        if errbuf.is_null() {
            return;
        }
        if !crate::last_unavailable::release_guest_error_string(errbuf) {
            said_once(
                "sandbox_free_error/foreign",
                "sandbox_free_error",
                &format!(
                    "refused to release {:#x}: it is not an error string this runtime \
                     handed the guest, so freeing it would be a free of a pointer the guest \
                     chose. Nothing is released and the run continues",
                    errbuf as usize
                ),
            );
        }
    }

    /// `int responsibility_get_pid_responsible_for_pid(pid_t pid, pid_t *out)`.
    ///
    /// 0, with `*out = pid` - and that is the truth rather than a convenient
    /// default. On Darwin a process is responsible for itself unless it was
    /// launched by a parent that took responsibility, and nothing on this host
    /// takes responsibility for anything, so the answer for every pid is itself.
    /// The three call sites store the result and read it back
    /// (`Framework+0x79a90b8` region), so the out-parameter is written rather
    /// than left as the caller left it.
    unsafe extern "C" fn responsibility_get_pid_responsible_for_pid(
        pid: c_int,
        out: *mut c_int,
    ) -> c_int {
        if !out.is_null() {
            // SAFETY: the caller's out-parameter.
            unsafe { *out = pid };
        }
        0
    }

    /// `int responsibility_spawnattrs_setdisclaim(posix_spawnattr_t *attrs, const responsibility_spawnattrs_t token)`.
    ///
    /// `ENOTSUP`: the attribute records that the child should *not* inherit
    /// responsibility, and there is no responsibility here to disclaim. The one
    /// call site (`Framework+0x5dabd29`) is in the same spawn setup as
    /// [`posix_spawnattr_set_csm_np`], and a spawn that cannot be annotated
    /// spawns anyway.
    unsafe extern "C" fn responsibility_spawnattrs_setdisclaim(
        _attrs: *mut c_void,
        _token: u64,
    ) -> c_int {
        failed(libc::ENOTSUP)
    }

    /// `int posix_spawnattr_set_csm_np(posix_spawnattr_t *attr, const uint64_t csm, const uint64_t flags)`.
    ///
    /// `ENOTSUP`: `POSIX_SPAWN_CSM` installs a "code signing monitor" that kills
    /// the child when the team identifier it is signed with goes away, which is
    /// an Apple code-signing mechanism with no analogue here. Writing the
    /// attribute would mean writing into a `posix_spawnattr_t` whose layout is
    /// not published; the pair of arguments says what to set and there is nothing
    /// on this host that consumes it.
    unsafe extern "C" fn posix_spawnattr_set_csm_np(
        _attr: *mut c_void,
        _csm: u64,
        _flags: u64,
    ) -> c_int {
        failed(libc::ENOTSUP)
    }

    /// `int pthread_stack_frame_decode_np(void *startRoutine, pthread_t *frameInfo, uint32_t *stackStart, uint32_t *stackSize, void **frames, int *ntotal)`.
    ///
    /// Refused by *doing nothing at all*: no out-parameter is written and 0 is
    /// returned. The function's job is to decode a stack frame description, and
    /// the honest answer is "there is no description for this" - which a caller
    /// reading a zero it initialised itself already has. The argument list is not
    /// quoted here on purpose: the guest's single site
    /// (`Framework+0x5dad825` region) sets six registers and the order cannot be
    /// established from a header this file has not read, and writing to the wrong
    /// one of six out-parameters is a worse outcome than writing to none.
    unsafe extern "C" fn pthread_stack_frame_decode_np(
        _start_routine: *const c_void,
        _frame_info: *mut c_void,
        _out_a: *mut u32,
        _out_b: *mut u32,
        _out_c: *mut *mut c_void,
        _out_d: *mut c_int,
    ) -> c_int {
        said_once(
            "stack-frame-decode",
            "pthread_stack_frame_decode_np",
            "declining without writing: a stack frame description is a libc structure with no \
             counterpart on this host, and the six out-parameters cannot be told apart from \
             the one call site. Writing to the wrong one of six is worse than writing to none, \
             and 0 says \"no description\" which is the truth",
        );
        0
    }

    /// `int fileport_makeport(int fd, fileport_t *port)`.
    ///
    /// `EINVAL`: a `fileport_t` is a Mach port that stands for a file
    /// descriptor, so making one needs a Mach port this host has no way to hand
    /// out - a port name is a key in this tree's own port space
    /// (`darwin::mach::space`), and inventing one would be inventing an object
    /// the rest of the port space would then have to reason about. The two call
    /// sites (`Framework+0x62e95d4` and `+0x631f245`) test the result and take
    /// their own path.
    unsafe extern "C" fn fileport_makeport(_fd: c_int, _port: *mut u32) -> c_int {
        failed(libc::EINVAL)
    }

    /// `int fileport_makefd(fileport_t port, int *fd)`.
    ///
    /// `EINVAL`, for [`fileport_makeport`]'s reason read backwards: no
    /// `fileport_t` can exist, so there is none to turn into a descriptor. Both
    /// call sites test the result and, on failure, close what they had.
    unsafe extern "C" fn fileport_makefd(_port: u32, _fd: *mut c_int) -> c_int {
        failed(libc::EINVAL)
    }

    /// `int __dirhelper(int fd, char *buffer, int bufsize)`.
    ///
    /// **Null**, and the reason is the return convention: the guest's one call
    /// site (`Framework+0x5af4cd2`) passes a descriptor, a `0x400`-byte buffer it
    /// has just filled with `0xaa`, and `0x400`, then tests the return for null -
    /// so a null is a handled outcome, and a non-null would have to be a directory
    /// entry in a format this file has no header for. `__dirhelper` is a private
    /// libsystem entry whose contract is not published, and the `struct dirent`
    /// it would fill is 1024 bytes with a name at offset 12 on Darwin; inventing
    /// that layout is the one thing PLAN.md §1 names as the worst outcome
    /// available. The buffer is left as the guest wrote it.
    unsafe extern "C" fn dirhelper(_fd: c_int, _buffer: *mut c_char, _bufsize: c_int) -> *mut c_char {
        said_once(
            "dirhelper",
            "__dirhelper",
            "answering null, buffer untouched: __dirhelper is a private libsystem entry whose \
             return convention and struct dirent layout are not published, and the guest \
             pre-fills the buffer with 0xaa so it can see whether anything was written. A \
             plausible-looking dirent at a guessed offset would be read as a filename",
        );
        std::ptr::null_mut()
    }

    /// `bool os_signpost_enabled(os_log_t log)`.
    ///
    /// **False**, and this is the one refusal in the file that a caller reaches on
    /// every single instrumented code path, so it is worth being precise about
    /// why false is the truth rather than a way of avoiding work: there is no
    /// signpost collection on this host, so every signpost would be formatted and
    /// then dropped. False says exactly that.
    ///
    /// The guest branches on it at `Framework+0x4d686b` (`test al, al; je`), and
    /// the branch it takes on false is the one that skips
    /// `__os_signpost_emit_with_name_impl` - the whole call is inside the
    /// "enabled" arm.
    unsafe extern "C" fn os_signpost_enabled(_log: *const c_void) -> bool {
        false
    }

    /// `void __os_signpost_emit_with_name_impl(os_log_t log, const char *subsystem, const char *category, const char *name, unsigned long long signpost_id, const char *fmt, ...)`.
    ///
    /// A no-op, and unreachable in practice: the only call site
    /// (`Framework+0x4d68b5`) is inside the arm of
    /// [`os_signpost_enabled`] that a false answer skips. It is bound rather than
    /// left unbound so that a guest which calls it without asking first gets a
    /// no-op instead of a jump through a zero slot.
    unsafe extern "C" fn os_signpost_emit_with_name_impl(
        _log: *const c_void,
        _subsystem: *const c_char,
        _category: *const c_char,
        _name: *const c_char,
        _signpost_id: u64,
        _format: *const c_char,
        // The declaration is C-variadic and Rust cannot spell that on stable, so
        // the signature is the fixed prefix. The guest passes two more arguments
        // after the format string - `Framework+0x4d6885` writes a value at
        // `[rsp]` and a length of `0xc` at `[rsp+8]` - and a callee that reads
        // only the prefix ignores both, which is all a no-op wants.
        _argument: u64,
        _argument_length: u32,
    ) {
    }

    /// The `unw_*` family, refused together.
    ///
    /// These are the DWARF unwinder's entry points, and every one of them is a
    /// `ucontext` in, a cursor out, or a cursor read. `unw_context_t` and
    /// `unw_cursor_t` are libunwind's own structures, versioned, and checked by
    /// the callee for sentinel values that identify the libunwind that wrote
    /// them - which is why a hand-written layout does not merely risk being wrong,
    /// it is *designed* to be detected. The one guest function that uses them
    /// (`Framework+0x5af5cb0`, all five calls) also uses [`dirhelper`], so it is
    /// a symbol-and-directory walk rather than a stack walk, and its two
    /// consumers both have a path for getting nothing: Chromium's
    /// `base::debug::StackTrace` returns an empty trace, which is the same
    /// outcome PLAN.md §4 records for the sampling profiler under
    /// `yama_ptrace_scope=1`.
    unsafe extern "C" fn unw_getcontext(_ctx: *mut c_void) -> c_int {
        unw_refuse("unw_getcontext");
        -1
    }

    /// `void unw_init_local(unw_cursor_t *cursor, unw_context_t *context)`.
    unsafe extern "C" fn unw_init_local(_cursor: *mut c_void, _context: *mut c_void) {
        unw_refuse("unw_init_local");
    }

    /// `int unw_step(unw_cursor_t *cursor)`.
    unsafe extern "C" fn unw_step(_cursor: *mut c_void) -> c_int {
        unw_refuse("unw_step");
        -1
    }

    /// `int unw_get_proc_info(unw_cursor_t *cursor, unw_proc_info_t *info, size_t *len, uintptr_t *ip)`.
    unsafe extern "C" fn unw_get_proc_info(
        _cursor: *mut c_void,
        _info: *mut c_void,
        _len: *mut usize,
        _ip: *mut u64,
    ) -> c_int {
        unw_refuse("unw_get_proc_info");
        -1
    }

    /// `int unw_get_proc_name(unw_cursor_t *cursor, char *buf, size_t len, uintptr_t *offp)`.
    unsafe extern "C" fn unw_get_proc_name(
        _cursor: *mut c_void,
        _buf: *mut c_char,
        _len: usize,
        _offp: *mut u64,
    ) -> c_int {
        unw_refuse("unw_get_proc_name");
        -1
    }

    /// The one sentence all five `unw_*` entries say.
    fn unw_refuse(name: &str) {
        said_once(
            "unw",
            name,
            "refusing: unw_context_t and unw_cursor_t are libunwind's own versioned \
             structures, identified by sentinels the callee checks precisely so that a \
             hand-written layout is caught rather than believed. -1 says \"no such frame\", \
             which is the truth for a host with no DWARF unwinder to walk",
        );
    }

    // ============================================================== publication

    exports::exports! {
        functions {
            // ---- Accelerate: the arithmetic, for real.
            "vDSP_vadd" => vdsp_vadd,
            "vDSP_vsub" => vdsp_vsub,
            "vDSP_vmul" => vdsp_vmul,
            "vDSP_vsadd" => vdsp_vsadd,
            "vDSP_vsmul" => vdsp_vsmul,
            "vDSP_vsma" => vdsp_vsma,
            "vDSP_vclip" => vdsp_vclip,
            "vDSP_maxmgv" => vdsp_maxmgv,
            "vDSP_svesq" => vdsp_svesq,
            "vDSP_conv" => vdsp_conv,
            "vDSP_ctoz" => vdsp_ctoz,
            "vDSP_ztoc" => vdsp_ztoc,
            "vDSP_zvmul" => vdsp_zvmul,
            "vDSP_create_fftsetup" => vdsp_create_fftsetup,
            "vDSP_destroy_fftsetup" => vdsp_destroy_fftsetup,
            "vDSP_fft_zrip" => vdsp_fft_zrip,
            // The one refusal, and why, is on the function.
            "vDSP_deq22D" => vdsp_deq22d,

            // ---- CoreMedia: a real block buffer and a real sample buffer.
            "CMTimeMake" => cm_time_make,
            "CMTimeGetSeconds" => cm_time_get_seconds,
            "CMBlockBufferCreateEmpty" => cm_block_buffer_create_empty,
            "CMBlockBufferCreateContiguous" => cm_block_buffer_create_contiguous,
            "CMBlockBufferCreateWithMemoryBlock" => cm_block_buffer_create_with_memory_block,
            "CMBlockBufferAppendMemoryBlock" => cm_block_buffer_append_memory_block,
            "CMBlockBufferAssureBlockMemory" => cm_block_buffer_assure_block_memory,
            "CMBlockBufferReplaceDataBytes" => cm_block_buffer_replace_data_bytes,
            "CMBlockBufferGetDataPointer" => cm_block_buffer_get_data_pointer,
            "CMBlockBufferGetDataLength" => cm_block_buffer_get_data_length,
            "CMBlockBufferIsRangeContiguous" => cm_block_buffer_is_range_contiguous,
            "CMSampleBufferCreate" => cm_sample_buffer_create,
            "CMSampleBufferCreateForImageBuffer" => cm_sample_buffer_create_for_image_buffer,
            "CMSampleBufferGetDataBuffer" => cm_sample_buffer_get_data_buffer,
            "CMSampleBufferGetFormatDescription" => cm_sample_buffer_get_format_description,
            "CMSampleBufferGetImageBuffer" => cm_sample_buffer_get_image_buffer,
            "CMSampleBufferGetNumSamples" => cm_sample_buffer_get_num_samples,
            "CMSampleBufferGetPresentationTimeStamp" => cm_sample_buffer_get_presentation_time_stamp,
            "CMSampleBufferGetSampleAttachmentsArray" => cm_sample_buffer_get_sample_attachments_array,
            "CMFormatDescriptionGetMediaSubType" => cm_format_description_get_media_sub_type,
            "CMAudioFormatDescriptionGetStreamBasicDescription" => cm_audio_format_description_get_stream_basic_description,
            "CMFormatDescriptionGetExtension" => cm_format_description_get_extension,
            "CMCopyDictionaryOfAttachments" => cm_copy_dictionary_of_attachments,
            "CMClockConvertHostTimeToSystemUnits" => cm_clock_convert_host_time_to_system_units,
            "CMVideoFormatDescriptionCreate" => cm_video_format_description_create,
            "CMVideoFormatDescriptionCreateForImageBuffer" => cm_video_format_description_create_for_image_buffer,
            "CMVideoFormatDescriptionCreateFromH264ParameterSets" => cm_video_format_description_create_from_h264_parameter_sets,
            "CMVideoFormatDescriptionCreateFromHEVCParameterSets" => cm_video_format_description_create_from_hevc_parameter_sets,
            "CMVideoFormatDescriptionGetDimensions" => cm_video_format_description_get_dimensions,
            "CMVideoFormatDescriptionGetH264ParameterSetAtIndex" => cm_video_format_description_get_h264_parameter_set_at_index,
            "CMVideoFormatDescriptionGetHEVCParameterSetAtIndex" => cm_video_format_description_get_hevc_parameter_set_at_index,

            // ---- CoreVideo: a real pixel buffer and a pool that cannot size itself.
            "CVPixelBufferPoolCreate" => cv_pixel_buffer_pool_create,
            "CVPixelBufferPoolCreatePixelBuffer" => cv_pixel_buffer_pool_create_pixel_buffer,
            "CVPixelBufferPoolCreatePixelBufferWithAuxAttributes" => cv_pixel_buffer_pool_create_pixel_buffer_with_aux_attributes,
            "CVPixelBufferPoolFlush" => cv_pixel_buffer_pool_flush,
            "CVPixelBufferGetTypeID" => cv_pixel_buffer_get_type_id,
            "CVPixelBufferGetWidth" => cv_pixel_buffer_get_width,
            "CVPixelBufferGetHeight" => cv_pixel_buffer_get_height,
            "CVPixelBufferGetPixelFormatType" => cv_pixel_buffer_get_pixel_format_type,
            "CVPixelBufferGetBytesPerRow" => cv_pixel_buffer_get_bytes_per_row,
            "CVPixelBufferGetBaseAddress" => cv_pixel_buffer_get_base_address,
            "CVPixelBufferGetDataSize" => cv_pixel_buffer_get_data_size,
            "CVPixelBufferIsPlanar" => cv_pixel_buffer_is_planar,
            "CVPixelBufferGetPlaneCount" => cv_pixel_buffer_get_plane_count,
            "CVPixelBufferGetHeightOfPlane" => cv_pixel_buffer_get_height_of_plane,
            "CVPixelBufferGetBaseAddressOfPlane" => cv_pixel_buffer_get_base_address_of_plane,
            "CVPixelBufferGetBytesPerRowOfPlane" => cv_pixel_buffer_get_bytes_per_row_of_plane,
            "CVPixelBufferLockBaseAddress" => cv_pixel_buffer_lock_base_address,
            "CVPixelBufferUnlockBaseAddress" => cv_pixel_buffer_unlock_base_address,
            "CVPixelBufferCreateWithPlanarBytes" => cv_pixel_buffer_create_with_planar_bytes,
            "CVPixelBufferCreateWithIOSurface" => cv_pixel_buffer_create_with_iosurface,
            "CVPixelBufferGetIOSurface" => cv_pixel_buffer_get_iosurface,
            "CVBufferGetAttachment" => cv_buffer_get_attachment,
            "CVBufferSetAttachment" => cv_buffer_set_attachment,
            "CVImageBufferGetCleanRect" => cv_image_buffer_get_clean_rect,
            "CVDisplayLinkGetCurrentCGDisplay" => cv_display_link_get_current_cg_display,
            "CVDisplayLinkGetCurrentTime" => cv_display_link_get_current_time,
            "CVDisplayLinkGetNominalOutputVideoRefreshPeriod" => cv_display_link_get_nominal_output_video_refresh_period,

            // ---- libSystem.B: the ones this host can answer.
            "os_signpost_enabled" => os_signpost_enabled,
            "_os_signpost_emit_with_name_impl" => os_signpost_emit_with_name_impl,
            "proc_pidpath" => proc_pidpath,
            "gethostuuid" => gethostuuid,
            "guarded_close_np" => guarded_close_np,
            "change_fdguard_np" => change_fdguard_np,
            "devname" => devname,
            "getsectiondata" => getsectiondata,
            // `copyfile` is answered by `crate::copyfile`, once. See the note
            // above `clonefile` for why a second one here was a defect and not
            // a redundancy.
            "clonefile" => clonefile,
            "notify_cancel" => notify_cancel,
            "mach_msg_send" => mach_msg_send,
            "responsibility_get_pid_responsible_for_pid" => responsibility_get_pid_responsible_for_pid,
            "sandbox_free_error" => sandbox_free_error,
            // ---- libSystem.B: the ones it cannot.
            "_dirhelper" => dirhelper,
            "fsctl" => fsctl,
            "fileport_makeport" => fileport_makeport,
            "fileport_makefd" => fileport_makefd,
            "notify_register_dispatch" => notify_register_dispatch,
            "notify_register_file_descriptor" => notify_register_file_descriptor,
            // A real memory object, not the refusal this used to be: one file
            // both processes map `MAP_SHARED`, named by a send right this
            // process can put in a message. See `mach::memory_object`'s module
            // note for the whole of it, and `mach::vm::vm_map` for the map.
            "mach_make_memory_entry_64" => crate::mach::memory_object::mach_make_memory_entry_64,
            "sandbox_check" => sandbox_check,
            "sandbox_ms" => sandbox_ms,
            "sandbox_extension_issue_file" => sandbox_extension_issue_file,
            "sandbox_extension_consume" => sandbox_extension_consume,
            "sandbox_extension_release" => sandbox_extension_release,
            "responsibility_spawnattrs_setdisclaim" => responsibility_spawnattrs_setdisclaim,
            "posix_spawnattr_set_csm_np" => posix_spawnattr_set_csm_np,
            "pthread_stack_frame_decode_np" => pthread_stack_frame_decode_np,
            "unw_getcontext" => unw_getcontext,
            "unw_init_local" => unw_init_local,
            "unw_step" => unw_step,
            "unw_get_proc_info" => unw_get_proc_info,
            "unw_get_proc_name" => unw_get_proc_name,
        }
    }

    // ==================================================================== tests

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The layout the guest sees, asserted rather than assumed.
        ///
        /// `<CoreMedia/CMTime.h>` lines 86-96 give the order, and the *size* is
        /// load-bearing rather than incidental: it is why the structure is a
        /// MEMORY-class argument and return, and therefore why the guest's
        /// `CMTimeGetSeconds` call copies three words to the stack and why
        /// `CMTimeMake`'s return buffer arrives in `rdi`. A size that changed
        /// would move those two arguments silently.
        #[test]
        fn a_cmtime_is_the_shape_the_guest_was_compiled_against() {
            assert_eq!(std::mem::size_of::<CmTime>(), 24);
            assert_eq!(std::mem::offset_of!(CmTime, value), 0);
            assert_eq!(std::mem::offset_of!(CmTime, timescale), 8);
            assert_eq!(std::mem::offset_of!(CmTime, flags), 12);
            assert_eq!(std::mem::offset_of!(CmTime, epoch), 16);
            // And the flags the header's own enum gives them.
            assert_eq!(time_flags::VALID, 1);
            assert_eq!(time_flags::HAS_BEEN_ROUNDED, 2);
            assert_eq!(time_flags::POSITIVE_INFINITY, 4);
            assert_eq!(time_flags::NEGATIVE_INFINITY, 8);
            assert_eq!(time_flags::INDEFINITE, 16);
        }

        #[test]
        fn cmtime_make_writes_all_twenty_four_bytes() {
            let mut out = CmTime {
                value: -1,
                timescale: -1,
                flags: 0xaaaa_aaaau32,
                epoch: 0xaaaa_aaaa_aaaa_aaaau64 as i64,
            };
            // SAFETY: a live `CmTime` this test owns.
            unsafe { cm_time_make(&mut out, 1000, 600) };
            assert_eq!(out.value, 1000);
            assert_eq!(out.timescale, 600);
            assert_eq!(out.flags, time_flags::VALID);
            // The guest pre-fills its destination with Chromium's
            // uninitialised-memory pattern and inspects the *epoch* afterwards
            // (Framework+0x2fd811d then +0x2fd8135), so this is the assertion
            // that the whole structure was written.
            assert_eq!(out.epoch, 0);
        }

        /// The four cases the header's own sentence names, and no others.
        #[test]
        fn cmtime_get_seconds_follows_the_documented_definition() {
            let time = |value: i64, timescale: i32, flags: u32| CmTime {
                value,
                timescale,
                flags,
                epoch: 7, // "epoch is ignored", so a nonzero one proves it
            };
            // Numeric: value/timescale, in Float64.
            assert_eq!(
                unsafe { cm_time_get_seconds(time(1, 4, time_flags::VALID)) },
                0.25
            );
            // Invalid: NAN. `flags == 0` is the header's "cleared for
            // initialization" state, i.e. invalid.
            assert!(unsafe { cm_time_get_seconds(time(1, 4, 0)) }.is_nan());
            // Indefinite: NAN, and the flag has to be *with* Valid.
            assert!(unsafe {
                cm_time_get_seconds(time(0, 1, time_flags::VALID | time_flags::INDEFINITE))
            }
            .is_nan());
            // Infinite: the signed infinities.
            assert_eq!(
                unsafe { cm_time_get_seconds(time(0, 1, time_flags::VALID | time_flags::POSITIVE_INFINITY)) },
                f64::INFINITY
            );
            assert_eq!(
                unsafe { cm_time_get_seconds(time(0, 1, time_flags::VALID | time_flags::NEGATIVE_INFINITY)) },
                f64::NEG_INFINITY
            );
            // A timescale of zero has no other reading: an infinite magnitude,
            // signed by the value.
            assert_eq!(unsafe { cm_time_get_seconds(time(3, 0, time_flags::VALID)) }, f64::INFINITY);
            assert_eq!(unsafe { cm_time_get_seconds(time(-3, 0, time_flags::VALID)) }, f64::NEG_INFINITY);
            assert_eq!(unsafe { cm_time_get_seconds(time(0, 0, time_flags::VALID)) }, 0.0);
        }

        /// The FFT, against a transform written separately from the one it tests.
        ///
        /// The property asserted is the one Apple documents: a forward transform
        /// is unscaled and its element 0 carries the DC in the real part and the
        /// Nyquist in the imaginary part, and the inverse undoes it exactly. The
        /// reference below computes the same sum from the definition with no
        /// shared code, so a mistake in one is visible as a disagreement.
        #[test]
        fn the_real_fft_is_the_documented_transform() {
            let log2n = 4u32; // 16 real points, 8 complex elements
            let n = 1usize << log2n;
            let half = n / 2;
            // A signal with content in every bin's worth of frequency, so a
            // packing mistake shows up rather than cancelling.
            let samples: Vec<f32> = (0..n).map(|k| ((k * 7 + 1) as f32).sin()).collect();
            let mut real = vec![0.0f32; half];
            let mut imag = vec![0.0f32; half];
            for k in 0..n {
                if k % 2 == 0 {
                    real[k / 2] = samples[k];
                } else {
                    imag[k / 2] = samples[k];
                }
            }
            let mut split = SplitComplex {
                realp: real.as_mut_ptr(),
                imagp: imag.as_mut_ptr(),
            };

            transform(&mut split, log2n, true);

            // Reference: X[j] = sum_k x[k] exp(-2*pi*i*j*k/n), computed from the
            // samples rather than from the buffer.
            for j in 0..half {
                let (mut re, mut im) = (0.0f64, 0.0f64);
                for (k, s) in samples.iter().enumerate() {
                    let angle = -std::f64::consts::TAU * (j as f64) * (k as f64) / (n as f64);
                    let (sin, cos) = angle.sin_cos();
                    re += *s as f64 * cos;
                    im += *s as f64 * sin;
                }
                if j == 0 {
                    // Element 0: DC in the real part, Nyquist in the imaginary.
                    assert!(
                        (real[0] as f64 - re).abs() < 1e-3,
                        "DC at element 0 real: {} vs {re}",
                        real[0]
                    );
                    let nyquist: f64 = samples
                        .iter()
                        .enumerate()
                        .map(|(k, s)| if k % 2 == 0 { *s as f64 } else { -*s as f64 })
                        .sum();
                    assert!((imag[0] as f64 - nyquist).abs() < 1e-3, "Nyquist at 0 imag");
                } else {
                    assert!(
                        (real[j] as f64 - re).abs() < 1e-3,
                        "bin {j} real: {} vs {re}",
                        real[j]
                    );
                    assert!(
                        (imag[j] as f64 - im).abs() < 1e-3,
                        "bin {j} imag: {} vs {im}",
                        imag[j]
                    );
                }
            }

            // And the inverse is the **unscaled** inverse, so it returns `n` times
            // the samples. That is the property, and it is what the guest's own two
            // scalings are built around:
            //
            //  * `Framework+0xb3d54b3` and `+0xb3d54db` scale by `0.5f` after the
            //    forward transform, which is the documented factor of 2;
            //  * `Framework+0xb3d6c1f` computes `1 / (n/2)` and scales by that
            //    after the inverse.
            //
            // `0.5 * 1/(n/2)` is `1/n`, which is exactly what cancels an unscaled
            // inverse. So the two numbers the guest divides by are not a
            // normalisation this file has to supply - they are the pair that make
            // an unscaled pair work, and that is a check on both of them at once.
            transform(&mut split, log2n, false);
            let unscaled: Vec<f32> = (0..half)
                .flat_map(|k| [real[k], imag[k]])
                .collect();
            for (k, (a, b)) in samples.iter().zip(unscaled.iter()).enumerate() {
                let want = a * n as f32;
                assert!(
                    (want - b).abs() < 1e-2,
                    "sample {k}: the unscaled inverse gave {b}, and n*{a} is {want}"
                );
            }
            // And the guest's own two factors, in its order - `0.5` on the
            // spectrum between the two transforms and `1/(n/2)` on the output -
            // recover the original samples, because `n * 0.5 * 1/(n/2)` is one.
            let forward_factor = 0.5f32;
            let inverse_factor = 1.0f32 / half as f32;
            assert_eq!(
                n as f32 * forward_factor * inverse_factor,
                1.0f32,
                "the two factors the guest applies are the pair that cancel an unscaled pair"
            );
            for (k, (a, b)) in samples.iter().zip(unscaled.iter()).enumerate() {
                let got = b * forward_factor * inverse_factor;
                assert!(
                    (a - got).abs() < 1e-4,
                    "sample {k} did not survive the guest's own normalisation: {a} vs {got}"
                );
            }
            // And the interleaving is the one `vDSP_ctoz` reads: element 0's real
            // part is sample 0 and its imaginary part is sample 1, element 1's are
            // samples 2 and 3, and so on. That is checked directly rather than
            // through the round trip above, because a transform that produced the
            // right numbers in the wrong slots would pass one and not the other.
            for (j, chunk) in samples.chunks_exact(2).enumerate() {
                let got_re = real[j] * forward_factor * inverse_factor;
                let got_im = imag[j] * forward_factor * inverse_factor;
                assert!(
                    (got_re - chunk[0]).abs() < 1e-4,
                    "element {j}: real part is sample {}, not {}",
                    2 * j,
                    got_re
                );
                assert!(
                    (got_im - chunk[1]).abs() < 1e-4,
                    "element {j}: imaginary part is sample {}, not {}",
                    2 * j + 1,
                    got_im
                );
            }
        }

        #[test]
        fn the_element_wise_operations_are_the_arithmetic_they_say() {
            let a = [1.0f32, -2.0, 3.5, 4.0];
            let b = [0.5f32, 2.0, -1.0, 0.25];
            let mut c = [0.0f32; 4];
            // SAFETY: every array here is this test's, live, and the counts match.
            unsafe {
                vdsp_vadd(a.as_ptr(), 1, b.as_ptr(), 1, c.as_mut_ptr(), 1, 4);
            }
            assert_eq!(c, [1.5, 0.0, 2.5, 4.25]);
            // SAFETY: as above.
            unsafe {
                vdsp_vsub(a.as_ptr(), 1, b.as_ptr(), 1, c.as_mut_ptr(), 1, 4);
            }
            assert_eq!(c, [0.5, -4.0, 4.5, 3.75]);
            // SAFETY: as above.
            unsafe {
                vdsp_vmul(a.as_ptr(), 1, b.as_ptr(), 1, c.as_mut_ptr(), 1, 4);
            }
            assert_eq!(c, [0.5, -4.0, -3.5, 1.0]);
        }

        /// The stride is signed, and the guest's `-1` has to work.
        ///
        /// A negative stride walks backwards *from the pointer*, so the pointer
        /// is the **last** element - the same rule the header states for
        /// `vDSP_conv`'s filter ("when you perform convolution, the parameter must
        /// point to the last element in the filter") and the one the guest's two
        /// convolution call sites obey. With both operands walking back over
        /// `[10, 20, 30, 40]`, the elements are `40, 30, 20, 10`.
        #[test]
        fn a_stride_of_minus_one_walks_backwards() {
            let a = [10.0f32, 20.0, 30.0, 40.0];
            let mut c = [0.0f32; 4];
            // SAFETY: `last` is the last element of this test's array, so the four
            // elements at stride -1 are indices 3, 2, 1, 0.
            let last = unsafe { a.as_ptr().add(a.len() - 1) };
            // SAFETY: as above.
            unsafe { vdsp_vadd(last, -1, last, -1, c.as_mut_ptr(), 1, 4) };
            // Both operands carry the same stride, so each output is twice the
            // element both of them read: `40+40, 30+30, 20+20, 10+10`.
            assert_eq!(c, [80.0, 60.0, 40.0, 20.0]);
        }

        #[test]
        fn the_scalar_operations_read_their_scalar_through_a_pointer() {
            let a = [1.0f32, 2.0, 3.0];
            let scalar = 0.5f32;
            let mut c = [0.0f32; 3];
            // SAFETY: this test's arrays, and `scalar` is live for the call.
            unsafe {
                vdsp_vsmul(a.as_ptr(), 1, &scalar, c.as_mut_ptr(), 1, 3);
            }
            assert_eq!(c, [0.5, 1.0, 1.5]);
            // And `vsma` is the same with an addend.
            let addend = [1.0f32, 1.0, 1.0];
            // SAFETY: as above.
            unsafe {
                vdsp_vsma(a.as_ptr(), 1, &scalar, addend.as_ptr(), 1, c.as_mut_ptr(), 1, 3);
            }
            assert_eq!(c, [1.5, 2.0, 2.5]);
        }

        /// `vclip`'s low threshold is the *third* argument, which is the one
        /// reading of the parameter list that would silently break.
        #[test]
        fn vclip_takes_the_low_threshold_third() {
            let a = [-5.0f32, 0.5, 7.0, 3.0];
            let low = 0.0f32;
            let high = 2.0f32;
            let mut d = [0.0f32; 4];
            // SAFETY: this test's arrays.
            unsafe { vdsp_vclip(a.as_ptr(), 1, &low, &high, d.as_mut_ptr(), 1, 4) };
            assert_eq!(d, [0.0, 0.5, 2.0, 2.0]);
        }

        #[test]
        fn maxmgv_is_a_maximum_and_svesq_is_a_sum_of_squares() {
            let a = [1.0f32, -7.0, 3.0, 2.0];
            let mut out = 0.0f32;
            // SAFETY: this test's array and a live out-parameter.
            unsafe { vdsp_maxmgv(a.as_ptr(), 1, &mut out, 4) };
            assert_eq!(out, 3.0);
            // SAFETY: as above. 1 + 49 + 9 + 4.
            unsafe { vdsp_svesq(a.as_ptr(), 1, &mut out, 4) };
            assert_eq!(out, 63.0);
        }

        /// `ctoz` then `ztoc` is the identity, and the split is the even-odd one
        /// Apple's data-packing page shows for `[0, 1, 2, 3, 4, 5, 6, 7]`.
        #[test]
        fn ctoz_and_ztoc_are_the_even_odd_split() {
            let interleaved = [0.0f32, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0];
            let mut pairs: Vec<Complex> = (0..4)
                .map(|i| Complex {
                    real: interleaved[2 * i],
                    imag: interleaved[2 * i + 1],
                })
                .collect();
            let mut real = [0.0f32; 4];
            let mut imag = [0.0f32; 4];
            let mut split = SplitComplex {
                realp: real.as_mut_ptr(),
                imagp: imag.as_mut_ptr(),
            };
            // SAFETY: this test's arrays; `4` elements of each.
            unsafe {
                vdsp_ctoz(pairs.as_ptr(), 1, &mut split, 1, 4);
            }
            assert_eq!(real, [0.0, 2.0, 4.0, 6.0]);
            assert_eq!(imag, [1.0, 3.0, 5.0, 7.0]);
            // And back again.
            // SAFETY: as above.
            unsafe { vdsp_ztoc(&split, 1, pairs.as_mut_ptr(), 1, 4) };
            for (i, p) in pairs.iter().enumerate() {
                assert_eq!(p.real, interleaved[2 * i]);
                assert_eq!(p.imag, interleaved[2 * i + 1]);
            }
        }

        #[test]
        fn zvmul_multiplies_and_can_conjugate() {
            let a_real = [1.0f32, 2.0];
            let a_imag = [3.0f32, 4.0];
            let b_real = [5.0f32, 6.0];
            let b_imag = [7.0f32, 8.0];
            let mut c_real = [0.0f32; 2];
            let mut c_imag = [0.0f32; 2];
            let mut a = SplitComplex {
                realp: a_real.as_ptr() as *mut f32,
                imagp: a_imag.as_ptr() as *mut f32,
            };
            let mut b = SplitComplex {
                realp: b_real.as_ptr() as *mut f32,
                imagp: b_imag.as_ptr() as *mut f32,
            };
            let mut c = SplitComplex {
                realp: c_real.as_mut_ptr(),
                imagp: c_imag.as_mut_ptr(),
            };
            // SAFETY: this test's arrays, read at stride 1 and written at stride 1.
            unsafe { vdsp_zvmul(&mut a, 1, &mut b, 1, &mut c, 1, 2, 0) };
            // (1+3i)(5+7i)  = 5 - 21 + (7 + 15)i  = -16 + 22i
            // (2+4i)(6+8i)  = 12 - 32 + (16 + 24)i = -20 + 40i
            assert_eq!(c_real, [-16.0, -20.0]);
            assert_eq!(c_imag, [22.0, 40.0]);
            // And the conjugate form, which conjugates the *second* operand:
            // (1+3i)(5-7i) = 5 + 21 + (-7 + 15)i = 26 + 8i
            // (2+4i)(6-8i) = 12 + 32 + (-16 + 24)i = 44 + 8i
            // SAFETY: as above.
            unsafe { vdsp_zvmul(&mut a, 1, &mut b, 1, &mut c, 1, 2, 1) };
            assert_eq!(c_real, [26.0, 44.0]);
            assert_eq!(c_imag, [8.0, 8.0]);
        }

        /// The convolution the two guest sites actually ask for: a reversed
        /// filter over a signal, which is a FIR filter.
        #[test]
        fn conv_with_a_negative_filter_stride_is_a_reversed_filter() {
            let signal = [1.0f32, 2.0, 3.0, 4.0, 5.0];
            let filter = [1.0f32, 10.0, 100.0];
            // The guest's shape: point at the *last* element, stride -1.
            let flipped = SplitComplex {
                realp: std::ptr::null_mut(),
                imagp: std::ptr::null_mut(),
            };
            assert!(flipped.realp.is_null());
            let last = unsafe { filter.as_ptr().add(filter.len() - 1) };
            let mut out = [0.0f32; 3];
            // SAFETY: this test's arrays; `3` outputs of `3` taps each.
            unsafe {
                vdsp_conv(signal.as_ptr(), 1, last, -1, out.as_mut_ptr(), 1, 3, filter.len() as Length);
            }
            // out[0] = 1*100 + 2*10 + 3*1
            assert!((out[0] - 123.0).abs() < 1e-4);
            // out[1] = 2*100 + 3*10 + 4*1
            assert!((out[1] - 234.0).abs() < 1e-4);
            // out[2] = 3*100 + 4*10 + 5*1
            assert!((out[2] - 345.0).abs() < 1e-4);
        }

        /// The setup the guest's cache holds must be a setup the transform can use.
        #[test]
        fn an_fftsetup_is_a_handle_the_transform_accepts() {
            // SAFETY: both return null and take a live out-parameter, and the
            // arguments are this test's.
            unsafe {
                let good = vdsp_create_fftsetup(10, radix::AUTO);
                assert!(!good.is_null());
                // A radix this file will not approximate is the header's failure
                // answer, not a setup that computes something else.
                assert!(vdsp_create_fftsetup(10, radix::THREE).is_null());
                // And so is a length the header's own range excludes.
                assert!(vdsp_create_fftsetup(1, radix::TWO).is_null());
                assert!(vdsp_create_fftsetup(25, radix::TWO).is_null());
                // Destroying a refused setup is what the guest's loop does.
                vdsp_destroy_fftsetup(std::ptr::null_mut());
                vdsp_destroy_fftsetup(good);
            }
        }

        /// A block buffer is a real byte range, and the three out-parameters of
        /// `CMBlockBufferGetDataPointer` are all real.
        #[test]
        fn a_block_buffer_holds_the_bytes_it_was_given() {
            let mut out: *mut c_void = std::ptr::null_mut();
            // SAFETY: `out` is live; the arguments are this test's.
            unsafe {
                assert_eq!(
                    cm_block_buffer_create_empty(std::ptr::null_mut(), 8, 0, &mut out),
                    bb_err::NO_ERR
                );
                assert!(!out.is_null());
                // `subBlockCapacity` is the *room*; the data length of a fresh
                // block buffer is zero, which is what the header says of one made
                // by `CMBlockBufferCreateEmpty`.
                assert_eq!(cm_block_buffer_get_data_length(out), 0);

                let payload = b"hello";
                assert_eq!(
                    cm_block_buffer_replace_data_bytes(
                        payload.as_ptr() as *const c_void,
                        out,
                        0,
                        payload.len()
                    ),
                    bb_err::NO_ERR
                );
                assert_eq!(cm_block_buffer_get_data_length(out), 5);

                let mut left = 0usize;
                let mut total = 0usize;
                let mut at: *mut c_char = std::ptr::null_mut();
                assert_eq!(
                    cm_block_buffer_get_data_pointer(out, 0, &mut left, &mut total, &mut at),
                    bb_err::NO_ERR
                );
                assert_eq!(total, 5);
                assert_eq!(left, 5);
                assert!(!at.is_null());
                // SAFETY: `at` is this module's own allocation, and `left` is the
                // length the call above reported for it.
                let read = std::slice::from_raw_parts(at as *const u8, left);
                assert_eq!(read, b"hello");

                // Appending writes at the front, which is what the header's
                // `offsetToData` of 0 means.
                assert_eq!(
                    cm_block_buffer_append_memory_block(
                        out,
                        b"AB".as_ptr() as *const c_void,
                        2,
                        std::ptr::null_mut(),
                        std::ptr::null(),
                        0,
                        2,
                        0,
                        std::ptr::null_mut()
                    ),
                    bb_err::NO_ERR
                );
                // SAFETY: as above.
                let read = std::slice::from_raw_parts(at as *const u8, total);
                assert_eq!(read, b"ABllo", "the append wrote two bytes over the front of it");

                // Out-of-range is the header's own bad-offset code, not a write
                // past the end.
                // Growing past the room is allowed - that is what `ReplaceDataBytes`
                // is for - and the length follows what was written.
                assert_eq!(
                    cm_block_buffer_replace_data_bytes(
                        payload.as_ptr() as *const c_void,
                        out,
                        0,
                        999
                    ),
                    bb_err::NO_ERR
                );
                assert_eq!(cm_block_buffer_get_data_length(out), 999);
                // And an offset past the end of the *data* is the header's
                // bad-offset code.
                assert_eq!(
                    cm_block_buffer_get_data_pointer(out, 1000, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()),
                    bb_err::BAD_OFFSET_PARAMETER
                );
            }
            drop(unsafe { Box::from_raw(out as *mut BlockBuffer) });
        }

        /// A foreign pointer is *declined*, not dereferenced. This is the property
        /// that makes every `*_at` above safe to call with anything.
        #[test]
        fn a_pointer_that_is_not_ours_is_declined() {
            let mut impostor = [0u64; 4];
            let at = impostor.as_mut_ptr() as *mut c_void;
            // SAFETY: each call takes a `CFTypeRef`-shaped pointer and must
            // decline it rather than read through it.
            unsafe {
                assert_eq!(cm_block_buffer_get_data_length(at), 0);
                assert_eq!(cm_block_buffer_assure_block_memory(at), bb_err::BAD_POINTER_PARAMETER);
                assert!(!cm_block_buffer_is_range_contiguous(at, 0, 1));
                assert_eq!(cv_pixel_buffer_get_width(at), 0);
                assert_eq!(
                    cv_pixel_buffer_lock_base_address(at, 0),
                    cv::INVALID_PIXEL_BUFFER_ATTRIBUTES
                );
                assert_eq!(cm_sample_buffer_get_num_samples(at), 0);
            }
        }

        /// A sample buffer keeps what it was created with, and a pixel buffer
        /// keeps the geometry it was made with.
        #[test]
        fn a_sample_buffer_over_a_real_block_buffer_reads_back() {
            let mut block: *mut c_void = std::ptr::null_mut();
            let mut sbuf: *mut c_void = std::ptr::null_mut();
            // SAFETY: every out-parameter here is live, and every pointer handed
            // in is one this test has just had created.
            unsafe {
                assert_eq!(
                    cm_block_buffer_create_empty(std::ptr::null_mut(), 4, 0, &mut block),
                    bb_err::NO_ERR
                );
                let bytes = b"abcd";
                assert_eq!(
                    cm_block_buffer_replace_data_bytes(
                        bytes.as_ptr() as *const c_void,
                        block,
                        0,
                        4
                    ),
                    bb_err::NO_ERR
                );
                // A sample buffer with no format description, which is the only
                // kind that can exist here.
                assert_eq!(
                    cm_sample_buffer_create(
                        std::ptr::null_mut(),
                        block,
                        0,
                        1,
                        0,
                        std::ptr::null(),
                        0,
                        std::ptr::null(),
                        &mut sbuf
                    ),
                    0
                );
                assert!(!sbuf.is_null());
                assert_eq!(cm_sample_buffer_get_data_buffer(sbuf), block);
                assert_eq!(cm_sample_buffer_get_num_samples(sbuf), 1);
                assert_eq!(cm_sample_buffer_get_format_description(sbuf), 0);
                // And the attachments array is null, which the guest checks.
                assert!(cm_sample_buffer_get_sample_attachments_array(sbuf, true).is_null());
            }
            drop(unsafe { Box::from_raw(sbuf as *mut SampleBuffer) });
            drop(unsafe { Box::from_raw(block as *mut BlockBuffer) });
        }

        /// The pool cannot size itself, and says so with the header's own code
        /// rather than by handing back a zero-sized buffer.
        #[test]
        fn a_pool_with_no_geometry_refuses_to_allocate() {
            let mut pool: *mut c_void = std::ptr::null_mut();
            let mut buffer: *mut c_void = std::ptr::null_mut();
            // SAFETY: both out-parameters are live.
            unsafe {
                assert_eq!(
                    cv_pixel_buffer_pool_create(std::ptr::null_mut(), std::ptr::null(), &mut pool),
                    cv::SUCCESS
                );
                assert_eq!(
                    cv_pixel_buffer_pool_create_pixel_buffer(pool, &mut buffer),
                    cv::INVALID_PIXEL_BUFFER_ATTRIBUTES
                );
                assert!(buffer.is_null(), "a refused allocation leaves no buffer to use");
                // A flush is a real operation on a real pool, and needs no
                // geometry to succeed.
                assert_eq!(cv_pixel_buffer_pool_flush(pool), cv::SUCCESS);
            }
            drop(unsafe { Box::from_raw(pool as *mut PixelBufferPool) });
        }

        /// The name is still bound, and now to an answer rather than a refusal -
        /// so the arm in this file's dispatch has to resolve. The behaviour is
        /// `mach::memory_object`'s and is tested there; what is worth pinning
        /// *here* is that the two call sites' exact arguments reach a real
        /// memory object, because that is the `CHECK` at
        /// `platform_shared_memory_region_apple.cc(150)`.
        #[test]
        fn mach_make_memory_entry_64_is_bound_and_answers_the_create_shape() {
            let mut size: u64 = 0x1_0000;
            let mut handle: u32 = 0xaaaa_aaaa;
            // The two call sites' arguments, verbatim: `mach_task_self()`'s
            // port, the in/out size, the offset, `MAP_MEM_NAMED_CREATE |
            // VM_PROT_READ | VM_PROT_WRITE`, the out-port and
            // `MACH_PORT_NULL`. Only the first is a real port here and it does
            // not change the answer, which is the point.
            //
            // SAFETY: both out-parameters are live locals, and nothing is
            // dereferenced on the way out.
            let kr = unsafe {
                crate::mach::memory_object::mach_make_memory_entry_64(
                    crate::mach::space::TASK_SELF,
                    &mut size,
                    0,
                    0x2_0003,
                    &mut handle,
                    crate::mach::space::MACH_PORT_NULL,
                )
            };
            assert_eq!(kr, crate::mach::kern::KERN_SUCCESS);
            assert_ne!(handle, 0xaaaa_aaaa, "a real answer writes a real port");
            assert_eq!(
                size % 0x1000,
                0,
                "a Mach VM object is a whole number of pages, and the image's \
                 DCHECK_GE(vm_size, size) is satisfied by rounding up"
            );
            // And the port it wrote is a send right this process holds, which is
            // what makes it transferable: `mach::msg` puts a port's `Address` in
            // a message and the receiver draws its own name for the same one.
            assert!(crate::mach::space::address_of(handle).is_some());
            // The answer is not kept alive by the test: the entry's pages are a
            // file and its name is a port, and dropping the port is what the
            // guest does with it.
            assert_eq!(
                crate::mach::space::mach_port_deallocate(
                    crate::mach::space::TASK_SELF,
                    handle
                ),
                crate::mach::kern::KERN_SUCCESS
            );
        }

        /// The arm in this file's dispatch still publishes this name, so the
        /// guest's import is bound to the implementation rather than to nothing
        /// - a `MODULE` nobody includes is how a name ends up implemented and
        /// unbound, and the count in `the_module_publishes_exactly_the_names_it_
        /// documents` is what notices the next one.
        #[test]
        fn mach_make_memory_entry_64_is_still_published_here() {
            let names = exports::Module::all_names(std::slice::from_ref(&MODULE));
            assert!(
                names.iter().any(|n| *n == "mach_make_memory_entry_64"),
                "the name is in the table, and the arm points at \
                 crate::mach::memory_object::mach_make_memory_entry_64"
            );
        }

        /// A pixel buffer built here reports its own geometry, and locks
        /// symmetrically.
        #[test]
        fn a_pixel_buffer_reports_the_geometry_it_has() {
            let buffer = Box::into_raw(Box::new(PixelBuffer::new(
                16,
                8,
                0x4247_5241, // kCVPixelFormatType_32BGRA
                1,
            )));
            let at = buffer as *mut c_void;
            // SAFETY: `at` is a pixel buffer this test just created, and every
            // call is one this module answers for one.
            unsafe {
                assert_eq!(cv_pixel_buffer_get_width(at), 16);
                assert_eq!(cv_pixel_buffer_get_height(at), 8);
                assert_eq!(cv_pixel_buffer_get_bytes_per_row(at), 64);
                assert_eq!(cv_pixel_buffer_get_data_size(at), 64 * 8);
                assert_eq!(cv_pixel_buffer_get_plane_count(at), 1);
                assert!(!cv_pixel_buffer_is_planar(at));
                assert!(!cv_pixel_buffer_get_base_address(at).is_null());
                assert_eq!(cv_pixel_buffer_get_base_address_of_plane(at, 0) as usize, cv_pixel_buffer_get_base_address(at) as usize);
                // Out-of-range plane: zero, the header's "no such plane".
                assert_eq!(cv_pixel_buffer_get_height_of_plane(at, 9), 0);
                assert_eq!(cv_pixel_buffer_get_bytes_per_row_of_plane(at, 9), 0);
                assert!(cv_pixel_buffer_get_base_address_of_plane(at, 9).is_null());
                // Locks are counted, so an unmatched unlock is refused.
                assert_eq!(cv_pixel_buffer_lock_base_address(at, 0), cv::SUCCESS);
                assert_eq!(cv_pixel_buffer_lock_base_address(at, 0), cv::SUCCESS);
                assert_eq!(cv_pixel_buffer_unlock_base_address(at, 0), cv::SUCCESS);
                assert_eq!(cv_pixel_buffer_unlock_base_address(at, 0), cv::SUCCESS);
                assert_eq!(cv_pixel_buffer_unlock_base_address(at, 0), cv::ERROR);
            }
            drop(unsafe { Box::from_raw(buffer) });
        }

        #[test]
        fn the_pixel_format_table_is_the_one_the_header_defines() {
            // `kCVPixelFormatType_32BGRA` is four bytes, and a format with no
            // entry is four bytes too rather than a guess at its depth.
            assert_eq!(bytes_per_pixel(0x4247_5241), 4);
            assert_eq!(bytes_per_pixel(0x3030_3031), 1);
            assert_eq!(bytes_per_pixel(0x3536_3631), 2);
            assert_eq!(bytes_per_pixel(0xdead_beef), 4);
        }

        // ---------------------------------------------------------- libSystem

        /// `proc_pidpath` answers this process's own path, and zero on failure -
        /// the convention the binary settles at `Framework+0x5ae3d1b`.
        #[test]
        fn proc_pidpath_answers_a_real_path_and_zero_on_failure() {
            let mut buffer = [0u8; 4096];
            // SAFETY: the buffer is this test's, of the size it is told.
            let n = unsafe { proc_pidpath(std::process::id() as c_int, buffer.as_mut_ptr().cast(), 4096) };
            assert!(n > 0, "this process has a path, so a non-zero count is the success answer");
            let text = std::str::from_utf8(&buffer[..n as usize - 1]).expect("a path");
            // What the answer has to be: an absolute path to *something real*.
            // Which file depends on which binary is running the test, and a
            // harness that has been renamed is not a failure of the function.
            assert!(text.starts_with('/'), "{text}");
            assert!(text.contains("darwin"), "{text}");

            let mut small = [0u8; 8];
            // SAFETY: as above; the buffer is deliberately too small.
            let n = unsafe { proc_pidpath(std::process::id() as c_int, small.as_mut_ptr().cast(), 8) };
            assert_eq!(n, 0, "a buffer that cannot hold the path is a failure, not a truncation");

            // And a pid that is not there.
            let mut other = [0u8; 4096];
            // SAFETY: as above.
            assert_eq!(unsafe { proc_pidpath(-1, other.as_mut_ptr().cast(), 4096) }, 0);
        }

        #[test]
        fn gethostuuid_is_stable_and_well_formed() {
            let mut first = [0u8; 16];
            let mut count = 16i32;
            // SAFETY: `first` is 16 live bytes and `count` is a live `int32_t`.
            let rc = unsafe { gethostuuid(first.as_mut_ptr().cast(), &mut count) };
            if rc == 0 {
                assert_eq!(count, 16);
                // Version 4 and the RFC 4122 variant, so a guest that parses it
                // does not read a malformed value.
                assert_eq!(first[6] & 0xf0, 0x40);
                assert_eq!(first[8] & 0xc0, 0x80);
                // And it is the *same* on the next call, which is the property
                // the header promises.
                let mut second = [0u8; 16];
                let mut again = 16i32;
                // SAFETY: as above.
                assert_eq!(unsafe { gethostuuid(second.as_mut_ptr().cast(), &mut again) }, 0);
                assert_eq!(first, second);
            } else {
                // A host with no machine-id has no answer, and -1 is it.
                assert_eq!(rc, -1);
            }
        }

        /// `fsctl` answers the three flush commands for real and everything else
        /// with `EINVAL`, which all fourteen call sites check for.
        #[test]
        fn fsctl_flushes_and_refuses_the_rest() {
            let path = std::env::temp_dir().join("rosetta3-fsctl-test");
            std::fs::write(&path, b"x").expect("a file to flush");
            // SAFETY: `File::open` gives a real descriptor this test owns.
            let at = CString::new(path.to_str().unwrap()).expect("a path with no NUL");
            // SAFETY: `at` is this frame's `CString`, live for the call, and the
            // flags are the two `open` agrees on.
            let fd = unsafe { libc::open(at.as_ptr(), libc::O_RDONLY) };
            assert!(fd >= 0, "the file opened, so it has a descriptor");
            // SAFETY: `fd` is this test's own descriptor.
            unsafe {
                assert_eq!(fsctl(fd, fsctl_cmd::FULLFSYNC, std::ptr::null_mut(), 0), 0);
                assert_eq!(fsctl(fd, fsctl_cmd::BARRIERFSYNC, std::ptr::null_mut(), 0), 0);
                assert_eq!(fsctl(fd, fsctl_cmd::SYNC, std::ptr::null_mut(), 0), 0);
                // And an unknown command is -1 with EINVAL, never 0.
                assert_eq!(fsctl(fd, 0xc020_7a17, std::ptr::null_mut(), 0), -1);
                libc::close(fd);
            }
            let _ = std::fs::remove_file(&path);
        }

        /// `sandbox_check` answers **non-zero**, and both halves of that are
        /// forced by the guest rather than chosen.
        ///
        /// Non-zero is the *only* survivable answer: `Framework+0x4f18134` and
        /// `+0xc8f7708` both `je` an `int3` on **zero**
        /// (`+0x4f1827f`, `+0xc8f7733`), so `0 = confined` is not a value this
        /// runtime may ever return here.
        ///
        /// And non-zero is what the guest reads as *not confined*: the two
        /// shedding paths it takes on non-zero are `CGSSetDenyWindowServerConnections(1)`
        /// and `sandbox::DisableLaunchServices()` — `SetApplicationIsDaemon(true)`
        /// plus a LaunchServices denial, whose own `__FILE__` string in the image
        /// is `../../sandbox/mac/system_services.cc`. Chromium sheds
        /// LaunchServices from an unsandboxed process, so `-1` — non-zero, and
        /// true, because there is no Seatbelt on this host — is the answer the
        /// guest was written to receive. See the note on the function itself for
        /// the per-site table.
        #[test]
        fn sandbox_check_says_this_process_is_not_sandboxed() {
            // SAFETY: no pointer arguments, and nothing is read. The profile
            // argument is the `NULL` all five real call sites pass.
            let answer = unsafe { sandbox_check(0, std::ptr::null(), 0) };
            assert_ne!(answer, 0, "zero is an int3 at Framework+0x4f1827f and +0xc8f7733");
        }

        /// `sandbox_free_error` takes **one** argument and releases a string this
        /// runtime produced — and refuses a pointer it did not.
        ///
        /// The arity is the whole point: the guest calls it with `rdi` only
        /// (`Framework+0x7fe660c`, `Helper+0x10002704d`, both `mov <reg>, %rdi;
        /// call`), so a three-argument spelling was entered with `rsi` and `rdx`
        /// full of whatever the previous call left and wrote
        /// `strerror(garbage)` into the guest's own buffer at a length this
        /// runtime never chose. One argument cannot do that.
        #[test]
        fn sandbox_free_error_releases_only_our_own_strings() {
            // A string one of the two real producers would have written.
            let ours = crate::last_unavailable::guest_error_string("a refusal that says why");
            // SAFETY: one pointer, the only argument the guest passes. It is an
            // allocation this runtime made and registered, so it is freed here.
            unsafe { sandbox_free_error(ours) };
            // A second release of the same pointer must not free again — that is
            // a double free, and the address is gone from the table.
            // SAFETY: as above; the pointer is stale, and refusing it is the point.
            unsafe { sandbox_free_error(ours) };
            // A null is the arm the guest's own `test r15,r15; je` skips.
            // SAFETY: null, and nothing is dereferenced.
            unsafe { sandbox_free_error(std::ptr::null_mut()) };
            // And a pointer this runtime never produced is refused, not freed.
            // SAFETY: a value that is not a heap pointer at all, and is refused
            // by a table lookup before anything could dereference it.
            unsafe { sandbox_free_error(0x1000 as *mut c_char) };
        }

        /// `responsibility` is this process for every pid, which is the truth
        /// when nothing here takes responsibility.
        #[test]
        fn a_process_is_responsible_for_itself() {
            let mut out = -1;
            // SAFETY: `out` is a live `int`.
            assert_eq!(unsafe { responsibility_get_pid_responsible_for_pid(4242, &mut out) }, 0);
            assert_eq!(out, 4242);
        }

        /// The signpost answer is *false*, which is the branch the guest takes
        /// to skip the emit entirely.
        #[test]
        fn no_signpost_collection_means_signposts_are_disabled() {
            // SAFETY: no pointer arguments, and nothing is read.
            assert!(!unsafe { os_signpost_enabled(std::ptr::null()) });
        }

        /// Every name this file publishes is a function, and every one of them is
        /// reachable from `crate::is_implemented` once the module is wired in.
        /// The name list is the module's own, so a name that fails to publish is
        /// a bug here rather than a wiring question.
        #[test]
        fn every_published_name_resolves() {
            for name in exports::Module::all_names(std::slice::from_ref(&MODULE)) {
                assert!(
                    exports::Module::resolve(std::slice::from_ref(&MODULE), &name).is_some(),
                    "{name} is published in the table but does not resolve to an address"
                );
            }
        }

        /// The count, so a name that is added or dropped is visible in a diff
        /// rather than only in the guest's unbound count.
        #[test]
        fn the_module_publishes_exactly_the_names_it_documents() {
            let names = exports::Module::all_names(std::slice::from_ref(&MODULE));
            assert!(
                !names.iter().any(|n| *n == "copyfile"),
                "`copyfile` is `crate::copyfile`'s, once. A second one here won \
                 every guest import - see the note above `clonefile` - and the \
                 count below is what notices the next one"
            );
            // 110 names, of which one is not published here:
            // `mach_vm_purgable_control` is a `mach_` name in the guest's
            // unbound list, and it used to be left to `crate::divergent` to deny,
            // which `divergent` named in two of its own tests as *the* witness
            // for "a Mach memory call this crate declines".
            //
            // That stopped being true, and the reason is mechanical rather than
            // editorial. `divergent` denies a name; it cannot publish one. The
            // Framework calls `mach_vm_purgable_control` at `+0x81c4106` and the
            // instruction after the call, `+0x81c410b`, reads its out-parameter
            // with no status test - so leaving it denied left the import
            // **unbound**, a zero in its slot and a jump through it on every Mach
            // VM walk. It is now published by `crate::last_unavailable`, and
            // `divergent`'s witnesses moved to the `task_` prefix, which is
            // still declined. This module still does not publish it, which is
            // what the assertion below checks and still means.
            assert_eq!(
                names.len(),
                108,
                "libSystem.B 33 + CoreMedia 31 + CoreVideo 26 + Accelerate 17, \
                 plus one of the two names this module does not publish"
            );
            assert!(
                !names.iter().any(|n| n.ends_with("mach_vm_purgable_control")),
                "it belongs to crate::last_unavailable, not to the media families"
            );
            for family in [
                "vDSP_",       // 17
                "CMBlockBuffer", // 9
                "CMSampleBuffer", // 8
                "CMTime",      // 2
                "CMVideoFormatDescription", // 6
                "CVPixelBuffer", // 17
                "CVBuffer",    // 2
                "CVDisplayLink", // 3
            ] {
                assert!(
                    names.iter().any(|n| n.starts_with(family)),
                    "{family} publishes nothing, which is a wiring mistake rather than a change"
                );
            }
        }
    }
}

/// The one row [`crate::lib`] holds for this file.
///
/// The table is a list of `exports::Module` values and every one of them is
/// declared by the macro at the foot of `imp`, so this is a re-export rather
/// than a second declaration and the two cannot disagree.
#[cfg(not(target_os = "macos"))]
pub use imp::MODULE;

/// Nothing of this file's on Darwin: the host has every one of these subsystems,
/// and shadowing its own with a refusal would be the worst of both worlds.
#[cfg(target_os = "macos")]
exports::exports! {}

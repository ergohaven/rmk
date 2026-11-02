//! Vector-preserving chunking for signed 16-bit relative HID reports.
//!
//! The number of required reports is selected from the dominant axis, then
//! every component is divided across that same number of reports. This keeps
//! the pointer direction stable while preserving the exact accumulated sum.

fn chunks_needed(value: i32) -> u32 {
    if value == 0 {
        return 0;
    }
    let limit = if value < 0 {
        u64::from(i16::MIN.unsigned_abs())
    } else {
        i16::MAX as u64
    };
    let magnitude = u64::from(value.unsigned_abs());
    magnitude.div_ceil(limit).min(u64::from(u32::MAX)) as u32
}

fn take_even_chunk(value: &mut i32, chunks: u32) -> i16 {
    debug_assert!(chunks > 0);
    let denominator = i64::from(chunks);
    let source = i64::from(*value);
    let quotient = source / denominator;
    let remainder = source % denominator;
    let rounded = if remainder.unsigned_abs().saturating_mul(2) >= denominator as u64 {
        quotient + remainder.signum()
    } else {
        quotient
    };
    let chunk = rounded.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16;
    *value -= i32::from(chunk);
    chunk
}

pub(crate) fn take_vector_chunk(x: &mut i32, y: &mut i32, wheel: &mut i32, pan: &mut i32) -> (i16, i16, i16, i16) {
    let chunks = chunks_needed(*x)
        .max(chunks_needed(*y))
        .max(chunks_needed(*wheel))
        .max(chunks_needed(*pan))
        .max(1);
    (
        take_even_chunk(x, chunks),
        take_even_chunk(y, chunks),
        take_even_chunk(wheel, chunks),
        take_even_chunk(pan, chunks),
    )
}

#[cfg(test)]
mod tests {
    use super::take_vector_chunk;

    fn drain(mut x: i32, mut y: i32) -> (Vec<(i16, i16)>, i32, i32) {
        let source_x = x;
        let source_y = y;
        let mut chunks = Vec::new();
        while x != 0 || y != 0 {
            let mut wheel = 0;
            let mut pan = 0;
            let (chunk_x, chunk_y, _, _) = take_vector_chunk(&mut x, &mut y, &mut wheel, &mut pan);
            chunks.push((chunk_x, chunk_y));
        }
        let total_x = chunks.iter().map(|(x, _)| i32::from(*x)).sum();
        let total_y = chunks.iter().map(|(_, y)| i32::from(*y)).sum();
        assert_eq!((total_x, total_y), (source_x, source_y));
        (chunks, x, y)
    }

    #[test]
    fn asymmetric_vector_keeps_the_same_direction() {
        let (chunks, x, y) = drain(-65_000, 2_000);
        assert_eq!(chunks, vec![(-32_500, 1_000), (-32_500, 1_000)]);
        assert_eq!((x, y), (0, 0));
    }

    #[test]
    fn all_quadrants_are_distributed_without_loss() {
        for (x, y) in [
            (70_000, 21_000),
            (70_000, -21_000),
            (-70_000, 21_000),
            (-70_000, -21_000),
        ] {
            let (chunks, residual_x, residual_y) = drain(x, y);
            assert_eq!(chunks.len(), 3);
            assert_eq!((residual_x, residual_y), (0, 0));
        }
    }

    #[test]
    fn signed_hid_limits_fit_in_one_report() {
        let (chunks, _, _) = drain(i32::from(i16::MAX), i32::from(i16::MIN));
        assert_eq!(chunks, vec![(i16::MAX, i16::MIN)]);
    }

    #[test]
    fn large_values_terminate_and_preserve_exact_sum() {
        let (chunks, _, _) = drain(2_000_000, -2_000_000);
        assert_eq!(chunks.len(), 62);
    }
}

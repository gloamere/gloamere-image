//! Conservative admission estimates, not a promise about allocator/driver RSS.
mod owned;
pub use owned::*;
pub const OPERATION_BUDGET: u64 = 768 * 1024 * 1024;
pub const OVERHEAD: u64 = 32 * 1024 * 1024;
pub fn rgba_bytes(width: u32, height: u32) -> u64 {
    u64::from(width) * u64::from(height) * 4
}
pub fn decode_estimate(input: u64, decoded: u64, width: u32, height: u32) -> u64 {
    input.saturating_mul(2)
        + crate::pipeline::MAX_DECODE_BYTES
        + decoded.saturating_mul(2)
        + rgba_bytes(width, height) * 3
        + OVERHEAD
}
pub fn resize_extra(input_width: u32, input_height: u32, width: u32, height: u32) -> u64 {
    // image 0.25.10 vertical_sample: input_width * output_height * RGBA f32.
    rgba_bytes(input_width, input_height)
        + u64::from(input_width) * u64::from(height) * 16
        + rgba_bytes(width, height)
        + OVERHEAD
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extreme_aspect_resize_is_rejected_before_allocation() {
        assert!(resize_extra(32768, 1, 1, 32768) > OPERATION_BUDGET);
        assert!(reserve(OPERATION_BUDGET + 1).is_err());
    }
    #[test]
    fn maximum_normal_image_decode_is_bounded_by_estimate() {
        assert!(
            decode_estimate(64 * 1024 * 1024, 128 * 1024 * 1024, 4000, 4000) < OPERATION_BUDGET
        );
    }
}

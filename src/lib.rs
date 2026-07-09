#![doc = include_str!("../README.md")]

/// Adds two numbers together.
///
/// This is a placeholder function to demonstrate the crate layout. Replace it
/// with the crate's real public API.
///
/// # Examples
///
/// ```
/// assert_eq!(rust_crate_template::add(2, 2), 4);
/// ```
#[must_use]
pub fn add(left: u64, right: u64) -> u64 {
    left + right
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        assert_eq!(add(2, 2), 4);
    }
}

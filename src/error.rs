//! The error type returned by the slicer.
//!
//! This crate never panics on caller input: every operation that cannot proceed returns one of
//! these variants instead. All coordinate math is checked, so out-of-range inputs are reported
//! rather than overflowing or producing a wrong result.

use thiserror::Error;

/// Something the slicer cannot process. Returned in place of a panic or a silently-wrong result.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum SliceError {
    /// The per-tile output resolution / tile side (`extent`) was zero, or larger than `i32::MAX`.
    #[error("extent must be between 1 and {}", i32::MAX)]
    InvalidExtent,

    /// The `buffer` was too large for the tile size: it must be **strictly less than half** the
    /// `extent`, so geometry near a tile edge spills into at most one neighbouring tile per axis.
    #[error("buffer must be strictly less than half the extent")]
    BufferTooLarge,

    /// The geometry was not a `LineString` or `MultiLineString` (the only kinds the slicer clips).
    #[error("expected a LineString or MultiLineString, got a {0}")]
    UnsupportedGeometry(&'static str),

    /// A line has more than `u16::MAX` vertices, or the geometry has more than `u16::MAX` lines —
    /// beyond what the slicer's compact indexing supports.
    #[error(
        "polyline too large: at most {0} lines, and {0} vertices per line, are supported",
        u16::MAX
    )]
    PolylineTooLarge,

    /// The geometry reaches too many tiles to slice: it spans more than `i16::MAX` tiles from its
    /// first vertex on an axis, or its segments would collectively require examining more candidate
    /// tiles than the slicer's working-set bound.
    #[error("polyline reaches too many tiles to slice")]
    TooManyTiles,

    /// Coordinate arithmetic overflowed `i32` — a coordinate or tile lies too close to the limits of
    /// the representable range for the given `extent`/`buffer`.
    #[error("coordinate arithmetic overflowed the i32 range")]
    Overflow,
}

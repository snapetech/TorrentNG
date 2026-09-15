pub mod error;
pub mod map;
pub mod v2;

pub use error::PieceMapError;
pub use map::{FileRegion, FileSpan, PieceMap, PieceRegion};
pub use v2::{V2FileSpan, V2PieceMap, V2PieceRegion};

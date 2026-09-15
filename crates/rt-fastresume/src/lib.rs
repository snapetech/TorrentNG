pub mod error;
pub mod state;
pub mod store;

pub use error::FastresumeError;
pub use state::{
    DurabilityWatermark, FastresumeState, FileHint, ImportPolicy, PartialPieceState, PieceState,
    MAX_FASTRESUME_BLOCKS_PER_PARTIAL_PIECE, MAX_FASTRESUME_DIRTY_PIECES,
    MAX_FASTRESUME_FILE_HINTS, MAX_FASTRESUME_PARTIAL_PIECES, MAX_FASTRESUME_PIECES,
};
pub use store::{FastresumeStore, MAX_FASTRESUME_BYTES};

//! 精确 timing 协议（schema_version=2）。

pub mod qpc_utc;
pub mod sha256;
pub mod sidecar;
pub mod sync_report;

#[allow(unused_imports)]
pub use qpc_utc::{QpcUtcCalibration, QpcUtcMapper};
#[allow(unused_imports)]
pub use sha256::sha256_hex;
pub use sidecar::{
    write_wav_and_sidecar_atomic, Discontinuity, TimeSyncSidecarV2, TimingAnchor, TimingSidecarV2,
};
pub use sync_report::{load_and_validate_pre_sync, ValidatedTimeSyncReport};
